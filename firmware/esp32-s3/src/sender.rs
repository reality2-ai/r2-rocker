//! TCP client that emits R2-WIRE frames to the dashboard.
//!
//! Phase 0.5+ scope: connect, send `r2.sensor.announce`, then loop
//! emitting synthetic acceleration at the configured rate plus battery
//! every 30 s. No SD ring, no ACK handling, no catch-up mode yet —
//! that's Phase 3+.
//!
//! On any TCP error the loop sleeps briefly and reconnects; samples
//! produced during the gap are dropped (the SD ring will fix this in
//! Phase 3).

use anyhow::{Context, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys::{
    esp_mac_type_t_ESP_MAC_WIFI_STA, esp_ota_mark_app_valid_cancel_rollback,
    esp_random, esp_read_mac, ESP_OK,
};
use log::{info, warn};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::identity::Identity;
use crate::led::LedHandle;
use crate::sim::{AccelSim, BatterySim};
use crate::wire::{
    frame_for_tcp, CborWriter, EVT_SENSOR_ACCELERATION, EVT_SENSOR_ANNOUNCE,
    EVT_SENSOR_BATTERY, EVT_SENSOR_STATUS,
};

const SAMPLE_RATE_HZ: u32 = 100;
const BATTERY_PERIOD_MS: u64 = 30_000;
/// Status events drive the dashboard's virtual LED; 2 s cadence is
/// snappy enough to feel live + cheap on the wire (~28 bytes/frame).
const STATUS_PERIOD_MS: u64 = 2_000;
const RECONNECT_BACKOFF_MS_INIT: u64 = 1_000;
const RECONNECT_BACKOFF_MS_MAX: u64 = 30_000;
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Identifies this exact build on the wire — semver + git short SHA
/// (+ "-dirty" if uncommitted) + " sim" tag while we're not driving a
/// real ADXL355 yet. Per `SPEC-R2-ROCKER-WIRE` §3.1, this string is
/// what the dashboard sees in `r2.sensor.announce`'s `fw_ver` field
/// and uses to decide whether an OTA update is needed.
const FW_VER: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("R2_GIT_SHA"),
    " sim"
);

pub struct Sender {
    pub gateway: SocketAddr,
    pub hostname: String,
    accel: AccelSim,
    battery: BatterySim,
    /// Per-device Ed25519 identity, NVS-persisted (Phase 5a).
    identity: Arc<Identity>,
    /// Read-only handle to the current LED FSM state. Sent on the
    /// wire as `r2.sensor.status` so the dashboard's virtual LEDs
    /// mirror the physical RGB LED.
    led: LedHandle,
    boot_instant: Instant,
    /// OTA-rollback gate (SPEC-R2-ROCKER-SENSOR §12.2): cleared until
    /// the first successful TCP frame round-trip, then we tell the
    /// bootloader the new image is good. A buggy firmware that joins
    /// WiFi but can't reach the dashboard never sets this, so the
    /// bootloader rolls back on the next reset.
    app_validated: bool,
}

impl Sender {
    pub fn new(
        gateway: SocketAddr,
        hostname: String,
        identity: Arc<Identity>,
        led: LedHandle,
    ) -> Self {
        Self {
            gateway,
            hostname,
            accel: AccelSim::rocker_default(),
            battery: BatterySim::lipo_default(),
            identity,
            led,
            boot_instant: Instant::now(),
            app_validated: false,
        }
    }

    /// Run forever — connect, stream, reconnect on error.
    pub fn run(&mut self) -> ! {
        let mut backoff = RECONNECT_BACKOFF_MS_INIT;
        loop {
            match self.session() {
                Ok(()) => {
                    warn!("sender: session ended cleanly — reconnecting");
                    backoff = RECONNECT_BACKOFF_MS_INIT;
                }
                Err(e) => {
                    warn!(
                        "sender: session error: {} — reconnect in {} ms",
                        e, backoff
                    );
                    FreeRtos::delay_ms(backoff as u32);
                    backoff = (backoff * 2).min(RECONNECT_BACKOFF_MS_MAX);
                }
            }
        }
    }

    fn session(&mut self) -> Result<()> {
        info!("sender: connecting to {}…", self.gateway);
        let mut stream = TcpStream::connect_timeout(&self.gateway, TCP_CONNECT_TIMEOUT)
            .with_context(|| format!("connect_timeout {}", self.gateway))?;
        stream.set_nodelay(true).ok();
        info!("sender: TCP up to {}", self.gateway);

        self.send_announce(&mut stream).context("send_announce")?;

        let sample_period_ms = (1000 / SAMPLE_RATE_HZ).max(1) as u64;
        let mut next_sample = Instant::now();
        let mut next_battery = Instant::now() + Duration::from_millis(BATTERY_PERIOD_MS);
        let mut next_status = Instant::now(); // first status fires immediately
        let mut seq: u32 = 1;

        loop {
            let now = Instant::now();
            if now >= next_sample {
                self.send_sample(&mut stream, seq).context("send_sample")?;
                // First successful frame round-trip → tell the
                // bootloader this image is good. Calling more than
                // once is a no-op on subsequent boots, so the flag
                // just avoids the syscall after we know it's done.
                if !self.app_validated {
                    self.mark_app_valid();
                    self.app_validated = true;
                }
                seq = seq.wrapping_add(1);
                next_sample += Duration::from_millis(sample_period_ms);
            }
            if now >= next_battery {
                self.send_battery(&mut stream).context("send_battery")?;
                next_battery += Duration::from_millis(BATTERY_PERIOD_MS);
            }
            if now >= next_status {
                self.send_status(&mut stream).context("send_status")?;
                next_status += Duration::from_millis(STATUS_PERIOD_MS);
            }

            // Sleep until the next due event.
            let next = next_sample.min(next_battery).min(next_status);
            if next > now {
                let dt = next - now;
                let dt_ms = dt.as_millis().min(50) as u32;
                if dt_ms > 0 {
                    FreeRtos::delay_ms(dt_ms);
                }
            }
        }
    }

    // ── Frame builders ──────────────────────────────────────────────

    fn ts_ms(&self) -> u32 {
        self.boot_instant.elapsed().as_millis() as u32
    }

    fn random_msg_id(&self) -> u16 {
        // SAFETY: esp_random is always callable.
        (unsafe { esp_random() } & 0xFFFF) as u16
    }

    fn send_announce(&self, stream: &mut TcpStream) -> Result<()> {
        // CBOR map per SPEC-R2-ROCKER-WIRE §3.1.
        // The signature at key 6 covers the canonical CBOR of keys 0..5.
        // We build the body twice for byte-identical encoding: once for
        // signing, once again as the prefix of the full payload that
        // adds key 6 (the signature).
        let mut nonce = [0u8; 16];
        unsafe {
            for chunk in nonce.chunks_exact_mut(4) {
                chunk.copy_from_slice(&esp_random().to_le_bytes());
            }
        }
        let device_pk = self.identity.device_pk();
        let ts_ms = self.ts_ms() as u64;
        let last_seq: u64 = 0; // Phase 3 will pull from NVS/SD ring tail.

        let write_keys_0_to_5 = |w: &mut CborWriter, n_keys: usize| {
            w.map(n_keys);
            w.key(0); w.bytes(&device_pk);
            w.key(1); w.text(&self.hostname);
            w.key(2); w.text(FW_VER);
            w.key(3); w.u(last_seq);
            w.key(4); w.u(ts_ms);
            w.key(5); w.bytes(&nonce);
        };

        // 1. Encode body (keys 0..5 only, map header = 6 entries).
        let mut body_buf = [0u8; 256];
        let mut bw = CborWriter::new(&mut body_buf);
        write_keys_0_to_5(&mut bw, 6);
        let body_len = bw.pos();
        let body = &body_buf[..body_len];

        // 2. Sign body bytes with device key.
        let sig = self.identity.sign(body);

        // 3. Encode full payload (keys 0..6, map header = 7 entries).
        let mut payload = [0u8; 320];
        let mut w = CborWriter::new(&mut payload);
        write_keys_0_to_5(&mut w, 7);
        w.key(6); w.bytes(&sig);
        let used = w.pos();

        let mut frame = [0u8; 384];
        let n = frame_for_tcp(&mut frame, self.random_msg_id(), EVT_SENSOR_ANNOUNCE, &payload[..used]);
        stream.write_all(&frame[..n])?;
        info!(
            "sender: sent ANNOUNCE ({} bytes payload, signed; device_pk first 8 bytes = {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}…)",
            used,
            device_pk[0], device_pk[1], device_pk[2], device_pk[3],
            device_pk[4], device_pk[5], device_pk[6], device_pk[7],
        );
        Ok(())
    }

    fn send_sample(&self, stream: &mut TcpStream, seq: u32) -> Result<()> {
        let t_s = self.boot_instant.elapsed().as_secs_f32();
        let (x, y, z) = self.accel.sample(t_s);

        let mut payload = [0u8; 32];
        let mut w = CborWriter::new(&mut payload);
        w.map(5);
        w.key(0); w.u(seq as u64);
        w.key(1); w.u(self.ts_ms() as u64);
        w.key(2); w.i(x as i64);
        w.key(3); w.i(y as i64);
        w.key(4); w.i(z as i64);
        let used = w.pos();

        let mut frame = [0u8; 64];
        let n = frame_for_tcp(&mut frame, self.random_msg_id(), EVT_SENSOR_ACCELERATION, &payload[..used]);
        stream.write_all(&frame[..n])?;
        Ok(())
    }

    /// OTA-rollback gate. Called once per session lifetime, the first
    /// time a frame round-trips successfully to the dashboard.
    fn mark_app_valid(&self) {
        let rc = unsafe { esp_ota_mark_app_valid_cancel_rollback() };
        if rc == ESP_OK {
            info!("[ota-gate] image marked VALID after first frame round-trip");
        } else {
            warn!("[ota-gate] esp_ota_mark_app_valid_cancel_rollback returned {}", rc);
        }
    }

    /// Emit `r2.sensor.status` with the current LED FSM state value.
    /// Dashboard consumes `payload.0` as `fsmState` and lights the
    /// virtual LED with the matching tg-* CSS class. SD%, sample-rate,
    /// uptime fields TBD when those subsystems land.
    fn send_status(&self, stream: &mut TcpStream) -> Result<()> {
        let state = self.led.current() as u8;
        let mut payload = [0u8; 12];
        let mut w = CborWriter::new(&mut payload);
        w.map(2);
        w.key(0); w.u(state as u64);              // FSM state (LedState repr)
        w.key(1); w.u(self.ts_ms() as u64);       // ts_ms
        let used = w.pos();

        let mut frame = [0u8; 32];
        let n = frame_for_tcp(&mut frame, self.random_msg_id(), EVT_SENSOR_STATUS, &payload[..used]);
        stream.write_all(&frame[..n])?;
        Ok(())
    }

    fn send_battery(&self, stream: &mut TcpStream) -> Result<()> {
        let t_s = self.boot_instant.elapsed().as_secs_f32();
        let (mv, pct) = self.battery.sample(t_s);

        let mut payload = [0u8; 24];
        let mut w = CborWriter::new(&mut payload);
        w.map(4);
        w.key(0); w.u(mv as u64);
        w.key(1); w.u(pct as u64);
        w.key(2); w.bool(false); // charging — always false on this board (off-board charging)
        w.key(3); w.u(self.ts_ms() as u64);
        let used = w.pos();

        let mut frame = [0u8; 48];
        let n = frame_for_tcp(&mut frame, self.random_msg_id(), EVT_SENSOR_BATTERY, &payload[..used]);
        stream.write_all(&frame[..n])?;
        info!("sender: battery {} mV ({}%)", mv, pct);
        Ok(())
    }
}

/// Compute a default hostname like `rocker-1cdbd441283c` from the WiFi MAC.
pub fn default_hostname() -> String {
    let mut mac = [0u8; 6];
    unsafe { esp_read_mac(mac.as_mut_ptr(), esp_mac_type_t_ESP_MAC_WIFI_STA); }
    format!(
        "rocker-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}
