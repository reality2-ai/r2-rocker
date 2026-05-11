//! ADXL355 SPI smoke-test driver.
//!
//! Phase 2 first cut: read DEVID_AD / DEVID_MST / PARTID over SPI to
//! prove the chip enumerates on the bus. Replace with a full driver
//! (range config, sample readout, FIFO drain) once the smoke test
//! passes — this module owns SPI2 today and will own it tomorrow.
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
//! `0x01` for read and `0x00` for write. Multi-byte reads auto-increment
//! the register address inside the chip, so we read three consecutive
//! ID registers in one transaction.

use anyhow::{Context, Result};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi::{
    config::{Config as SpiConfig, DriverConfig as SpiDriverConfig},
    SpiDeviceDriver, SpiDriver, SPI2,
};
use log::{error, info};

/// Expected identification-register values (ADXL355 datasheet §11).
pub const EXPECTED_DEVID_AD:  u8 = 0xAD;
pub const EXPECTED_DEVID_MST: u8 = 0x1D;
pub const EXPECTED_PARTID:    u8 = 0xED;

const REG_DEVID_AD: u8 = 0x00;

/// Snapshot of the three identification registers.
#[derive(Clone, Copy, Debug)]
pub struct WhoAmI {
    pub devid_ad:  u8,
    pub devid_mst: u8,
    pub partid:    u8,
}

impl WhoAmI {
    pub fn matches(&self) -> bool {
        self.devid_ad  == EXPECTED_DEVID_AD
            && self.devid_mst == EXPECTED_DEVID_MST
            && self.partid    == EXPECTED_PARTID
    }
}

/// Bring up SPI2 for the ADXL355 wiring and read the three
/// identification registers in one auto-incrementing transaction.
/// Logs the result; caller decides whether to halt or continue on
/// a mismatch (we currently continue, so WiFi + TCP still come up
/// for further debug from the dashboard side).
pub fn smoke_test_who_am_i<SCLK, MOSI, MISO, CS>(
    spi2: SPI2,
    sclk: SCLK,
    mosi: MOSI,
    miso: MISO,
    cs:   CS,
) -> Result<WhoAmI>
where
    SCLK: Peripheral<P = AnyIOPin> + 'static,
    MOSI: Peripheral<P = AnyIOPin> + 'static,
    MISO: Peripheral<P = AnyIOPin> + 'static,
    CS:   Peripheral<P = AnyIOPin> + 'static,
{
    info!(
        "[ADXL355] init SPI2 (sclk=GPIO12, mosi=GPIO11, miso=GPIO13, cs=GPIO10, mode 0, 5 MHz)"
    );

    let driver = SpiDriver::new::<SPI2>(
        spi2,
        sclk,
        mosi,
        Some(miso),
        &SpiDriverConfig::new(),
    )
    .context("ADXL355 SpiDriver::new(SPI2)")?;

    let mut dev = SpiDeviceDriver::new(
        driver,
        Some(cs),
        // Datasheet §10 max f_SCK = 10 MHz; 5 MHz is comfortable.
        // Mode 0: CPOL=0, CPHA=0.
        &SpiConfig::new().baudrate(5.MHz().into()),
    )
    .context("ADXL355 SpiDeviceDriver::new")?;

    // Build the transfer buffer: 1 command byte + 3 dummy bytes to clock
    // out DEVID_AD / DEVID_MST / PARTID (auto-increment from REG_DEVID_AD).
    let mut buf = [(REG_DEVID_AD << 1) | 0x01, 0, 0, 0];
    dev.transfer_in_place(&mut buf)
        .context("ADXL355 SPI transfer_in_place")?;

    let who = WhoAmI {
        devid_ad:  buf[1],
        devid_mst: buf[2],
        partid:    buf[3],
    };

    info!(
        "[ADXL355] DEVID_AD=0x{:02X} DEVID_MST=0x{:02X} PARTID=0x{:02X}",
        who.devid_ad, who.devid_mst, who.partid,
    );
    if who.matches() {
        info!("[ADXL355] all IDs match expected values — chip enumerates ✓");
    } else {
        error!(
            "[ADXL355] WHO_AM_I MISMATCH — expected 0xAD/0x1D/0xED, got 0x{:02X}/0x{:02X}/0x{:02X}. \
             Check SPI wiring + Pmod pin-1 orientation + Pmod power (3V3 not 5V).",
            who.devid_ad, who.devid_mst, who.partid,
        );
    }

    Ok(who)
}
