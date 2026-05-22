//! Phase 5L — RGB LED state machine.
//!
//! Drives the onboard WS2812 (GPIO38 on DevKitC-1 v1.1) per the colour
//! map in `HARDWARE-WIRING.md` §5 and `SPEC-R2-ROCKER-SENSOR.md` §4.1.
//! The dashboard's virtual LED uses the same `tg-*` colour + animation
//! map (`webapp/index.html`), so the on-screen indicators mirror
//! the physical LED once the firmware emits FSM state on the wire.
//!
//! Calm-tech endpoint: ambient signal, glanceable, no UI noise. The
//! operator can tell at a distance whether the rig is healthy.

use anyhow::{Context, Result};
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::rmt::RmtChannel;
use smart_leds::{SmartLedsWrite, RGB8};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ws2812_esp32_rmt_driver::Ws2812Esp32Rmt;

/// FSM state values — wire-compatible with the dashboard's `ledClassFor()`
/// switch in `webapp/index.html`.
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
pub enum LedState {
    Boot = 0,             // white flash
    Advertising = 1,      // blue, 1 Hz pulse
    BleConnected = 2,     // cyan, fast pulse
    WifiConnecting = 3,   // cyan→yellow flicker (we render same as BleConnected for now)
    StreamingLive = 4,    // green, heartbeat 60 bpm
    StreamingCatchup = 5, // yellow, heartbeat
    Calibrating = 6,      // purple, solid
    LowBattery = 7,       // orange, slow pulse — overlay set via `LedHandle::set_low_battery()`
    Ota = 8,              // white, fast strobe
    Error = 9,            // red, fast pulse
    /// Streaming but with synthetic data — ADXL355 init failed at sender
    /// start; samples come from the simulator. Rhythmically distinct
    /// from `Calibrating` (also purple) by pulse vs solid. See
    /// `SPEC-R2-ROCKER-SENSOR-HEALTH` §4.
    StreamingDegradedSim = 10, // purple, slow pulse 0.5 Hz
    /// Actively recording samples into a named capture file (post-Mark
    /// in SPEC-R2-ROCKER-CAPTURE state machine). Visually distinct from
    /// `StreamingLive` (background heartbeat) — a crisp green tick
    /// every 0.5s so the operator can tell at a glance that the file
    /// is growing, not just that the link is alive.
    Recording = 11,
    /// Operator "identify" overlay — solid white. Driven by the
    /// `identify` AtomicBool in LedHandle; see `set_identify`.
    Identify = 12,
}

impl LedState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Boot,
            1 => Self::Advertising,
            2 => Self::BleConnected,
            3 => Self::WifiConnecting,
            4 => Self::StreamingLive,
            5 => Self::StreamingCatchup,
            6 => Self::Calibrating,
            7 => Self::LowBattery,
            8 => Self::Ota,
            9 => Self::Error,
            10 => Self::StreamingDegradedSim,
            11 => Self::Recording,
            12 => Self::Identify,
            _ => Self::Boot,
        }
    }
}

/// Cheap clonable handle used by the rest of the firmware to push state
/// changes to the LED thread. Reads are lock-free atomics.
#[derive(Clone)]
pub struct LedHandle {
    state: Arc<AtomicU8>,
    low_battery: Arc<AtomicBool>,
    ota: Arc<AtomicBool>,
    /// Operator "identify" overlay — solid white over every other
    /// state. Set / cleared from the streaming-TCP `r2.dash.identify_set`
    /// command (see sender::dispatch_inbound). Highest priority of
    /// any overlay so the device is unambiguously findable in a
    /// busy fleet, even mid-OTA.
    identify: Arc<AtomicBool>,
    /// Auto-revert deadline for the identify overlay, per
    /// `SPEC-R2-ROCKER-SENSOR-IDENTIFY` §3.2. If the dashboard
    /// fails to send `identify_set off` within 60 s of the on
    /// command (e.g. operator forgot, dashboard restarted), the
    /// LED loop clears the overlay so a forgotten Identify
    /// doesn't pin solid white and drain the LiPo.
    identify_deadline: Arc<Mutex<Option<Instant>>>,
    /// Optional synchronised wall-clock source. When set (post
    /// `Clock::load()`), the LED loop computes pulse phases from
    /// `clock.ts_ms_i64()` instead of local Instant::elapsed() — so
    /// every sensor in the rig that has converged on the dashboard's
    /// time-sync offset pulses, heartbeats, and ticks in lockstep. If
    /// unset (early-boot / pre-sync), falls back to local elapsed
    /// time so the LED still animates.
    sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>>,
}

impl LedHandle {
    pub fn set(&self, state: LedState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }
    pub fn current(&self) -> LedState {
        // OTA overlay reports as Ota for the dashboard's status event,
        // so the virtual LED matches the physical one in lockstep.
        if self.ota.load(Ordering::Relaxed) { return LedState::Ota; }
        LedState::from_u8(self.state.load(Ordering::Relaxed))
    }
    /// Low-battery overrides the underlying state colour while set
    /// (slow-pulse orange). The underlying state continues to operate;
    /// this is purely a presentation overlay per spec §4.1.
    pub fn set_low_battery(&self, on: bool) {
        self.low_battery.store(on, Ordering::Relaxed);
    }
    /// OTA overlay — white strobe overrides the underlying state while
    /// the firmware is receiving + writing an image. Driven from the
    /// firmware's main loop polling `r2_esp::ota_tcp::ota_in_progress()`.
    pub fn set_ota(&self, on: bool) {
        self.ota.store(on, Ordering::Relaxed);
    }
    /// Identify overlay — solid white, highest priority. Set from the
    /// dashboard's `r2.dash.identify_set` command so the operator can
    /// pick this specific sensor out of a fleet at a glance.
    pub fn set_identify(&self, on: bool) {
        self.identify.store(on, Ordering::Relaxed);
        if let Ok(mut d) = self.identify_deadline.lock() {
            *d = if on { Some(Instant::now() + Duration::from_secs(60)) } else { None };
        }
    }
    /// Plumb the synchronised clock in once it's loaded. From this
    /// point on all sensors that have converged on the dashboard's
    /// time-sync offset will animate in phase.
    pub fn attach_clock(&self, clock: Arc<crate::clock::Clock>) {
        if let Ok(mut slot) = self.sync_clock.lock() {
            *slot = Some(clock);
        }
    }
}

/// Spawn the LED animator thread. Returns a handle the rest of the
/// firmware uses to push state changes; the thread runs forever.
///
/// `channel` is an RMT channel (e.g. `peripherals.rmt.channel0`);
/// `gpio` is any output-capable pin (DevKitC-1 v1.1: GPIO38).
pub fn start<C, P>(channel: C, gpio: P) -> Result<LedHandle>
where
    C: Peripheral + Send + 'static,
    <C as Peripheral>::P: RmtChannel,
    P: Peripheral + Send + 'static,
    <P as Peripheral>::P: esp_idf_svc::hal::gpio::OutputPin,
{
    let state = Arc::new(AtomicU8::new(LedState::Boot as u8));
    let low_battery = Arc::new(AtomicBool::new(false));
    let ota = Arc::new(AtomicBool::new(false));
    let identify = Arc::new(AtomicBool::new(false));
    let identify_deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>> = Arc::new(Mutex::new(None));

    let state_for_thread = state.clone();
    let low_for_thread = low_battery.clone();
    let ota_for_thread = ota.clone();
    let identify_for_thread = identify.clone();
    let identify_deadline_for_thread = identify_deadline.clone();
    let clock_for_thread = sync_clock.clone();

    std::thread::Builder::new()
        .stack_size(4096)
        .name("led".into())
        .spawn(move || {
            // Build the WS2812 driver inside the thread so the !Send RMT
            // handle stays on a single OS thread for its lifetime.
            let mut led = match Ws2812Esp32Rmt::new(channel, gpio) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("[LED] Ws2812Esp32Rmt::new failed: {e}");
                    return;
                }
            };
            run_led_loop(&mut led, state_for_thread, low_for_thread, ota_for_thread, identify_for_thread, identify_deadline_for_thread, clock_for_thread);
        })
        .context("spawn LED thread")?;

    Ok(LedHandle { state, low_battery, ota, identify, identify_deadline, sync_clock })
}

const FRAME_MS: u64 = 33; // ~30 Hz tick — smooth pulses at low CPU cost
// Slow, calm "idle/waiting" beat — 25 BPM gives a 2.4 s lub-dub
// cycle. Outside an active capture, the sensor really is just
// holding the link open; the LED should match that. Distinct
// from the 0.5 s `Recording` tick at a glance.
const HEARTBEAT_BPM: f32 = 25.0;
/// Global brightness cap applied after `render()`. The DevKitC's onboard
/// WS2812 has no diffuser — at full RGB it's painfully bright on a bare
/// board (0.70 was the bench setting). With the sensor PCB stacked on
/// top of the ESP32, most of the light is now attenuated through the
/// carrier board, so full power gives the operator the right amount of
/// glow at room light. Drop back lower (e.g. 0.15-0.30) for any future
/// deployment where the LED is exposed or sits behind a translucent
/// housing close to the eye.
const BRIGHTNESS: f32 = 1.00;

fn run_led_loop(
    led: &mut Ws2812Esp32Rmt<'_>,
    state: Arc<AtomicU8>,
    low_battery: Arc<AtomicBool>,
    ota: Arc<AtomicBool>,
    identify: Arc<AtomicBool>,
    identify_deadline: Arc<Mutex<Option<Instant>>>,
    sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>>,
) {
    let start = Instant::now();
    loop {
        // Identify auto-revert: if the on-command's 60 s window
        // elapsed without an explicit off, clear the overlay so the
        // LED falls back to the underlying state. Belt-and-braces
        // against a dashboard restart between on and off, per
        // SPEC-R2-ROCKER-SENSOR-IDENTIFY §3.2.
        if let Ok(mut d) = identify_deadline.lock() {
            if let Some(t) = *d {
                if Instant::now() >= t {
                    identify.store(false, Ordering::Relaxed);
                    *d = None;
                }
            }
        }
        let id = identify.load(Ordering::Relaxed);
        let s = if id {
            // Highest-priority overlay — operator is trying to find
            // this sensor in the fleet. Solid white wins over
            // everything else (state, OTA, low_battery).
            LedState::Identify
        } else if ota.load(Ordering::Relaxed) {
            LedState::Ota
        } else {
            LedState::from_u8(state.load(Ordering::Relaxed))
        };
        let lb = low_battery.load(Ordering::Relaxed) && !id;
        // Phase source: prefer the synchronised wall clock once Clock
        // has been plumbed in (post-`Clock::load()` in main.rs). All
        // sensors converged on the dashboard's time-sync offset will
        // see the same `epoch_ms`, so their pulse / heartbeat / tick
        // animations stay in lockstep across the rig. Before sync, or
        // if the lock fails, fall back to local Instant::elapsed so
        // the LED still animates.
        //
        // Reduce the epoch ms into a 60-second window before handing
        // it to render(). Absolute epoch ms in 2026 is ~1.78e12 — well
        // beyond f32's ~7-decimal precision, so `as_secs_f32()` /
        // `.fract()` / `sin()` lose all sub-second precision and the
        // pulse collapses to a constant value. All pulse periods we
        // use (0.4 / 0.5 / 1.0 / 1.5 / 2.0 / 2.4 s) divide 60 s
        // evenly, so the modulo is invisible at the period boundary;
        // the only state that doesn't divide cleanly is the 0.18 s
        // OTA strobe, which is transient and tolerable to a once-a-
        // minute hiccup. Sync survives because every sensor applies
        // the same modulo to the same `epoch_ms`.
        const PHASE_WINDOW_MS: i64 = 60_000;
        let elapsed = match sync_clock.lock().ok().and_then(|g| g.clone()) {
            Some(clock) => {
                let ms = clock.ts_ms_i64();
                if ms > 0 {
                    Duration::from_millis(ms.rem_euclid(PHASE_WINDOW_MS) as u64)
                } else {
                    start.elapsed()
                }
            }
            None => start.elapsed(),
        };
        let colour = scale(render(s, lb, elapsed), BRIGHTNESS);
        if let Err(e) = led.write(std::iter::once(colour)) {
            log::warn!("[LED] write failed: {e}");
        }
        std::thread::sleep(Duration::from_millis(FRAME_MS));
    }
}

/// Map `(state, low_battery, elapsed)` → an RGB8 colour for this frame.
/// All animation maths lives here; the IO loop above is dumb.
fn render(state: LedState, low_battery: bool, elapsed: Duration) -> RGB8 {
    // Low-battery overlay wins per spec §4.1 (slow 1.5 s pulse, orange).
    if low_battery {
        return scale(rgb(0xFF, 0x80, 0x00), pulse(elapsed, 1.5));
    }

    match state {
        // 100 ms white flash on cold boot, then dark until ADVERTISING.
        LedState::Boot => {
            if elapsed < Duration::from_millis(100) {
                rgb(0x40, 0x40, 0x40) // dimmed white — full white draws ~60 mA
            } else {
                rgb(0, 0, 0)
            }
        }
        LedState::Advertising  => scale(rgb(0x00, 0x00, 0xFF), pulse(elapsed, 1.0)),
        LedState::BleConnected => scale(rgb(0x00, 0xC0, 0xC0), pulse(elapsed, 0.4)),
        // Phase 5L v0.1: render WiFi-connecting same as BLE-connected. The
        // spec calls for cyan→yellow flicker; needs a two-colour blend
        // we can add when we see how the colour reads against the rig.
        LedState::WifiConnecting => scale(rgb(0x00, 0xC0, 0xC0), pulse(elapsed, 0.4)),
        LedState::StreamingLive    => scale(rgb(0x00, 0xC0, 0x20), heartbeat(elapsed, HEARTBEAT_BPM)),
        LedState::StreamingCatchup => scale(rgb(0xFF, 0xCC, 0x00), heartbeat(elapsed, HEARTBEAT_BPM)),
        LedState::Calibrating => rgb(0x80, 0x00, 0xC0), // purple, solid
        LedState::LowBattery  => scale(rgb(0xFF, 0x80, 0x00), pulse(elapsed, 1.5)),
        LedState::Ota         => strobe(rgb(0x40, 0x40, 0x40), elapsed, 0.18),
        LedState::Error       => scale(rgb(0xFF, 0x00, 0x00), pulse(elapsed, 0.25)),
        LedState::StreamingDegradedSim => scale(rgb(0x80, 0x00, 0xC0), pulse(elapsed, 2.0)),
        // Single crisp green tick every 0.5s — a short gaussian bump
        // at the start of each half-second window, then dark for the
        // rest. Distinguishable at a glance from `StreamingLive`'s
        // gentle heartbeat: "actively recording" vs "link alive".
        LedState::Recording => scale(rgb(0x00, 0xE0, 0x30), single_tick(elapsed, 0.5)),
        // Solid white — the operator pressed the Identify toggle on
        // the Devices tab. Unambiguous: pick THIS sensor out of the
        // rack. BRIGHTNESS cap applies as usual.
        LedState::Identify => rgb(0xFF, 0xFF, 0xFF),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> RGB8 { RGB8 { r, g, b } }

/// Scale an RGB triple by 0.0..=1.0. Avoids float-style hue distortion
/// at low brightness because we just multiply each channel uniformly.
fn scale(c: RGB8, k: f32) -> RGB8 {
    let k = k.clamp(0.0, 1.0);
    RGB8 {
        r: (c.r as f32 * k) as u8,
        g: (c.g as f32 * k) as u8,
        b: (c.b as f32 * k) as u8,
    }
}

/// Smooth sinusoidal pulse 0..=1 with period `period_secs`.
fn pulse(t: Duration, period_secs: f32) -> f32 {
    let phase = t.as_secs_f32() / period_secs;
    let s = (phase * core::f32::consts::TAU).sin(); // -1..=1
    0.5 + 0.5 * s                                   // 0..=1
}

/// Two-beat heartbeat: a quick lub-dub each `60 / bpm` seconds.
fn heartbeat(t: Duration, bpm: f32) -> f32 {
    let period = 60.0 / bpm;
    let phase = (t.as_secs_f32() / period).fract();
    // Two narrow gaussian-ish bumps within the period (lub at 0.0, dub at 0.18).
    let b1 = (-((phase - 0.00) * 14.0).powi(2)).exp();
    let b2 = (-((phase - 0.18) * 14.0).powi(2)).exp() * 0.7;
    (b1 + b2).clamp(0.0, 1.0)
}

/// Square-wave strobe: full colour vs off, 50 % duty.
fn strobe(c: RGB8, t: Duration, period_secs: f32) -> RGB8 {
    let phase = (t.as_secs_f32() / period_secs).fract();
    if phase < 0.5 { c } else { rgb(0, 0, 0) }
}

/// Single narrow tick per `period_secs`: a gaussian bump at the start
/// of the period (~5 % in) that decays to ~0 within ~15 % of the
/// period, leaving the rest dark. Used by `LedState::Recording` for an
/// unmistakable "something is happening" beat without being noisy.
fn single_tick(t: Duration, period_secs: f32) -> f32 {
    let phase = (t.as_secs_f32() / period_secs).fract();
    (-((phase - 0.05) * 14.0).powi(2)).exp()
}
