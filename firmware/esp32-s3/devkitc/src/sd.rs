//! microSD bring-up on the shared SPI2 bus (SPEC-R2-ROCKER-SENSOR §6.1).
//!
//! Mount semantics for v0.1:
//!   * Initialise an SD-over-SPI device on the existing `SpiDriver`
//!     (shared with the ADXL355, distinct CS line per wiring spec
//!     `HARDWARE-WIRING-DEVKITC.md` §3.2).
//!   * Mount FAT at `/sdcard`. Auto-format if there's no FS on the card
//!     so a brand-new card just works on first boot.
//!   * Write a 0-byte probe file `/sdcard/.r2-mounted` to confirm the
//!     filesystem is actually writable.
//!
//! Graceful failure: any step that fails (no card inserted, SPI/SD
//! init error, mount error, probe-file error) returns `None` from
//! `try_mount`. The caller treats that as "no durability available"
//! and continues streaming-only — the sensor stays useful even with
//! the SD path absent.
//!
//! Phase-2 ring writer (§6.2 onwards) is a separate module that uses
//! the mount this provides; this file just brings the FS up.

use esp_idf_svc::fs::fatfs::Fatfs;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::sd::{spi::SdSpiHostDriver, SdCardConfiguration, SdCardDriver};
use esp_idf_svc::hal::spi::SpiDriver;
use esp_idf_svc::io::vfs::MountedFatfs;
use log::{info, warn};
use std::sync::Arc;


/// Mount point — every consumer of the SD ring (§6.1 onwards) opens
/// files under `<MOUNT_POINT>/r2/log.NNNN.bin`.
pub const MOUNT_POINT: &str = "/sdcard";

type SharedBus = Arc<SpiDriver<'static>>;
type SdHost = SdSpiHostDriver<'static, SharedBus>;
type SdDrv = SdCardDriver<SdHost>;
type SdFs = Fatfs<SdDrv>;
type SdMount = MountedFatfs<SdFs>;

/// Mounted SD card. Drop unmounts the filesystem.
pub struct SdCard {
    /// Mount guard — keep this alive for the lifetime of the SD path.
    _mount: SdMount,
}

impl SdCard {
    /// Best-effort SD mount on the shared bus. Returns `Some(SdCard)` on
    /// success, `None` on any failure (no card, mount error, no
    /// writable filesystem). All failures are warned to the log; the
    /// caller proceeds without durability.
    pub fn try_mount<CS>(bus: SharedBus, cs: CS) -> Option<Self>
    where
        CS: Peripheral<P = AnyIOPin> + 'static,
    {
        info!("[SD] attempting mount on shared SPI2 bus");

        // 1. SDSPI host on the shared bus + dedicated CS.
        let host = match SdSpiHostDriver::new(
            bus,
            Some(cs),
            AnyIOPin::none(),
            AnyIOPin::none(),
            AnyIOPin::none(),
            // ESP-IDF v5.2+: wp_active_high. We have no WP pin.
            #[cfg(not(any(
                esp_idf_version_major = "4",
                all(esp_idf_version_major = "5", esp_idf_version_minor = "0"),
                all(esp_idf_version_major = "5", esp_idf_version_minor = "1"),
            )))]
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                warn!("[SD] SdSpiHostDriver::new failed: {e} — no durability");
                return None;
            }
        };

        // 2. Probe the card. This is where "no card inserted" surfaces.
        //    Diagnostic config: slow 400 kHz init bus (rules out signal-
        //    integrity issues on T-junctioned MOSI/MISO/SCK), generous
        //    10 s command timeout in case the card is sluggish on its
        //    first CMD0 after power-on.
        let mut sd_cfg = SdCardConfiguration::new();
        sd_cfg.speed_khz = 400;
        sd_cfg.command_timeout_ms = 10_000;
        info!(
            "[SD] SdCardDriver::new_spi (speed_khz={}, command_timeout_ms={})",
            sd_cfg.speed_khz, sd_cfg.command_timeout_ms
        );
        let card = match SdCardDriver::new_spi(host, &sd_cfg) {
            Ok(c) => c,
            Err(e) => {
                warn!("[SD] SdCardDriver::new_spi failed: {e} — no card or bad SPI");
                return None;
            }
        };

        // 3. FATFS instance on this SD card.
        let fatfs = match Fatfs::new_sdcard(0, card) {
            Ok(f) => f,
            Err(e) => {
                warn!("[SD] Fatfs::new_sdcard failed: {e}");
                return None;
            }
        };

        // 4. VFS mount at /sdcard. The mount call internally registers
        //    with esp_vfs_fat_register and the standard `std::fs` works
        //    against this path afterwards.
        let mount = match MountedFatfs::mount(fatfs, MOUNT_POINT, 4) {
            Ok(m) => m,
            Err(e) => {
                // Most common cause: card has no FAT filesystem.
                // v0.1 just logs and bails — operator pre-formats the
                // card (mkfs.vfat -F 32 -s 64 / SD Memory Card
                // Formatter). Auto-format support is a follow-up.
                warn!(
                    "[SD] MountedFatfs::mount failed: {e} \
                     — is the card formatted FAT32? (`mkfs.vfat -F 32`)"
                );
                return None;
            }
        };

        // 5. Writable-FS probe. Tries to create a zero-byte file so a
        //    read-only or otherwise-degraded mount fails loudly here.
        //    Filename must not start with `.` — ESP-IDF's FATFS layer
        //    rejects dotfile names with EINVAL even though FAT itself
        //    has no notion of dotfiles. Use a plain name.
        let probe = format!("{}/r2-probe.tmp", MOUNT_POINT);
        match std::fs::File::create(&probe) {
            Ok(_) => {
                info!("[SD] mounted at {} (probe write ok)", MOUNT_POINT);
                // Clean up; probe file isn't useful after this point.
                let _ = std::fs::remove_file(&probe);
            }
            Err(e) => {
                warn!("[SD] mounted but probe write to {probe} failed: {e}");
                return None;
            }
        }

        Some(Self { _mount: mount })
    }
}
