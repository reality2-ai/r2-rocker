//! Real battery telemetry — ADC1_CH3 on GPIO 4, divider-fed.
//!
//! Per SPEC-R2-ROCKER-SENSOR §8 and HARDWARE-WIRING-DEVKITC §4.2:
//!   * Two 100 kΩ resistors form a 0.5 divider from VBATT to GND.
//!   * The midpoint goes to GPIO 4 (ADC1_CH3).
//!   * Sample at 12-bit / 12 dB attenuation (esp-idf v5 spelling
//!     `DB_11` is the same value — the chip's full-scale on this
//!     attenuation setting is ~3.1 V).
//!   * Each reading is the median of 16 successive samples to reject
//!     ADC noise.
//!   * Curve-fitting (two-point) calibration is applied so the mV
//!     reading is corrected for the chip's per-unit ADC offset.
//!   * `v_cell_mv = adc_mv × 2` (divider ratio inverse).
//!
//! If ADC init fails (boards without the divider fitted, or a flaky
//! ADC initialisation), we fall back to `BatterySim` so the sender
//! still emits sensible-looking telemetry — the dashboard treats
//! `r2.sensor.battery` as a routine periodic event and a missing
//! battery feed would look like the sensor is broken.

use crate::sim::BatterySim;
use esp_idf_svc::hal::adc::attenuation;
use esp_idf_svc::hal::adc::oneshot::{
    config::{AdcChannelConfig, Calibration},
    AdcChannelDriver, AdcDriver,
};
use esp_idf_svc::hal::adc::Resolution;
use esp_idf_svc::hal::gpio::Gpio4;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::adc::ADC1;
use log::{info, warn};
use std::time::Instant;

/// Number of raw ADC samples to median-filter per reading.
/// Per SPEC §8.1 — rejects single-cycle ADC noise spikes.
const SAMPLES_PER_READING: usize = 16;

/// Plausibility window (post-scaling, at the cell): anything outside
/// this band can't be a real single-cell LiPo and is almost certainly
/// the ADC reading a floating GPIO 4 (board with no divider fitted) or
/// the under-charged S/H of a high-impedance divider with no bypass
/// cap. Treated as "no usable battery sense" → fall back to BatterySim.
const PLAUSIBLE_MV_MIN: u16 = 2500;
const PLAUSIBLE_MV_MAX: u16 = 4500;

/// Maximum sample-to-sample spread (max − min) within one 16-sample
/// reading, in calibrated mV. A stable, properly-bypassed divider
/// produces spreads in the tens of mV at most. Anything larger is
/// the ADC S/H failing to acquire (high source impedance / no cap)
/// and the median we'd return would be a confidently-wrong number.
const PLAUSIBLE_SPREAD_MV: u16 = 100;

type Channel = AdcChannelDriver<'static, Gpio4, AdcDriver<'static, ADC1>>;

pub struct Battery {
    chan: Option<Channel>,
    /// Always present — used when the ADC channel is `None`, OR when
    /// a single ADC read errors mid-flight, OR when the read fails
    /// the plausibility / variance check below.
    sim: BatterySim,
    boot: Instant,
    /// Set true the first time a real ADC read passes the plausibility
    /// + variance gates; once we know the divider is real and stable,
    /// we trust subsequent reads even if a single one wobbles. Cleared
    /// only by reboot. Latching this way avoids the LED flapping
    /// between LOW_BATTERY (sim drifting down) and a real cell reading
    /// if a single sample blip clears the gate.
    real_seen: bool,
    /// Counter so we don't spam the log with "implausible" warnings
    /// every BATTERY_PERIOD_MS forever — first one's loud, then quiet.
    implausible_warns: u32,
}

impl Battery {
    /// Build a Battery reader that prefers the real ADC. Falls back
    /// to `BatterySim` if either the AdcDriver or AdcChannelDriver
    /// fails to initialise.
    pub fn new(
        adc1: impl Peripheral<P = ADC1> + 'static,
        gpio4: impl Peripheral<P = Gpio4> + 'static,
    ) -> Self {
        let chan = build_channel(adc1, gpio4);
        if chan.is_some() {
            info!("[battery] ADC1_CH3 (GPIO 4) initialised — real telemetry");
        } else {
            warn!("[battery] ADC init failed — falling back to BatterySim");
        }
        Self {
            chan,
            sim: BatterySim::lipo_default(),
            boot: Instant::now(),
            real_seen: false,
            implausible_warns: 0,
        }
    }

    /// Build a sim-only Battery — useful for carriers whose wiring
    /// spec hasn't yet allocated a battery-sense pin (currently the
    /// XIAO carrier per HARDWARE-WIRING-XIAO.md). Drop-in replaceable
    /// with `new()` once the divider is wired and the spec updated.
    #[allow(dead_code)]
    pub fn sim_only() -> Self {
        info!("[battery] sim-only mode — no ADC channel configured for this carrier");
        Self {
            chan: None,
            sim: BatterySim::lipo_default(),
            boot: Instant::now(),
            real_seen: false,
            implausible_warns: 0,
        }
    }

    /// One (voltage_mv, percent) reading. Same signature as
    /// `BatterySim::sample` so the sender doesn't care which is
    /// driving the feed. The plausibility + variance gates decide
    /// whether a given ADC reading is trustworthy:
    ///
    /// * **No usable divider** (floating GPIO 4 on a board not fitted
    ///   with a divider): consecutive ADC samples vary wildly →
    ///   variance check fails → sim.
    /// * **High-impedance divider, no bypass cap**: ADC S/H
    ///   under-charges, samples scatter → variance check fails → sim.
    /// * **Working divider with cap**: tight spread, plausible cell
    ///   range → real reading.
    ///
    /// Once a real reading has passed the gate (`real_seen`) we trust
    /// further reads even if a single one wobbles — that way a brief
    /// noise spike doesn't visually flap LED state between sim drift
    /// and live cell voltage.
    pub fn sample(&mut self) -> (u16, u8) {
        if let Some(chan) = self.chan.as_mut() {
            if let Some(reading) = read_with_spread(chan) {
                let cell_mv = reading.median.saturating_mul(2);
                let plausible = cell_mv >= PLAUSIBLE_MV_MIN
                    && cell_mv <= PLAUSIBLE_MV_MAX;
                let stable = reading.spread <= PLAUSIBLE_SPREAD_MV;
                if (plausible && stable) || self.real_seen {
                    if !self.real_seen {
                        info!(
                            "[battery] real telemetry locked in — cell_mv={} (spread={} mV)",
                            cell_mv, reading.spread,
                        );
                        self.real_seen = true;
                    }
                    return (cell_mv, percent_for_mv(cell_mv));
                }
                // First-time gate failure — log loudly so the operator
                // knows the divider hardware isn't reading reliably.
                // After 3 warns, fall silent.
                if self.implausible_warns < 3 {
                    warn!(
                        "[battery] ADC reading rejected — cell_mv={} (need {}..{}), spread={} mV (need ≤ {}). \
                         Falling back to BatterySim. \
                         Likely cause: no divider fitted, or 100 nF cap missing on the divider midpoint.",
                        cell_mv, PLAUSIBLE_MV_MIN, PLAUSIBLE_MV_MAX,
                        reading.spread, PLAUSIBLE_SPREAD_MV,
                    );
                    self.implausible_warns += 1;
                }
            }
        }
        let t_s = self.boot.elapsed().as_secs_f32();
        self.sim.sample(t_s)
    }
}

fn build_channel(
    adc1: impl Peripheral<P = ADC1> + 'static,
    gpio4: impl Peripheral<P = Gpio4> + 'static,
) -> Option<Channel> {
    let driver = match AdcDriver::new(adc1) {
        Ok(d) => d,
        Err(e) => { warn!("[battery] AdcDriver::new failed: {e}"); return None; }
    };
    let cfg = AdcChannelConfig {
        attenuation: attenuation::DB_11,
        resolution:  Resolution::Resolution12Bit,
        calibration: Calibration::Curve,
    };
    match AdcChannelDriver::new(driver, gpio4, &cfg) {
        Ok(c) => Some(c),
        Err(e) => { warn!("[battery] AdcChannelDriver::new failed: {e}"); None }
    }
}

struct AdcReading {
    median: u16,
    spread: u16, // max − min in calibrated mV across the 16 samples
}

/// 16-sample read returning both median and spread (so the caller can
/// decide whether the ADC actually acquired a stable source). `None`
/// if any sample errored. Spread is the discriminator between
/// "high-impedance divider with no bypass" (wide scatter) and "real
/// cell on a proper divider" (tight cluster).
fn read_with_spread(chan: &mut Channel) -> Option<AdcReading> {
    let mut samples = [0u16; SAMPLES_PER_READING];
    for i in 0..SAMPLES_PER_READING {
        match chan.read() {
            Ok(mv) => samples[i] = mv,
            Err(e) => { warn!("[battery] chan.read failed: {e}"); return None; }
        }
    }
    samples.sort_unstable();
    let median = samples[SAMPLES_PER_READING / 2];
    let spread = samples[SAMPLES_PER_READING - 1].saturating_sub(samples[0]);
    Some(AdcReading { median, spread })
}

/// Piecewise-linear state-of-charge curve per SPEC-R2-ROCKER-SENSOR §8.3.
fn percent_for_mv(mv: u16) -> u8 {
    // Anchor points (mV, percent), monotonically increasing in mV.
    const PTS: &[(u16, u8)] = &[
        (3300, 0), (3400, 5),  (3500, 10), (3600, 20),
        (3700, 35),(3800, 50), (3900, 65), (4000, 80),
        (4100, 90),(4200, 100),
    ];
    if mv <= PTS[0].0 { return 0; }
    if mv >= PTS[PTS.len()-1].0 { return 100; }
    for w in PTS.windows(2) {
        let (lo_mv, lo_pct) = w[0];
        let (hi_mv, hi_pct) = w[1];
        if mv >= lo_mv && mv <= hi_mv {
            let span_mv  = (hi_mv - lo_mv) as u32;
            let span_pct = (hi_pct - lo_pct) as u32;
            let into     = (mv - lo_mv) as u32;
            return (lo_pct as u32 + into * span_pct / span_mv) as u8;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn curve_endpoints() {
        assert_eq!(percent_for_mv(3000), 0);
        assert_eq!(percent_for_mv(3300), 0);
        assert_eq!(percent_for_mv(4200), 100);
        assert_eq!(percent_for_mv(5000), 100);
    }
    #[test] fn curve_anchors() {
        assert_eq!(percent_for_mv(3700), 35);
        assert_eq!(percent_for_mv(3800), 50);
        assert_eq!(percent_for_mv(4100), 90);
    }
    #[test] fn curve_interpolates() {
        // Halfway between 3700 (35) and 3800 (50) → ~42 (rounds down).
        assert_eq!(percent_for_mv(3750), 42);
    }
}
