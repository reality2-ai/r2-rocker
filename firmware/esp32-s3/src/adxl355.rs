//! ADXL355 SPI driver.
//!
//! Phase 2 driver — takes ownership of SPI2 + the four wired pins,
//! does the boot self-test (WHO_AM_I), clears standby, and exposes
//! `read_xyz_lsb()` for synchronous sample reads. The sender thread
//! polls it at the sample-rate tick; ODR is left at the chip's
//! power-on default (4 kHz) so each poll sees the most recent value
//! the analog chain has produced.
//!
//! Wiring per `HARDWARE-WIRING.md` §2.1:
//!
//! | Signal | DevKitC pin | ADXL355-PMDZ pin |
//! |---|---|---|
//! | CS   | GPIO10 | Pmod pin 1 |
//! | MOSI | GPIO11 | Pmod pin 2 |
//! | MISO | GPIO13 | Pmod pin 3 |
//! | SCLK | GPIO12 | Pmod pin 4 |
//! | GND  | —      | Pmod pin 5 (or 11) |
//! | VDD  | 3V3    | Pmod pin 6 (or 12) |
//!
//! Per the ADXL355 datasheet §10 (SPI bus), each access starts with a
//! single command byte: `(register_address << 1) | R/W`, where R/W is
//! `0x01` for read and `0x00` for write. Multi-byte reads auto-
//! increment the register address inside the chip, so the 3-axis
//! sample read is a single 10-byte transaction.
//!
//! 20-bit signed samples are stored left-aligned across 3 bytes per
//! axis (bits 23..4 = sample, bits 3..0 = reserved zeros, big-endian).
//! `decode_20bit_signed` handles the shift + sign-extend.

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi::{
    config::{Config as SpiConfig, DriverConfig as SpiDriverConfig},
    SpiDeviceDriver, SpiDriver, SPI2,
};
use log::info;

/// Expected identification-register values (ADXL355 datasheet §11).
pub const EXPECTED_DEVID_AD:  u8 = 0xAD;
pub const EXPECTED_DEVID_MST: u8 = 0x1D;
pub const EXPECTED_PARTID:    u8 = 0xED;

// Register addresses (datasheet §11).
const REG_DEVID_AD:  u8 = 0x00;
const REG_XDATA3:    u8 = 0x08; // X high byte; auto-increment runs through Z low at 0x10
const REG_POWER_CTL: u8 = 0x2D;

// POWER_CTL bits.
const POWER_CTL_MEASURE_MODE: u8 = 0x00; // bit 0 = 0 → measurement; default is 1 (standby)

/// Owns SPI2 + the four wired pins for the chip's lifetime. The struct
/// is constructed inside the sender thread (SPI device drivers are not
/// `Send`) and lives for the program's duration.
pub struct Adxl355<'d> {
    dev: SpiDeviceDriver<'d, SpiDriver<'d>>,
}

impl<'d> Adxl355<'d> {
    /// Initialise SPI2 with the wired pins, verify the identification
    /// registers, and clear standby. Returns a usable driver instance
    /// on success.
    pub fn new<SCLK, MOSI, MISO, CS>(
        spi2: SPI2,
        sclk: SCLK,
        mosi: MOSI,
        miso: MISO,
        cs:   CS,
    ) -> Result<Self>
    where
        SCLK: Peripheral<P = AnyIOPin> + 'd,
        MOSI: Peripheral<P = AnyIOPin> + 'd,
        MISO: Peripheral<P = AnyIOPin> + 'd,
        CS:   Peripheral<P = AnyIOPin> + 'd,
    {
        info!(
            "[ADXL355] init SPI2 (sclk=GPIO12, mosi=GPIO11, miso=GPIO13, cs=GPIO10, mode 0, 5 MHz)"
        );

        let dev = SpiDeviceDriver::new_single(
            spi2,
            sclk,
            mosi,
            Some(miso),
            Some(cs),
            &SpiDriverConfig::new(),
            // Datasheet §10 max f_SCK = 10 MHz; 5 MHz is comfortable.
            // Mode 0 (CPOL=0, CPHA=0) is the part's default.
            &SpiConfig::new().baudrate(5.MHz().into()),
        )
        .context("SpiDeviceDriver::new_single")?;

        let mut adxl = Self { dev };

        // Boot self-test (datasheet §11 / SPEC-R2-ROCKER-SENSOR §2.1 step 6).
        let (a, m, p) = adxl.read_who_am_i()?;
        info!(
            "[ADXL355] DEVID_AD=0x{:02X} DEVID_MST=0x{:02X} PARTID=0x{:02X}",
            a, m, p
        );
        if a != EXPECTED_DEVID_AD || m != EXPECTED_DEVID_MST || p != EXPECTED_PARTID {
            return Err(anyhow!(
                "WHO_AM_I mismatch — expected 0xAD/0x1D/0xED, got 0x{:02X}/0x{:02X}/0x{:02X}",
                a, m, p
            ));
        }
        info!("[ADXL355] all IDs match expected values — chip enumerates ✓");

        // Clear standby — POWER_CTL bit 0 = 0 puts the chip into
        // measurement mode. Default ODR (4 kHz) and range (±2 g) are
        // fine for v0.1; tuning ODR + filter happens once we trust the
        // data path. Range stays at ±2 g per LSB_PER_G_AT_2G = 256000
        // (SPEC-R2-ROCKER-WIRE §4.1).
        adxl.write_reg(REG_POWER_CTL, POWER_CTL_MEASURE_MODE)?;
        info!("[ADXL355] POWER_CTL cleared — measurement mode active");

        Ok(adxl)
    }

    /// Read the three identification registers in one auto-incrementing
    /// transaction. Used in `new()` for self-test; exposed for
    /// diagnostic use.
    pub fn read_who_am_i(&mut self) -> Result<(u8, u8, u8)> {
        let mut buf = [(REG_DEVID_AD << 1) | 0x01, 0, 0, 0];
        self.dev.transfer_in_place(&mut buf)
            .context("ADXL355 SPI transfer (read_who_am_i)")?;
        Ok((buf[1], buf[2], buf[3]))
    }

    /// Write one register. The R/W bit is 0 (write) — different from
    /// most parts, where R/W lives in the high bit.
    fn write_reg(&mut self, reg: u8, val: u8) -> Result<()> {
        let mut buf = [(reg << 1) & !0x01, val];
        self.dev.transfer_in_place(&mut buf)
            .with_context(|| format!("ADXL355 SPI transfer (write_reg 0x{:02X})", reg))?;
        Ok(())
    }

    /// Read X / Y / Z as signed 20-bit LSB values in one transaction.
    /// Units: LSB at the chip's currently-active range. For the
    /// power-on default (±2 g), 1 g = 256_000 LSB (datasheet §6).
    pub fn read_xyz_lsb(&mut self) -> Result<(i32, i32, i32)> {
        // 1 command byte + 9 data bytes (3 axes × 3 bytes each).
        let mut buf = [0u8; 10];
        buf[0] = (REG_XDATA3 << 1) | 0x01;
        self.dev.transfer_in_place(&mut buf)
            .context("ADXL355 SPI transfer (read_xyz_lsb)")?;
        let x = decode_20bit_signed(&buf[1..4]);
        let y = decode_20bit_signed(&buf[4..7]);
        let z = decode_20bit_signed(&buf[7..10]);
        Ok((x, y, z))
    }
}

/// Decode a 20-bit signed integer stored left-aligned across 3 bytes
/// (big-endian, MSB-first; low 4 bits of the third byte are reserved
/// zeros per datasheet §11). Returns a sign-extended `i32`.
fn decode_20bit_signed(b: &[u8]) -> i32 {
    let raw24 = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
    let raw20 = raw24 >> 4;
    let sign_bit = 1u32 << 19;
    if (raw20 & sign_bit) != 0 {
        (raw20 | 0xFFF0_0000) as i32
    } else {
        raw20 as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_zero() {
        assert_eq!(decode_20bit_signed(&[0x00, 0x00, 0x00]), 0);
    }

    #[test]
    fn decode_positive_max() {
        // 0x7FFFF0 in the 3 bytes → raw20 = 0x7FFFF (524287)
        assert_eq!(decode_20bit_signed(&[0x7F, 0xFF, 0xF0]), 524287);
    }

    #[test]
    fn decode_negative_one() {
        // 0xFFFFF0 → raw20 = 0xFFFFF → sign-extended = -1
        assert_eq!(decode_20bit_signed(&[0xFF, 0xFF, 0xF0]), -1);
    }

    #[test]
    fn decode_one_g_at_2g_range() {
        // 1 g at ±2 g range = 256_000 LSB = 0x3E800 → left-shifted into
        // 0x3E_8000 → bytes [0x03, 0xE8, 0x00]
        assert_eq!(decode_20bit_signed(&[0x03, 0xE8, 0x00]), 256_000);
    }
}
