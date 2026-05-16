//! SD ring-segment writer (SPEC-R2-ROCKER-SENSOR §6.2–6.5).
//!
//! On boot, scan the SD mount root for `logNNNN.csv` segments, identify
//! the highest-numbered, read the first 10 bytes of its last record
//! (the `seq` column) to recover `tail_seq`, and continue writing into
//! that segment. Rotate to a new segment when the current one hits
//! `segment_size_bytes` (8 MiB default). Delete the oldest segment when
//! the count would exceed `ring_segments` (12 default → 96 MiB ≈ 7 h at
//! 100 Hz given the CSV record size).
//!
//! Record format (§6.2 v0.2), fixed-width CSV — 62 bytes per row:
//!
//! ```text
//!     seq         ts_ms       x          y          z   \n
//!   "        0,     0,         0,         0,         0\n"
//!   "       10,  1234,       -42,        17,        -3\n"
//!   ^         ^             ^           ^           ^
//!   |---10----|------14-----|-----11----|-----11----|-----11----| + 4 commas + 1 LF = 62
//! ```
//!
//! Columns are right-aligned and space-padded so every record is
//! exactly 62 bytes. This keeps `seek(record_index * 62)` valid for
//! boot recovery while remaining trivially parseable by pandas /
//! Excel / `awk` (any CSV reader with `skipinitialspace=True` or
//! whitespace-tolerant numeric coercion).
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

/// Bytes per record per spec §6.2 (v0.2 CSV layout). `RECORD_FMT` and
/// this constant must agree exactly; mismatch breaks boot recovery's
/// seek-arithmetic.
const RECORD_BYTES: u64 = 62;
/// Column widths for the right-aligned CSV layout. The `seq` column
/// width is also used as the parse window during boot recovery.
const W_SEQ:   usize = 10;
const W_TS_MS: usize = 14;
const W_AXIS:  usize = 11;
/// Default segment size in bytes (8 MiB ≈ 35 minutes at 100 Hz with
/// 62-byte CSV records).
const SEGMENT_SIZE_BYTES: u64 = 8 * 1024 * 1024;
/// Ring depth — oldest segment deleted when count would exceed this.
const RING_SEGMENTS: usize = 12;
/// fsync cadence. The capturing logger writes to FATFS's in-RAM cache;
/// without `sync_all()` the data and FAT directory entries never reach
/// the card and pulling the SD card mid-run yields an empty file.
/// 100 records ≈ 1 s at 100 Hz — a reasonable bound on worst-case data
/// loss on hot SD-pull. `rotate()` and `Drop` also sync.
const SYNC_EVERY_N_RECORDS: u32 = 100;
/// Subdirectory under the SD mount point. Spec §6.1 places segments
/// under `/r2/` but ESP-IDF's FATFS layer has been unreliable about
/// creating subdirectories from `std::fs::create_dir_all` (returns Ok
/// without actually creating the directory, then subsequent file opens
/// fail with EINVAL). For v0.1 we put segments at the mount root —
/// cosmetic spec deviation, no functional impact. Will revisit once
/// either ESP-IDF or our usage of it can be trusted to create the
/// subdir reliably.
const RING_DIR: &str = "";

/// Open ring + writable handle to the current segment. Periodic
/// `sync_all()` is called every `SYNC_EVERY_N_RECORDS` appends and on
/// rotate to keep FATFS's RAM cache from swallowing data on hot SD
/// removal.
pub struct Ring {
    base: PathBuf,
    current: File,
    current_num: u32,
    current_bytes: u64,
    tail_seq: u32,
    records_since_sync: u32,
}

impl Ring {
    /// Open the ring at the SD mount point. Performs boot recovery per §6.5.
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
                    // The seq column is the first W_SEQ bytes of the
                    // last record. Right-aligned ASCII decimal; trim
                    // leading spaces and parse.
                    let mut f = File::open(&path)
                        .with_context(|| format!("open {:?} for tail-seq scan", path))?;
                    f.seek(SeekFrom::Start((n_records - 1) * RECORD_BYTES))?;
                    let mut buf = [0u8; W_SEQ];
                    f.read_exact(&mut buf)?;
                    let seq = parse_seq_field(&buf).with_context(|| {
                        format!("parse seq from {:?} (last record)", path)
                    })?;
                    (highest, seq)
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
            records_since_sync: 0,
        })
    }

    /// Highest `seq` written to disk (across boots). Sender uses this on
    /// boot to seed its in-RAM counter to `tail_seq + 1` per §6.5 step 3.
    pub fn tail_seq(&self) -> u32 {
        self.tail_seq
    }

    /// Append a single record. Rotates the segment if the next write
    /// would exceed the segment-size threshold. Calls `sync_all()` every
    /// `SYNC_EVERY_N_RECORDS` writes so a hot SD-pull doesn't lose more
    /// than ~1 s of samples. Errors propagate to the caller; the
    /// sender's policy is "log and continue" for now (§6.7).
    #[allow(dead_code)]
    pub fn append(
        &mut self,
        seq: u32,
        ts_ms: i64,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<()> {
        if self.current_bytes.saturating_add(RECORD_BYTES) > SEGMENT_SIZE_BYTES {
            self.rotate()?;
        }

        // Format into a stack buffer first so we can assert the layout
        // stayed 62 bytes wide and avoid silent corruption from a value
        // exceeding its column width. Width here is the *minimum*: the
        // formatter does not truncate, so an overflow shows up as a
        // wider line and a debug_assert.
        let mut buf = [0u8; RECORD_BYTES as usize];
        let n = {
            let mut cur = &mut buf[..];
            write!(
                cur,
                "{:>w_seq$},{:>w_ts$},{:>w_a$},{:>w_a$},{:>w_a$}\n",
                seq, ts_ms, x, y, z,
                w_seq = W_SEQ,
                w_ts = W_TS_MS,
                w_a = W_AXIS,
            ).context("ring append format")?;
            (RECORD_BYTES as usize) - cur.len()
        };
        debug_assert_eq!(
            n, RECORD_BYTES as usize,
            "ring CSV row width drift: seq={} ts_ms={} x={} y={} z={}",
            seq, ts_ms, x, y, z,
        );

        self.current.write_all(&buf[..n]).context("ring append write")?;
        self.current_bytes += n as u64;
        self.tail_seq = seq;

        self.records_since_sync = self.records_since_sync.saturating_add(1);
        if self.records_since_sync >= SYNC_EVERY_N_RECORDS {
            // Best-effort: a failed fsync gets a warn and we keep going.
            // The next rotation or boot still has the data on disk if
            // the failure was transient; if it isn't, the SD card is
            // failing and §6.7 escalation applies.
            if let Err(e) = self.current.sync_all() {
                warn!("[ring] periodic sync_all failed: {} — continuing", e);
            }
            self.records_since_sync = 0;
        }
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
            // Read the seq column of the LAST record. If that <= through_seq,
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
        self.records_since_sync = 0;
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

impl Drop for Ring {
    fn drop(&mut self) {
        // Last-chance flush on shutdown. esp_restart() will drop us;
        // without this, anything in the FATFS RAM cache since the last
        // periodic sync is lost.
        if let Err(e) = self.current.sync_all() {
            warn!("[ring] drop-time sync_all failed: {}", e);
        }
    }
}

fn read_last_seq(path: &std::path::Path, n_records: u64) -> Result<u32> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start((n_records - 1) * RECORD_BYTES))?;
    let mut buf = [0u8; W_SEQ];
    f.read_exact(&mut buf)?;
    parse_seq_field(&buf)
}

/// Trim ASCII spaces and parse a u32 from the first column of a record.
fn parse_seq_field(buf: &[u8]) -> Result<u32> {
    let s = std::str::from_utf8(buf).context("seq field not UTF-8")?;
    let trimmed = s.trim();
    trimmed
        .parse::<u32>()
        .with_context(|| format!("seq field {:?} not a u32", trimmed))
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

// v0.2: `.csv` extension so the file format is obvious on inspection.
// Strict 8.3 (single dot) keeps ESP-IDF's FATFS happy. The old `.bin`
// segments from v0.1 are not scanned — any leftover on a card from a
// prior build is ignored (and operator can `rm` if desired).
fn segment_name(num: u32) -> String {
    format!("log{:04}.csv", num)
}

fn parse_segment_name(name: &str) -> Option<u32> {
    name.strip_prefix("log")
        .and_then(|s| s.strip_suffix(".csv"))
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segment_name() {
        assert_eq!(parse_segment_name("log0001.csv"), Some(1));
        assert_eq!(parse_segment_name("log0042.csv"), Some(42));
        assert_eq!(parse_segment_name("log9999.csv"), Some(9999));
        assert_eq!(parse_segment_name("log0001.txt"), None);
        assert_eq!(parse_segment_name("log0001.bin"), None);
        assert_eq!(parse_segment_name("data.0001.csv"), None);
        assert_eq!(parse_segment_name(""), None);
    }

    #[test]
    fn formats_segment_name() {
        assert_eq!(segment_name(1), "log0001.csv");
        assert_eq!(segment_name(42), "log0042.csv");
        assert_eq!(segment_name(9999), "log9999.csv");
    }

    #[test]
    fn record_width_constant_matches_format() {
        let mut buf = [0u8; 256];
        let n = {
            let mut cur = &mut buf[..];
            write!(
                cur,
                "{:>w_seq$},{:>w_ts$},{:>w_a$},{:>w_a$},{:>w_a$}\n",
                0u32, 0i64, 0i32, 0i32, 0i32,
                w_seq = W_SEQ,
                w_ts = W_TS_MS,
                w_a = W_AXIS,
            ).unwrap();
            buf.len() - cur.len()
        };
        assert_eq!(n as u64, RECORD_BYTES);
    }

    #[test]
    fn parses_seq_field_right_aligned() {
        assert_eq!(parse_seq_field(b"         0").unwrap(), 0);
        assert_eq!(parse_seq_field(b"       100").unwrap(), 100);
        assert_eq!(parse_seq_field(b"4294967295").unwrap(), u32::MAX);
    }
}
