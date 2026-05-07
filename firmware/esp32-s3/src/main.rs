//! r2-rocker firmware — Phase 0.5 + simulated sender.
//!
//! Boots, brings up WiFi, connects to a configured gateway IP on TCP
//! port 21042, and streams synthetic accelerometer + battery frames
//! per `SPEC-R2-ROCKER-WIRE`. The numbers are fake (no ADXL355 wired
//! up yet), but the wire shape is real — pointable at a dashboard for
//! end-to-end testing.

mod sim;
mod wire;
mod wifi;
mod sender;

use anyhow::{anyhow, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{esp_ota_mark_app_valid_cancel_rollback, link_patches, ESP_OK};
use log::{info, warn};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const WIFI_SSID:    &str = env!("R2_WIFI_SSID");
const WIFI_PASS:    &str = env!("R2_WIFI_PASS");
const GATEWAY_IP:   &str = env!("R2_GATEWAY_IP");
const GATEWAY_PORT: u16 = 21042;

fn main() -> Result<()> {
    link_patches();
    EspLogger::initialize_default();

    info!("================================================");
    info!("r2-rocker firmware v{} (simulated sender)", env!("CARGO_PKG_VERSION"));
    info!("================================================");

    if WIFI_SSID.is_empty() {
        warn!("R2_WIFI_SSID is empty — copy wifi_config.toml.example to wifi_config.toml and set the values");
    }

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let _wifi = wifi::connect(
        peripherals.modem,
        sysloop,
        nvs,
        WIFI_SSID,
        WIFI_PASS,
    )?;

    // WiFi up → mark this OTA partition valid so the bootloader doesn't
    // roll back. Per SPEC-R2-ROCKER-SENSOR §12.2 we should also wait for
    // a successful TCP announce + first dashboard ACK; for now WiFi-up
    // is enough since this firmware never actively writes to OTA itself.
    mark_app_valid();

    let gateway_ip: IpAddr = GATEWAY_IP
        .parse::<Ipv4Addr>()
        .map_err(|_| anyhow!("R2_GATEWAY_IP={:?} is not a valid IPv4 address", GATEWAY_IP))?
        .into();
    let gateway = SocketAddr::new(gateway_ip, GATEWAY_PORT);
    let hostname = sender::default_hostname();
    info!("hostname: {}  →  gateway: {}", hostname, gateway);

    let mut s = sender::Sender::new(gateway, hostname);
    s.run();
}

fn mark_app_valid() {
    let rc = unsafe { esp_ota_mark_app_valid_cancel_rollback() };
    if rc == ESP_OK {
        info!("ota: marked running partition VALID (rollback cancelled)");
    } else {
        warn!("ota: esp_ota_mark_app_valid_cancel_rollback returned {}", rc);
    }
}
