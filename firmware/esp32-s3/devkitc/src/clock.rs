//! Synchronised time helper — SPEC-R2-ROCKER-TIMESYNC.
//!
//! Every `ts_ms` value the sensor emits or persists is computed as
//! `uptime_ms + clock_offset_ms`. The offset is NVS-persisted (key
//! `clock_offset` in namespace `r2_rocker`) so it survives reboots
//! mid-experiment.
//!
//! The dashboard pushes signed deltas via `r2.dash.set_clock_offset`;
//! the sensor adds each delta to the running offset and persists
//! through `apply_delta`.

use anyhow::{Context, Result};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{info, warn};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const NVS_NAMESPACE: &str = "r2_rocker";
const NVS_KEY: &str = "clock_offset";

/// NVS-backed monotonic + offset clock. Cheap to clone (`Arc`-shared).
///
/// `offset_ms` is held under a `Mutex` rather than an atomic because
/// Xtensa-targeted Rust stdlib has no native 64-bit atomic. The lock
/// is uncontended in practice (one writer thread updates on
/// `set_clock_offset`, multiple readers acquire briefly per sample).
pub struct Clock {
    boot: Instant,
    offset_ms: Mutex<i64>,
    nvs: Mutex<EspNvs<NvsDefault>>,
}

impl Clock {
    /// Load `clock_offset_ms` from NVS (default 0 if absent or unreadable)
    /// and snapshot a boot reference for monotonic uptime.
    pub fn load(nvs_part: EspDefaultNvsPartition) -> Result<Arc<Self>> {
        let nvs = EspNvs::<NvsDefault>::new(nvs_part, NVS_NAMESPACE, true)
            .context("open NVS namespace for clock")?;

        let mut buf = [0u8; 8];
        let offset_ms = match nvs.get_blob(NVS_KEY, &mut buf) {
            Ok(Some(slice)) if slice.len() == 8 => i64::from_le_bytes(buf),
            _ => 0,
        };
        info!("[clock] loaded clock_offset_ms = {} from NVS", offset_ms);

        Ok(Arc::new(Self {
            boot: Instant::now(),
            offset_ms: Mutex::new(offset_ms),
            nvs: Mutex::new(nvs),
        }))
    }

    /// Current synchronised time, truncated to u32 for the wire format.
    /// Wraparound on the u32 cast is per spec (TIMESYNC §5.4) — analysis
    /// tools handle it.
    pub fn ts_ms(&self) -> u32 {
        self.ts_ms_i64() as u32
    }

    /// Same value as ts_ms() but as i64 (for SD records, logs, batched
    /// timestamps, anywhere needing wider range). Phase 2 SD ring is
    /// the first caller; until that lands the wire-side u32 form is the
    /// only path actually used.
    #[allow(dead_code)]
    pub fn ts_ms_i64(&self) -> i64 {
        let uptime = self.boot.elapsed().as_millis() as i64;
        let offset = *self.offset_ms.lock().expect("clock offset poisoned");
        uptime.wrapping_add(offset)
    }

    /// Current `clock_offset_ms` (diagnostic; used by the status frame
    /// and event-log payloads once those are extended in a later phase).
    #[allow(dead_code)]
    pub fn offset_ms(&self) -> i64 {
        *self.offset_ms.lock().expect("clock offset poisoned")
    }

    /// Apply a signed delta per `r2.dash.set_clock_offset`. Updates the
    /// in-RAM offset and writes through to NVS. Per SPEC-R2-ROCKER-TIMESYNC
    /// §2.3, the protocol semantics are "add this delta to the existing
    /// offset" — set_clock_offset pushes are rare (once per calibration +
    /// occasional drift correction), so we persist on every apply rather
    /// than the rate-limited write the SD/ack path uses.
    pub fn apply_delta(&self, delta_ms: i64) {
        let new_offset = {
            let mut g = self.offset_ms.lock().expect("clock offset poisoned");
            *g = g.wrapping_add(delta_ms);
            *g
        };
        info!(
            "[clock] applied delta {:+} ms — new clock_offset_ms = {}",
            delta_ms, new_offset
        );
        if let Ok(mut nvs) = self.nvs.lock() {
            let bytes = new_offset.to_le_bytes();
            if let Err(e) = nvs.set_blob(NVS_KEY, &bytes) {
                warn!("[clock] NVS persist failed: {e}");
            }
        }
    }
}
