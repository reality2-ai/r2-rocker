//! Named-capture state machine (SPEC-R2-ROCKER-CAPTURE).
//!
//! Sits alongside the rolling ring. The sender thread owns one
//! `CaptureMgr` and calls `observe()` on every sample after the
//! ring's own `append()`. The capture path is deliberately a no-op
//! while `state == Idle` (the common case) — there's no extra
//! per-sample work when no run is being recorded.

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use r2_esp::data_tcp::CurrentRecording;

/// Calibration window in milliseconds (SPEC-R2-ROCKER-CAPTURE §2).
const CAL_WINDOW_MS: i64 = 2000;

/// Sub-directory of the SD mount point where capture files live
/// (SPEC §4). When `create_dir_all` fails on the ESP-IDF FATFS layer
/// the firmware falls back to placing files at the mount root with
/// a `cap-` filename prefix — see `open_capture_file`.
const CAPTURES_SUBDIR: &str = "captures";

/// Run-name length cap per SPEC §3.
const NAME_MAX: usize = 32;

/// Column widths for the calibrated CSV row — match the ring
/// (SPEC-R2-ROCKER-SENSOR §6.2) so a single column-width policy
/// covers both file types.
const W_SEQ: usize = 10;
const W_TS_MS: usize = 14;
const W_AXIS: usize = 11;
const ROW_BYTES: u64 = (W_SEQ + 1 + W_TS_MS + 1 + W_AXIS + 1 + W_AXIS + 1 + W_AXIS + 1) as u64;

/// fsync cadence inside Recording. Same justification as the ring:
/// FATFS buffers writes in RAM and a hot SD-pull loses anything not
/// fsynced. 100 records ≈ 1 s at 100 Hz.
const SYNC_EVERY_N_RECORDS: u32 = 100;

/// Wire-encoded state byte for `r2.sensor.capture.state` (SPEC §3).
pub const STATE_IDLE: u8        = 0;
pub const STATE_CALIBRATING: u8 = 1;
pub const STATE_RECORDING: u8   = 2;

pub enum CaptureState {
    Idle,
    Calibrating {
        /// Sensor-side wall-clock at Start. Used to clip the cal
        /// window — samples arriving after `start_ms + CAL_WINDOW_MS`
        /// are dropped from the accumulator (the mean is locked at
        /// that point but not yet committed; only Mark commits it).
        start_ms: i64,
        sum_x: i64,
        sum_y: i64,
        sum_z: i64,
        n: u32,
        /// The mean is cached here once the window closes so an early
        /// Mark (before window end) and a late Mark (after window end)
        /// both behave identically. `None` until the window closes
        /// or Mark arrives.
        locked_offset: Option<(i32, i32, i32)>,
    },
    Recording {
        /// Open file handle when SD is available; `None` when the
        /// operator marked an SD-less sensor (still drives the wire-
        /// path calibration so the Live chart flatlines, but nothing
        /// hits the disk).
        file: Option<File>,
        /// Locked-in offset; subtracted from every sample's axis
        /// value before the row is written.
        offset: (i32, i32, i32),
        /// Filename without leading directory — held so the
        /// `capture.state` event can echo it back to the dashboard.
        /// `None` mirrors `file == None` (no file was opened).
        file_name: Option<String>,
        records_since_sync: u32,
    },
}

pub struct CaptureMgr {
    state: CaptureState,
    mount_point: PathBuf,
    /// Shared handle the `data_tcp` listener consults to refuse
    /// GET / DEL against the file we're actively writing. Updated
    /// here on every transition: `Some(<filename>)` while Recording,
    /// `None` otherwise.
    current_recording: CurrentRecording,
}

impl CaptureMgr {
    pub fn new(mount_point: &str, current_recording: CurrentRecording) -> Self {
        Self {
            state: CaptureState::Idle,
            mount_point: PathBuf::from(mount_point),
            current_recording,
        }
    }

    fn set_current(&self, name: Option<&str>) {
        if let Ok(mut g) = self.current_recording.lock() {
            *g = name.map(|s| s.to_string());
        }
    }

    /// Wire-encoded current state.
    pub fn state_byte(&self) -> u8 {
        match self.state {
            CaptureState::Idle => STATE_IDLE,
            CaptureState::Calibrating { .. } => STATE_CALIBRATING,
            CaptureState::Recording { .. } => STATE_RECORDING,
        }
    }

    /// True while a calibration window is open. Used by the sender
    /// to drive the LED to `Calibrating` and back.
    pub fn is_calibrating(&self) -> bool {
        matches!(self.state, CaptureState::Calibrating { .. })
    }

    /// True while a capture file is open for writing. Used by the
    /// `data_tcp` GET / DEL paths to reject operations on the
    /// currently-recording file.
    pub fn is_recording(&self) -> bool {
        matches!(self.state, CaptureState::Recording { .. })
    }

    /// The locked-in (x, y, z) offset while Recording. `None` in any
    /// other state. Sender consults this to apply the same offset
    /// to the wire-emitted `r2.sensor.acceleration` payload so the
    /// dashboard's Live chart shows the calibrated values that are
    /// landing on the SD card.
    pub fn current_offset(&self) -> Option<(i32, i32, i32)> {
        match self.state {
            CaptureState::Recording { offset, .. } => Some(offset),
            _ => None,
        }
    }

    /// Filename of the currently-open capture (without directory),
    /// or `None` when not in `Recording` OR when Recording but no
    /// SD was available to open a file. Used to echo in the
    /// `r2.sensor.capture.state` event.
    pub fn open_file_name(&self) -> Option<&str> {
        match &self.state {
            CaptureState::Recording { file_name: Some(s), .. } => Some(s.as_str()),
            _ => None,
        }
    }

    /// Enter Calibrating. Any prior file is closed (effectively a
    /// Stop+Start in one event, per SPEC §2 transitions table).
    pub fn start(&mut self, ts_ms: i64) {
        // If we were Recording with a file, close it first.
        if let CaptureState::Recording { file: Some(ref mut f), .. } = self.state {
            let _ = f.sync_all();
        }
        self.state = CaptureState::Calibrating {
            start_ms: ts_ms,
            sum_x: 0, sum_y: 0, sum_z: 0,
            n: 0,
            locked_offset: None,
        };
        self.set_current(None);
        info!("[capture] start — entering Calibrating window ({} ms)", CAL_WINDOW_MS);
    }

    /// Lock the calibration mean, validate `name`, open the capture
    /// file. Filename built from the dashboard-supplied `ts_ms` so
    /// every sensor in the fleet writes the same name.
    pub fn mark(&mut self, ts_ms: i64, name: &str) -> Result<()> {
        if !is_valid_name(name) {
            return Err(anyhow!("invalid capture name {:?}", name));
        }

        // If we're not Calibrating, refuse — the operator has to
        // Start first per the state diagram.
        let offset = match self.state {
            CaptureState::Calibrating { sum_x, sum_y, sum_z, n, locked_offset, .. } => {
                if let Some(off) = locked_offset { off }
                else if n > 0 {
                    (
                        (sum_x / n as i64) as i32,
                        (sum_y / n as i64) as i32,
                        (sum_z / n as i64) as i32,
                    )
                } else {
                    // Cal window had no samples yet — use zero offset
                    // rather than refuse the Mark. Better calibration
                    // than nothing, and matches the operator's intent
                    // (they pressed Mark; record).
                    warn!("[capture] mark before any cal samples — using zero offset");
                    (0, 0, 0)
                }
            }
            CaptureState::Recording { .. } => {
                return Err(anyhow!("mark while already recording — call stop first"));
            }
            CaptureState::Idle => {
                return Err(anyhow!("mark while idle — call start first"));
            }
        };

        let file_name = format!("{:016}-{}.csv", ts_ms.max(0), name);
        // Try to open the file — but a failure here (no SD, FS
        // refused) is no longer fatal: we still transition to
        // Recording so the wire-path calibration kicks in. The
        // operator sees the Live chart flatline; nothing lands on
        // disk for the SD-less sensor.
        let (file_opt, name_opt) = match self.open_capture_file(&file_name) {
            Ok(f) => {
                info!("[capture] mark — offset={:?} file={}", offset, file_name);
                self.set_current(Some(&file_name));
                (Some(f), Some(file_name))
            }
            Err(e) => {
                warn!("[capture] mark — offset locked but no file: {} (offset still applied to wire)", e);
                self.set_current(None);
                (None, None)
            }
        };
        self.state = CaptureState::Recording {
            file: file_opt,
            offset,
            file_name: name_opt,
            records_since_sync: 0,
        };
        Ok(())
    }

    /// Close the open capture file (fsync). Idempotent — calling Stop
    /// in Idle or Calibrating is a no-op so the dashboard can fan
    /// it out without per-state branching.
    pub fn stop(&mut self) -> Result<()> {
        let prev = std::mem::replace(&mut self.state, CaptureState::Idle);
        self.set_current(None);
        if let CaptureState::Recording { file, file_name, .. } = prev {
            if let Some(mut f) = file {
                let label = file_name.as_deref().unwrap_or("<unnamed>");
                f.sync_all().with_context(|| format!("sync capture {}", label))?;
                info!("[capture] stop — closed {}", label);
            } else {
                info!("[capture] stop — no file was open (SD-less capture)");
            }
        }
        Ok(())
    }

    /// Per-sample hook. Returns Ok in all states. Behaviour:
    /// - Idle: no-op.
    /// - Calibrating: accumulate raw axis values; auto-close the
    ///   accumulator at `start_ms + CAL_WINDOW_MS`.
    /// - Recording: write one calibrated CSV row, fsync periodically.
    pub fn observe(&mut self, seq: u32, ts_ms: i64, x: i32, y: i32, z: i32) -> Result<()> {
        match &mut self.state {
            CaptureState::Idle => Ok(()),
            CaptureState::Calibrating {
                start_ms, sum_x, sum_y, sum_z, n, locked_offset,
            } => {
                if locked_offset.is_none() {
                    if ts_ms < *start_ms + CAL_WINDOW_MS {
                        *sum_x = sum_x.saturating_add(x as i64);
                        *sum_y = sum_y.saturating_add(y as i64);
                        *sum_z = sum_z.saturating_add(z as i64);
                        *n = n.saturating_add(1);
                    } else if *n > 0 {
                        *locked_offset = Some((
                            (*sum_x / *n as i64) as i32,
                            (*sum_y / *n as i64) as i32,
                            (*sum_z / *n as i64) as i32,
                        ));
                        info!(
                            "[capture] cal window closed — n={} offset=({}, {}, {})",
                            n,
                            (*sum_x / *n as i64) as i32,
                            (*sum_y / *n as i64) as i32,
                            (*sum_z / *n as i64) as i32,
                        );
                    }
                }
                Ok(())
            }
            CaptureState::Recording { file, offset, records_since_sync, .. } => {
                // The wire-path calibration in sender.rs reads the
                // same `offset` via `current_offset()`. This arm only
                // runs the file write — when no file was opened
                // (SD-less mark) we skip it but keep the offset
                // locked so the Live chart still zeroes.
                let Some(f) = file.as_mut() else { return Ok(()); };
                let cx = x.saturating_sub(offset.0);
                let cy = y.saturating_sub(offset.1);
                let cz = z.saturating_sub(offset.2);
                let mut buf = [0u8; ROW_BYTES as usize];
                let n = {
                    let mut cur = &mut buf[..];
                    write!(
                        cur,
                        "{:>w_seq$},{:>w_ts$},{:>w_a$},{:>w_a$},{:>w_a$}\n",
                        seq, ts_ms, cx, cy, cz,
                        w_seq = W_SEQ,
                        w_ts = W_TS_MS,
                        w_a = W_AXIS,
                    ).context("capture row format")?;
                    (ROW_BYTES as usize) - cur.len()
                };
                debug_assert_eq!(n, ROW_BYTES as usize, "capture CSV width drift");
                f.write_all(&buf[..n]).context("capture write")?;

                *records_since_sync = records_since_sync.saturating_add(1);
                if *records_since_sync >= SYNC_EVERY_N_RECORDS {
                    if let Err(e) = f.sync_all() {
                        warn!("[capture] periodic sync_all failed: {} — continuing", e);
                    }
                    *records_since_sync = 0;
                }
                Ok(())
            }
        }
    }

    /// Open `<mount>/captures/<file_name>` for write. Falls back to
    /// `<mount>/cap-<file_name>` if `create_dir_all` fails (matches
    /// SPEC §4's documented fall-back, motivated by the ESP-IDF
    /// FATFS create_dir_all quirk noted in
    /// SPEC-R2-ROCKER-SENSOR §6.1).
    fn open_capture_file(&self, file_name: &str) -> Result<File> {
        let dir = self.mount_point.join(CAPTURES_SUBDIR);
        let primary = dir.join(file_name);

        // Try the proper sub-directory layout first.
        if fs::create_dir_all(&dir).is_ok() {
            match self.open_one(&primary) {
                Ok(f) => return Ok(f),
                Err(e) => warn!(
                    "[capture] open {:?} failed: {} — falling back to mount root",
                    primary, e
                ),
            }
        } else {
            warn!(
                "[capture] create_dir_all {:?} failed — falling back to mount root",
                dir
            );
        }

        let fallback = self.mount_point.join(format!("cap-{}", file_name));
        self.open_one(&fallback)
    }

    fn open_one(&self, path: &std::path::Path) -> Result<File> {
        // Ring-style branch: FATFS rejects `OpenOptions::append(true)`
        // and `create(true) + write(true) (no truncate)`, but
        // `File::create` (O_CREAT|O_TRUNC|O_WRONLY) and
        // `OpenOptions::write(true)` on an existing file both work.
        // Captures use unique filenames so the file shouldn't exist
        // yet; if it does (e.g. operator re-used a name), we open +
        // seek-to-end so accumulated data isn't truncated.
        if fs::metadata(path).is_ok() {
            let mut f = OpenOptions::new()
                .write(true)
                .open(path)
                .with_context(|| format!("open existing {:?} for write", path))?;
            f.seek(SeekFrom::End(0))
                .with_context(|| format!("seek-end on {:?}", path))?;
            Ok(f)
        } else {
            File::create(path)
                .with_context(|| format!("create {:?}", path))
        }
    }
}

impl Drop for CaptureMgr {
    fn drop(&mut self) {
        if let CaptureState::Recording { file: Some(ref mut f), ref file_name, .. } = self.state {
            let label = file_name.as_deref().unwrap_or("<unnamed>");
            if let Err(e) = f.sync_all() {
                warn!("[capture] drop-time sync_all of {} failed: {}", label, e);
            }
        }
    }
}

/// Validate a run name per SPEC-R2-ROCKER-CAPTURE §3 — UTF-8 already
/// (Rust `&str`), 1..=32 bytes, charset `[A-Za-z0-9_-]`.
fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX { return false; }
    name.bytes().all(|b| matches!(b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(is_valid_name("run-01"));
        assert!(is_valid_name("RUN_01_asphaltA"));
        assert!(is_valid_name("a"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("with space"));
        assert!(!is_valid_name("with/slash"));
        assert!(!is_valid_name("with.dot"));
        assert!(!is_valid_name(&"a".repeat(33)));
    }

    #[test]
    fn row_width() {
        // Confirm the format matches the declared ROW_BYTES.
        let mut buf = [0u8; 128];
        let mut cur = &mut buf[..];
        write!(
            cur,
            "{:>w_seq$},{:>w_ts$},{:>w_a$},{:>w_a$},{:>w_a$}\n",
            0u32, 0i64, 0i32, 0i32, 0i32,
            w_seq = W_SEQ, w_ts = W_TS_MS, w_a = W_AXIS,
        ).unwrap();
        let n = buf.len() - cur.len();
        assert_eq!(n as u64, ROW_BYTES);
    }
}
