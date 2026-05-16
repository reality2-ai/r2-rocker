//! SD ring-segment writer (SPEC-R2-ROCKER-SENSOR §6.2–6.5).
//!
//! On boot, scan `<mount>/r2/` for `log.NNNN.bin` segments, identify the
//! highest-numbered, read its last 20-byte record to recover `tail_seq`,
//! and continue writing into that segment. Rotate to a new segment when
//! the current one hits `segment_size_bytes` (8 MiB default). Delete the
//! oldest segment when the count would exceed `ring_segments` (12
//! default → 96 MiB ≈ 14 hours at 100 Hz).
//!
//! Record format (§6.2), 20 bytes fixed:
//!   offset 0..3   seq    u32 LE
//!   offset 4..7   ts_ms  u32 LE   (already-synchronised per TIMESYNC §2.2)
//!   offset 8..11  x      i32 LE
//!   offset 12..15 y      i32 LE
//!   offset 16..19 z      i32 LE
//!
//! v0.1 limitations (to track in follow-ups):
//!   * `segment_size_mb` / `ring_segments` are hardcoded; spec calls
//!     for them to be NVS-tunable.
//!   * No `meta.bin` snapshot (§6.4); the spec's secondary durability
//!     path is deferred.
//!   * Failure handling on write error is "log and continue" — the
//!     spec calls for a small in-RAM retry queue + ERROR escalation
//!     after 30 s. Operator-supervised rig tolerates this gap for now.

use anyhow::{Context, Result};
use log::{info, warn};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Bytes per record per spec §6.2.
const RECORD_BYTES: u64 = 20;
/// Default segment size in bytes (8 MiB ≈ 70 minutes at 100 Hz / 20-byte records).
const SEGMENT_SIZE_BYTES: u64 = 8 * 1024 * 1024;
/// Ring depth — oldest segment deleted when count would exceed this.
const RING_SEGMENTS: usize = 12;
/// Subdirectory under the SD mount point. Spec §6.1 places segments
/// under `/r2/` but ESP-IDF's FATFS layer has been unreliable about
/// creating subdirectories from `std::fs::create_dir_all` (returns Ok
/// without actually creating the directory, then subsequent file opens
/// fail with EINVAL). For v0.1 we put segments at the mount root —
/// cosmetic spec deviation, no functional impact. Will revisit once
/// either ESP-IDF or our usage of it can be trusted to create the
/// subdir reliably.
const RING_DIR: &str = "";

/// Open ring + writable handle to the current segment. `Drop` flushes
/// nothing extra — the kernel-side cache and FAT driver handle that.
/// Call `sync()` periodically (or before sleep) if you want a hard
/// barrier; v0.1 doesn't.
pub struct Ring {
    base: PathBuf,
    current: File,
    current_num: u32,
    current_bytes: u64,
    tail_seq: u32,
}

impl Ring {
    /// Open the ring at `<mount_point>/r2/`. Creates the directory if
    /// missing. Performs boot recovery per §6.5.
    pub fn open(mount_point: &str) -> Result<Self> {
        let base = PathBuf::from(mount_point).join(RING_DIR);
        fs::create_dir_all(&base)
            .with_context(|| format!("create ring dir {:?}", base))?;

        let mut segments = enumerate_segments(&base)?;
        segments.sort();

        let (current_num, tail_seq) = match segments.last() {
            Some(&highest) => {
                let path = base.join(segment_name(highest));
                let file_bytes = fs::metadata(&path)?.len();
                let n_records = file_bytes / RECORD_BYTES;
                if n_records == 0 {
                    (highest, 0u32)
                } else {
                    // Read the seq field of the last record (first 4 bytes).
                    let mut f = File::open(&path)
                        .with_context(|| format!("open {:?} for tail-seq scan", path))?;
                    f.seek(SeekFrom::Start((n_records - 1) * RECORD_BYTES))?;
                    let mut buf = [0u8; 4];
                    f.read_exact(&mut buf)?;
                    (highest, u32::from_le_bytes(buf))
                }
            }
            None => (1, 0),
        };

        // Open the current segment for writing. ESP-IDF's FATFS layer
        // is picky about flag combinations:
        //   * `OpenOptions::append(true)`           → EINVAL
        //   * `create(true) + write(true)`  (no truncate) → EINVAL
        //   * `File::create` (O_CREAT|O_TRUNC|O_WRONLY)   → ok
        //   * `OpenOptions::write(true)`  (no create)     → ok
        // So branch on existence: `File::create` for fresh segments,
        // plain `write(true)` open for existing ones — followed by
        // explicit seek-to-end so we don't overwrite resumed data.
        let path = base.join(segment_name(current_num));
        let path_exists = fs::metadata(&path).is_ok();
        let mut current = if path_exists {
            OpenOptions::new()
                .write(true)
                .open(&path)
                .with_context(|| format!("open existing {:?} for write", path))?
        } else {
            File::create(&path)
                .with_context(|| format!("create new {:?}", path))?
        };
        let current_bytes = if path_exists {
            fs::metadata(&path)?.len()
        } else {
            0
        };
        if current_bytes > 0 {
            current.seek(SeekFrom::End(0))
                .with_context(|| format!("seek end on {:?}", path))?;
        }

        info!(
            "[ring] opened {:?} — seg {} ({} bytes, ~{} records), \
             tail_seq={}, {} total segments",
            base,
            current_num,
            current_bytes,
            current_bytes / RECORD_BYTES,
            tail_seq,
            segments.len()
        );

        Ok(Self {
            base,
            current,
            current_num,
            current_bytes,
            tail_seq,
        })
    }

    /// Highest `seq` written to disk (across boots). Sender uses this on
    /// boot to seed its in-RAM counter to `tail_seq + 1` per §6.5 step 3.
    pub fn tail_seq(&self) -> u32 {
        self.tail_seq
    }

    /// Append a single record. Rotates the segment if the next write
    /// would exceed the segment-size threshold. Errors propagate to the
    /// caller; the sender's policy is "log and continue" for now (§6.7).
    #[allow(dead_code)]
    pub fn append(
        &mut self,
        seq: u32,
        ts_ms: u32,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<()> {
        if self.current_bytes.saturating_add(RECORD_BYTES) > SEGMENT_SIZE_BYTES {
            self.rotate()?;
        }
        let mut rec = [0u8; 20];
        rec[0..4].copy_from_slice(&seq.to_le_bytes());
        rec[4..8].copy_from_slice(&ts_ms.to_le_bytes());
        rec[8..12].copy_from_slice(&x.to_le_bytes());
        rec[12..16].copy_from_slice(&y.to_le_bytes());
        rec[16..20].copy_from_slice(&z.to_le_bytes());
        self.current.write_all(&rec).context("ring append write")?;
        self.current_bytes += RECORD_BYTES;
        self.tail_seq = seq;
        Ok(())
    }

    /// Free SD segments whose records are all `seq ≤ through_seq` per
    /// SPEC-R2-ROCKER-SENSOR §7.4. The current write-target segment is
    /// never deleted (it's open for append; closing/deleting would
    /// crash the writer). Best-effort: a single failed unlink is
    /// logged and the loop stops; remaining freeable segments will be
    /// picked up on the next ack.
    #[allow(dead_code)]
    pub fn free_through(&mut self, through_seq: u32) -> Result<()> {
        let mut segments = enumerate_segments(&self.base)?;
        segments.sort();
        for num in segments {
            if num == self.current_num {
                // Never delete the segment we're writing into.
                break;
            }
            let path = self.base.join(segment_name(num));
            let file_bytes = match fs::metadata(&path) {
                Ok(m) => m.len(),
                Err(e) => {
                    warn!("[ring] stat {:?} failed: {} — skipping", path, e);
                    continue;
                }
            };
            let n_records = file_bytes / RECORD_BYTES;
            if n_records == 0 {
                // Empty segment — fine to remove.
                let _ = fs::remove_file(&path);
                continue;
            }
            // Read the seq field of the LAST record. If that <= through_seq,
            // every record in the segment has been acked.
            let last_seq = match read_last_seq(&path, n_records) {
                Ok(s) => s,
                Err(e) => {
                    warn!("[ring] read last seq from {:?} failed: {}", path, e);
                    continue;
                }
            };
            if last_seq > through_seq {
                // First segment with un-acked records — everything
                // higher-numbered must also be un-acked. Stop.
                break;
            }
            match fs::remove_file(&path) {
                Ok(_) => info!(
                    "[ring] freed segment {} (last_seq={} ≤ through_seq={})",
                    num, last_seq, through_seq
                ),
                Err(e) => {
                    warn!("[ring] remove {:?} failed: {} — bailing", path, e);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Close the current segment, open the next one, and delete the
    /// oldest if doing so would exceed `RING_SEGMENTS`. Called from
    /// `append` when the size threshold is hit.
    fn rotate(&mut self) -> Result<()> {
        // Best-effort fsync on the segment we're leaving. Even if the
        // kernel hasn't flushed, the FAT driver should commit on close.
        let _ = self.current.sync_all();

        let next_num = self.current_num.checked_add(1).unwrap_or(1);
        let path = self.base.join(segment_name(next_num));
        // Fresh segment — File::create (CREATE | WRITE | TRUNCATE) avoids
        // the EINVAL-on-append issue in ESP-IDF's FATFS. Truncation is
        // safe because segment numbers strictly increase, so this file
        // doesn't exist yet under normal operation.
        let new_current = File::create(&path)
            .with_context(|| format!("create {:?} for rotated segment", path))?;
        self.current = new_current;
        self.current_num = next_num;
        self.current_bytes = 0;
        info!("[ring] rotated to segment {}", next_num);

        // Enforce ring depth. Always keep the segment we just opened —
        // delete from the oldest end of the list.
        let mut segments = enumerate_segments(&self.base)?;
        segments.sort();
        while segments.len() > RING_SEGMENTS {
            let oldest = segments.remove(0);
            // Never delete the segment we're writing into.
            if oldest == self.current_num {
                break;
            }
            let path = self.base.join(segment_name(oldest));
            match fs::remove_file(&path) {
                Ok(_) => info!("[ring] removed oldest segment {}", oldest),
                Err(e) => {
                    warn!("[ring] remove {:?} failed: {} — continuing", path, e);
                    break;
                }
            }
        }
        Ok(())
    }
}

fn read_last_seq(path: &std::path::Path, n_records: u64) -> Result<u32> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start((n_records - 1) * RECORD_BYTES))?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn enumerate_segments(base: &PathBuf) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(base).with_context(|| format!("readdir {:?}", base))? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if let Some(num) = parse_segment_name(name) {
                out.push(num);
            }
        }
    }
    Ok(out)
}

// Spec §6.1 calls for `log.NNNN.bin` but ESP-IDF's FATFS LFN handling
// rejects multi-dot filenames; we use `logNNNN.bin` (strict 8.3) on
// disk. Naming-only deviation; the wire / record formats and the boot-
// recovery logic don't change.
fn segment_name(num: u32) -> String {
    format!("log{:04}.bin", num)
}

fn parse_segment_name(name: &str) -> Option<u32> {
    name.strip_prefix("log")
        .and_then(|s| s.strip_suffix(".bin"))
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segment_name() {
        assert_eq!(parse_segment_name("log0001.bin"), Some(1));
        assert_eq!(parse_segment_name("log0042.bin"), Some(42));
        assert_eq!(parse_segment_name("log9999.bin"), Some(9999));
        assert_eq!(parse_segment_name("log0001.txt"), None);
        assert_eq!(parse_segment_name("data.0001.bin"), None);
        assert_eq!(parse_segment_name(""), None);
    }

    #[test]
    fn formats_segment_name() {
        assert_eq!(segment_name(1), "log0001.bin");
        assert_eq!(segment_name(42), "log0042.bin");
        assert_eq!(segment_name(9999), "log9999.bin");
    }
}
