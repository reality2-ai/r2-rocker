//! r2-esp — shared ESP-IDF modules for R2 firmware.
//!
//! This crate contains the platform-common code that runs on any ESP32
//! variant (C6, S3, etc.) via ESP-IDF. Board-specific code (pin maps,
//! LCD drivers, SX1262 driver, entry points) stays in the platform
//! directory; this crate holds the transport + identity + provisioning
//! modules that are identical across boards.
//!
//! ## Modules
//!
//! - `ota_tcp` — TCP OTA firmware receive listener (port 21043)
//! - `wifi_sta` — WiFi station mode connection
//! - `wifi_prov` — BLE-based WiFi credential provisioning + NVS persistence
//! - `hive_id` — persistent UUID-based hive identity (R2-WIRE §6.2.1)
//! - `l2cap` — BLE L2CAP CoC server for R2-WIRE event transport (behind `ble` feature)
//! - `beacon` — R2-BEACON BLE legacy advert + scan + peer table (behind `ble` feature)
//!
//! ## Target
//!
//! This crate only compiles for ESP-IDF targets (`xtensa-esp32*-espidf`
//! or `riscv32*-esp-espidf`). It is excluded from the workspace's
//! default build.

pub mod hive_id;
pub mod ota_tcp;
pub mod wifi_sta;
pub mod wifi_prov;

#[cfg(feature = "ble")]
pub mod l2cap;

#[cfg(feature = "ble")]
pub mod beacon;
