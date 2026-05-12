//! Synthetic accelerometer + battery sample generator.
//!
//! Stand-in for the real ADXL355 + battery divider while soldering is
//! pending. Numbers are shaped like real ADXL355 output (raw 20-bit
//! signed LSB at ±2 g range — 256 000 LSB/g per ADXL355 datasheet) so
//! the dashboard can scale them with the same constants it'll use for
//! the real sensor.

const LSB_PER_G_AT_2G: f32 = 256_000.0;
const TWO_PI: f32 = 6.283_185_307;

/// Synthetic 3-axis accelerometer. Slow rocker-like motion on a primary
/// axis (sine), small lateral oscillation (out-of-phase cosine), gravity
/// constant on the vertical axis. Plenty alive enough to validate
/// dashboard charts end-to-end.
pub struct AccelSim {
    /// Period of the primary rocking cycle, in seconds (e.g. 2.0 = 0.5 Hz).
    pub primary_period_s: f32,
    /// Amplitude of the primary axis, in g.
    pub primary_g: f32,
    /// Amplitude of the lateral component, in g.
    pub lateral_g: f32,
}

impl AccelSim {
    pub fn rocker_default() -> Self {
        Self {
            primary_period_s: 2.0,
            primary_g: 0.4,
            lateral_g: 0.05,
        }
    }

    /// Returns `(x, y, z)` in raw ADXL355 LSB units (i32, sign-extended
    /// from the 20-bit signed value). Caller passes uptime in seconds.
    pub fn sample(&self, t_s: f32) -> (i32, i32, i32) {
        let phase = TWO_PI * (t_s / self.primary_period_s);
        let x_g = self.primary_g * libm::sinf(phase);
        let y_g = self.lateral_g * libm::cosf(phase);
        let z_g = 1.0; // gravity along z, sensor flat.

        let to_lsb = |g: f32| -> i32 { (g * LSB_PER_G_AT_2G) as i32 };
        (to_lsb(x_g), to_lsb(y_g), to_lsb(z_g))
    }
}

/// Synthetic battery — slow linear discharge plus a touch of noise so
/// the dashboard shows a lifelike trace. Resets to full at boot.
pub struct BatterySim {
    pub start_mv: u16,
    pub min_mv: u16,
    /// mV consumed per second of operation. ~ 0.04 mV/s ≈ depletes a
    /// 1000 mV span in ~7 hours, i.e. half-life of a 2000 mAh LiPo.
    pub drain_mv_per_s: f32,
}

impl BatterySim {
    pub fn lipo_default() -> Self {
        Self {
            start_mv: 4150,
            min_mv: 3300,
            drain_mv_per_s: 0.04,
        }
    }

    /// `(voltage_mv, percent)` for the given uptime.
    pub fn sample(&self, t_s: f32) -> (u16, u8) {
        let target = self.start_mv as f32 - self.drain_mv_per_s * t_s;
        let mv = if target < self.min_mv as f32 {
            self.min_mv as f32
        } else {
            target
        };
        let voltage_mv = mv as u16;
        // Crude piecewise: 4200=100, 3300=0 (linear over the working span).
        let percent = if voltage_mv >= 4200 {
            100
        } else if voltage_mv <= 3300 {
            0
        } else {
            (((voltage_mv - 3300) as u32 * 100) / 900) as u8
        };
        (voltage_mv, percent)
    }
}
