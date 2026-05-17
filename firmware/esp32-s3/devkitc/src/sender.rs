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
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adxl355::Adxl355;
use crate::clock::Clock;
use crate::identity::Identity;
use crate::led::LedHandle;
use crate::ring::Ring;
use crate::sim::{AccelSim, BatterySim};
use crate::wire::{
    decode_compact_frame, frame_for_tcp, parse_dash_ack_through_seq,
    parse_set_clock_offset, parse_sync_pulse_req_id,
    CborWriter,
    EVT_DASH_ACK, EVT_DASH_SET_CLOCK_OFFSET, EVT_DASH_SYNC_PULSE, EVT_SENSOR_ACCELERATION,
    EVT_SENSOR_ANNOUNCE, EVT_SENSOR_BATTERY, EVT_SENSOR_STATUS, EVT_SENSOR_SYNC_PONG,
};

const SAMPLE_RATE_HZ: u32 = 100;
const BATTERY_PERIOD_MS: u64 = 30_000;
/// Status events drive the dashboard's virtual LED; 2 s cadence is
/// snappy enough to feel live + cheap on the wire (~28 bytes/frame).
const STATUS_PERIOD_MS: u64 = 2_000;
const RECONNECT_BACKOFF_MS_INIT: u64 = 1_000;
const RECONNECT_BACKOFF_MS_MAX: u64 = 30_000;
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Identifies this exact build on the wire —
/// `<semver>-<build-date-time>[-sim]+<git-sha>[-dirty]`. The `-sim`
/// segment is present only when the sender is running off the
/// synthetic accelerometer (ADXL355 init failed). Built per-instance
/// in `Sender::new` so it reflects runtime reality, not a compile-time
/// guess.
fn build_fw_ver(_real_adxl: bool) -> String {
    // The fw_ver string is composed once at build time and emitted
    // verbatim — see firmware/.../build.rs. In dev mode it's the
    // semver + UTC timestamp + git sha; in release mode (R2_RELEASE=1)
    // it's just the tag (e.g. `v0.2.0`) so the dashboard can match it
    // 1:1 against the GitHub Releases tag list for "needs update?"
    // comparison.
    //
    // The `-sim` suffix that used to differ between real/sim ADXL was
    // dropped — runtime sim-fallback is signalled via the announce
    // payload's `data_source` field per SPEC-R2-ROCKER-SENSOR-HEALTH,
    // which the dashboard already shows on the device card.
    env!("R2_FW_VER").to_string()
}

pub struct Sender {
    pub gateway: SocketAddr,
    pub hostname: String,
    /// Real ADXL355 driver. Some(_) means SPI init + WHO_AM_I + standby-
    /// clear succeeded; samples come from this. None means the driver
    /// failed at boot and we fall back to `accel_sim` so the wire path
    /// still works for further debug.
    adxl: Option<Adxl355>,
    /// Fallback / always-available simulator. Used when `adxl` is None
    /// OR when an individual sample read errors (logged + skipped).
    accel_sim: AccelSim,
    battery: BatterySim,
    /// Per-device Ed25519 identity, NVS-persisted (Phase 5a).
    identity: Arc<Identity>,
    /// Read-only handle to the current LED FSM state. Sent on the
    /// wire as `r2.sensor.status` so the dashboard's virtual LEDs
    /// mirror the physical RGB LED.
    led: LedHandle,
    /// Synchronised clock — every emitted `ts_ms` flows through this
    /// per SPEC-R2-ROCKER-TIMESYNC. NVS-backed offset is applied
    /// transparently inside `Clock::ts_ms`.
    clock: Arc<Clock>,
    /// SD ring writer (SPEC-R2-ROCKER-SENSOR §6). `None` means the SD
    /// path is unavailable for this boot (no card, mount failed) — the
    /// sender continues streaming-only.
    ring: Option<Ring>,
    /// Last `through_seq` received via `r2.dash.ack`. Used to free SD
    /// segments whose records have all been acknowledged by the
    /// dashboard. RAM-only in v0.1 — spec §6.4 calls for rate-limited
    /// NVS persistence, deferred.
    last_acked_seq: u32,
    boot_instant: Instant,
    /// OTA-rollback gate (SPEC-R2-ROCKER-SENSOR §12.2): cleared until
    /// the first successful TCP frame round-trip, then we tell the
    /// bootloader the new image is good. A buggy firmware that joins
    /// WiFi but can't reach the dashboard never sets this, so the
    /// bootloader rolls back on the next reset.
    app_validated: bool,
    /// Build identifier sent in every `r2.sensor.announce`. Reflects
    /// the runtime ADXL355 path: includes `-sim` only when we fell back
    /// to the synthetic accelerometer.
    fw_ver: String,
}

impl Sender {
    pub fn new(
        gateway: SocketAddr,
        hostname: String,
        identity: Arc<Identity>,
        led: LedHandle,
        adxl: Option<Adxl355>,
        clock: Arc<Clock>,
        ring: Option<Ring>,
    ) -> Self {
        let fw_ver = build_fw_ver(adxl.is_some());
        Self {
            gateway,
            hostname,
            adxl,
            accel_sim: AccelSim::rocker_default(),
            battery: BatterySim::lipo_default(),
            identity,
            led,
            clock,
            ring,
            last_acked_seq: 0,
            boot_instant: Instant::now(),
            app_validated: false,
            fw_ver,
        }
    }

    /// Run forever — connect, stream, reconnect on error.
    pub fn run(&mut self) -> ! {
        let mut backoff = RECONNECT_BACKOFF_MS_INIT;
        loop {
            // Reflect "trying to reach the dashboard" on the LED before
            // every connect attempt. session() flips it back to a
            // Streaming* state once the TCP connect succeeds. Without
            // this the LED stays green through the entire reconnect
            // window (e.g. when the dashboard cycles the hotspot) and
            // the operator can't tell that the link is actually down.
            self.led.set(crate::led::LedState::WifiConnecting);
            match self.session() {
                Ok(()) => {
                    warn!("sender: session ended cleanly — reconnecting");
                    backoff = RECONNECT_BACKOFF_MS_INIT;
                }
                Err(e) => {
                    // {:#} prints the full anyhow error chain
                    // (context + underlying io::Error). Without it the
                    // log only shows the top-level context string and
                    // we can't tell what really failed.
                    warn!(
                        "sender: session error: {:#} — reconnect in {} ms",
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
        // Short read timeout so the single-threaded loop below can poll
        // for inbound bytes between writes without blocking. ESP-IDF's
        // lwIP doesn't implement dup() (try_clone returns ENOSYS), so a
        // separate reader thread isn't an option — we interleave reads
        // and writes on the same TcpStream. 1 ms is the FreeRTOS tick
        // granularity; reads either return immediately on data or
        // unblock within ~1 ms otherwise.
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok();
        info!("sender: TCP up to {}", self.gateway);

        // TCP is up — restore the streaming-state LED. `real_adxl`
        // decides green-heartbeat vs amber-heartbeat (sim fallback).
        self.led.set(if self.adxl.is_some() {
            crate::led::LedState::StreamingLive
        } else {
            crate::led::LedState::StreamingDegradedSim
        });

        self.send_announce(&mut stream).context("send_announce")?;

        let sample_period_ms = (1000 / SAMPLE_RATE_HZ).max(1) as u64;
        let mut next_sample = Instant::now();
        let mut next_battery = Instant::now() + Duration::from_millis(BATTERY_PERIOD_MS);
        let mut next_status = Instant::now(); // first status fires immediately
        // Seed the seq counter from the SD ring's recovered tail per
        // SPEC-R2-ROCKER-SENSOR §6.5: continue right after the
        // highest-numbered record on disk. Without a ring (no SD)
        // start at 1 — the dashboard's `last_seq=0` in the announce
        // signals there's no durability and the dashboard SHOULD NOT
        // expect to fill backlog gaps.
        let mut seq: u32 = self.ring.as_ref().map(|r| r.tail_seq().wrapping_add(1)).unwrap_or(1);
        info!("sender: starting at seq={} ({})",
              seq,
              if self.ring.is_some() { "resumed from SD ring" } else { "no SD ring — fresh count" });

        // Accumulating buffer for incremental frame parsing — TCP can
        // deliver bytes in any chunk size, so we keep partial frames
        // here across loop iterations.
        let mut inbound_buf: Vec<u8> = Vec::with_capacity(256);
        let mut read_scratch = [0u8; 256];

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

            // Inbound poll. read() returns WouldBlock / TimedOut if no
            // data within the 1 ms window — that's fine, we go around
            // the loop and try again next tick. Ok(0) is the dashboard
            // closing its half of the socket; treat as session end.
            match stream.read(&mut read_scratch) {
                Ok(0) => {
                    return Err(anyhow::anyhow!("dashboard closed TCP socket"));
                }
                Ok(n) => {
                    inbound_buf.extend_from_slice(&read_scratch[..n]);
                    while inbound_buf.len() >= 2 {
                        let frame_len = u16::from_be_bytes([inbound_buf[0], inbound_buf[1]]) as usize;
                        if frame_len < 12 || frame_len > 4096 {
                            warn!("[sender] bad inbound frame length {} — dropping session", frame_len);
                            return Err(anyhow::anyhow!("inbound frame length {} out of range", frame_len));
                        }
                        if inbound_buf.len() < 2 + frame_len { break; }
                        let frame: Vec<u8> = inbound_buf.drain(..2 + frame_len).collect();
                        let body = &frame[2..2 + frame_len];
                        if let Some((event_hash, payload)) = decode_compact_frame(body) {
                            self.dispatch_inbound(event_hash, payload, &mut stream)?;
                        }
                    }
                }
                Err(e) if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {
                    // No bytes in the 1 ms window — expected; continue.
                }
                Err(e) => {
                    return Err(e).context("inbound read");
                }
            }

            // Yield briefly until the next due event. Capped low so the
            // inbound poll runs at high enough cadence to keep the
            // dashboard's command round-trip latency snappy. The 1 ms
            // read above already provides some sleep when no data
            // arrives, so this is just a safety yield.
            let next = next_sample.min(next_battery).min(next_status);
            if next > now {
                let dt = next - now;
                let dt_ms = dt.as_millis().min(5) as u32;
                if dt_ms > 0 {
                    FreeRtos::delay_ms(dt_ms);
                }
            }
        }
    }

    /// Handle one inbound dashboard → sensor frame. Same dispatch surface
    /// the old reader thread had, but reads/writes the SAME `stream`
    /// because ESP-IDF's lwIP doesn't support socket dup() (`try_clone`
    /// returns ENOSYS).
    fn dispatch_inbound(
        &mut self,
        event_hash: u32,
        payload: &[u8],
        stream: &mut TcpStream,
    ) -> Result<()> {
        match event_hash {
            EVT_DASH_ACK => {
                if let Some(through_seq) = parse_dash_ack_through_seq(payload) {
                    if through_seq > self.last_acked_seq {
                        self.last_acked_seq = through_seq;
                        if let Some(ref mut ring) = self.ring {
                            if let Err(e) = ring.free_through(through_seq) {
                                warn!("[ring] free_through({}) failed: {}", through_seq, e);
                            }
                        }
                    }
                } else {
                    warn!("[sender] malformed dash.ack payload");
                }
            }
            EVT_DASH_SET_CLOCK_OFFSET => {
                if let Some(delta_ms) = parse_set_clock_offset(payload) {
                    info!("[sender] r2.dash.set_clock_offset delta_ms={:+}", delta_ms);
                    self.clock.apply_delta(delta_ms);
                } else {
                    warn!("[sender] malformed set_clock_offset payload");
                }
            }
            EVT_DASH_SYNC_PULSE => {
                if let Some(req_id) = parse_sync_pulse_req_id(payload) {
                    let sensor_ts_ms = self.clock.ts_ms_i64();
                    let mut body = [0u8; 32];
                    let body_len = {
                        let mut w = CborWriter::new(&mut body);
                        w.map(2);
                        w.key(0); w.u(req_id as u64);
                        w.key(1); w.u(sensor_ts_ms as u64);
                        w.pos()
                    };
                    let mut frame = [0u8; 48];
                    let n = frame_for_tcp(
                        &mut frame,
                        (req_id & 0xFFFF) as u16,
                        EVT_SENSOR_SYNC_PONG,
                        &body[..body_len],
                    );
                    stream.write_all(&frame[..n]).context("sync_pong write")?;
                } else {
                    warn!("[sender] malformed sync_pulse payload");
                }
            }
            other => {
                info!("[sender] unhandled inbound event 0x{:08X} ({} byte payload)", other, payload.len());
            }
        }
        Ok(())
    }

    // ── Frame builders ──────────────────────────────────────────────

    fn ts_ms(&self) -> u32 {
        self.clock.ts_ms()
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
            w.key(2); w.text(&self.fw_ver);
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

    fn send_sample(&mut self, stream: &mut TcpStream, seq: u32) -> Result<()> {
        // Prefer real samples when the chip is initialised; fall back to
        // the simulator if SPI init failed at boot OR if an individual
        // read errors. A per-read failure logs once and resorts to sim
        // for that one sample — the chip may still be usable for the
        // next tick (e.g. transient bus glitch).
        let (x, y, z) = if let Some(adxl) = self.adxl.as_mut() {
            match adxl.read_xyz_lsb() {
                Ok(xyz) => xyz,
                Err(e) => {
                    warn!("[ADXL355] sample read failed: {e:?} — using sim for this tick");
                    let t_s = self.boot_instant.elapsed().as_secs_f32();
                    self.accel_sim.sample(t_s)
                }
            }
        } else {
            let t_s = self.boot_instant.elapsed().as_secs_f32();
            self.accel_sim.sample(t_s)
        };

        // Sample once so the wire ts_ms and the on-disk ts_ms come
        // from the same clock read. The wire path uses u32 today (see
        // §3 of SPEC-R2-ROCKER-WIRE); the SD path widens to i64 so the
        // CSV column is self-contained — operators reading the file
        // post-hoc shouldn't have to undo a u32 truncation just to
        // recover the wall-clock value.
        let ts_ms_i64 = self.clock.ts_ms_i64();
        let ts_ms = ts_ms_i64 as u32;

        // Durable copy first (SPEC-R2-ROCKER-SENSOR §7.2). Best-effort:
        // log + continue on write error so the wire path stays alive
        // even with a flaky/full card. v0.1 omits the in-RAM retry
        // queue + ERROR escalation that §6.7 calls for.
        if let Some(ref mut ring) = self.ring {
            if let Err(e) = ring.append(seq, ts_ms_i64, x, y, z) {
                warn!("[ring] append seq={} failed: {}", seq, e);
            }
        }

        let mut payload = [0u8; 32];
        let mut w = CborWriter::new(&mut payload);
        w.map(5);
        w.key(0); w.u(seq as u64);
        w.key(1); w.u(ts_ms as u64);
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

    /// Emit `r2.sensor.status` with the current LED FSM state value
    /// and the data-source health signal. Dashboard consumes `payload.0`
    /// as `fsmState` (lights the virtual LED) and `payload.7` as
    /// `dataSource` (0 = real ADXL355, 1 = simulator) per
    /// SPEC-R2-ROCKER-SENSOR-HEALTH §3. SD%, sample-rate, uptime fields
    /// TBD when those subsystems land.
    fn send_status(&self, stream: &mut TcpStream) -> Result<()> {
        let state = self.led.current() as u8;
        let data_source: u8 = if self.adxl.is_some() { 0 } else { 1 };
        let mut payload = [0u8; 16];
        let mut w = CborWriter::new(&mut payload);
        w.map(3);
        w.key(0); w.u(state as u64);              // FSM state (LedState repr)
        w.key(1); w.u(self.ts_ms() as u64);       // ts_ms
        w.key(7); w.u(data_source as u64);        // data_source: 0 real, 1 sim
        let used = w.pos();

        let mut frame = [0u8; 32];
        let n = frame_for_tcp(&mut frame, self.random_msg_id(), EVT_SENSOR_STATUS, &payload[..used]);
        stream.write_all(&frame[..n])?;
        Ok(())
    }

    fn send_battery(&self, stream: &mut TcpStream) -> Result<()> {
        let t_s = self.boot_instant.elapsed().as_secs_f32();
        let (mv, pct) = self.battery.sample(t_s);

        // LOW_BATTERY overlay (orange slow pulse) per SPEC §4.1 / §8.4.
        // Threshold 3300 mV; cleared at ≥3400 mV (hysteresis). The flag
        // is read by the LED loop and overlays whatever underlying state
        // is current.
        const LOW_BATTERY_MV: u16 = 3300;
        const LOW_BATTERY_CLEAR_MV: u16 = 3400;
        if mv <= LOW_BATTERY_MV {
            self.led.set_low_battery(true);
        } else if mv >= LOW_BATTERY_CLEAR_MV {
            self.led.set_low_battery(false);
        }

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
