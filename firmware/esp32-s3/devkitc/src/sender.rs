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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adxl355::Adxl355;
use crate::clock::Clock;
use crate::identity::Identity;
use crate::led::LedHandle;
use crate::sim::{AccelSim, BatterySim};
use crate::wire::{
    decode_compact_frame, frame_for_tcp, parse_set_clock_offset, parse_sync_pulse_req_id,
    CborWriter,
    EVT_DASH_SET_CLOCK_OFFSET, EVT_DASH_SYNC_PULSE, EVT_SENSOR_ACCELERATION,
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
fn build_fw_ver(real_adxl: bool) -> String {
    if real_adxl {
        format!(
            "{}-{}+{}",
            env!("CARGO_PKG_VERSION"),
            env!("R2_BUILD_TIMESTAMP"),
            env!("R2_GIT_SHA"),
        )
    } else {
        format!(
            "{}-{}-sim+{}",
            env!("CARGO_PKG_VERSION"),
            env!("R2_BUILD_TIMESTAMP"),
            env!("R2_GIT_SHA"),
        )
    }
}

pub struct Sender {
    pub gateway: SocketAddr,
    pub hostname: String,
    /// Real ADXL355 driver. Some(_) means SPI init + WHO_AM_I + standby-
    /// clear succeeded; samples come from this. None means the driver
    /// failed at boot and we fall back to `accel_sim` so the wire path
    /// still works for further debug.
    adxl: Option<Adxl355<'static>>,
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
        adxl: Option<Adxl355<'static>>,
        clock: Arc<Clock>,
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
            boot_instant: Instant::now(),
            app_validated: false,
            fw_ver,
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

        // Spawn an inbound-frame reader on a try_cloned half of the socket.
        // Dispatches dashboard → sensor commands. Frames that need to
        // round-trip a reply (sync_pulse → sync_pong) are pushed back
        // through `outbound_rx` so the main writer loop emits them on
        // the same socket — avoids two threads racing the writer side.
        let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>();
        let stop_reader = Arc::new(AtomicBool::new(false));
        let _reader_handle = {
            let read_stream = stream
                .try_clone()
                .context("try_clone TCP stream for reader thread")?;
            // 2-second read timeout so the reader notices stop_reader
            // promptly when session() exits without inbound traffic.
            read_stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .ok();
            let stop = stop_reader.clone();
            let clock = self.clock.clone();
            let out_tx = outbound_tx.clone();
            std::thread::Builder::new()
                .stack_size(4096)
                .name("sender-rx".into())
                .spawn(move || inbound_reader_loop(read_stream, stop, clock, out_tx))
                .context("spawn sender-rx thread")?
        };
        // RAII-ish: signal the reader to stop when session() returns.
        struct StopOnDrop(Arc<AtomicBool>);
        impl Drop for StopOnDrop {
            fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); }
        }
        let _stop_guard = StopOnDrop(stop_reader.clone());

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

            // Drain outbound frames from the reader thread (sync_pong
            // replies, future dashboard-triggered emissions). Non-
            // blocking; if the channel is empty we just keep going.
            loop {
                match outbound_rx.try_recv() {
                    Ok(frame) => {
                        if let Err(e) = stream.write_all(&frame) {
                            warn!("sender: outbound write failed: {} — dropping session", e);
                            return Err(e).context("outbound write");
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
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

/// Inbound-frame loop on the streaming TCP socket.
/// Reads u16-prefixed R2-WIRE compact frames, dispatches by event hash.
/// Replies that need to round-trip on the same socket (`sync_pong`) are
/// handed to the writer-thread via `out_tx` instead of written directly
/// — two threads writing to the same socket would race the byte stream.
/// Exits when `stop` is set (writer side has signalled session end) OR
/// when the dashboard closes its half of the socket (EOF).
fn inbound_reader_loop(
    mut stream: TcpStream,
    stop: Arc<AtomicBool>,
    clock: Arc<Clock>,
    out_tx: mpsc::Sender<Vec<u8>>,
) {
    let mut len_buf = [0u8; 2];
    let mut frame_buf = Vec::with_capacity(64);

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // Read the 2-byte length prefix. EAGAIN/Timeout → loop and re-check
        // stop; clean EOF / TCP error → exit.
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) => continue,
            Err(_) => return,
        }
        let frame_len = u16::from_be_bytes(len_buf) as usize;
        if frame_len < 12 || frame_len > 4096 {
            warn!("[sender-rx] suspicious frame length {} — closing reader", frame_len);
            return;
        }
        frame_buf.resize(frame_len, 0);
        if stream.read_exact(&mut frame_buf).is_err() {
            return;
        }
        let Some((event_hash, payload)) = decode_compact_frame(&frame_buf) else {
            continue; // malformed — skip
        };
        match event_hash {
            EVT_DASH_SET_CLOCK_OFFSET => {
                if let Some(delta_ms) = parse_set_clock_offset(payload) {
                    info!("[sender-rx] r2.dash.set_clock_offset delta_ms={:+}", delta_ms);
                    clock.apply_delta(delta_ms);
                } else {
                    warn!("[sender-rx] malformed set_clock_offset payload");
                }
            }
            EVT_DASH_SYNC_PULSE => {
                let Some(req_id) = parse_sync_pulse_req_id(payload) else {
                    warn!("[sender-rx] malformed sync_pulse payload");
                    continue;
                };
                let sensor_ts_ms = clock.ts_ms_i64();
                // r2.sensor.sync_pong payload: {0: req_id, 1: sensor_ts_ms}
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
                if out_tx.send(frame[..n].to_vec()).is_err() {
                    // Writer is gone — session is ending. Reader exits.
                    return;
                }
            }
            other => {
                // Ack, cal-req, etc. land here until later phases wire them up.
                info!("[sender-rx] unhandled event hash 0x{:08X} ({} byte payload)",
                      other, payload.len());
            }
        }
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
