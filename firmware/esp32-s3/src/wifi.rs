//! WiFi STA bring-up using esp-idf-svc's blocking adapter.
//!
//! Adapted from `r2-core/crates/r2-esp/src/wifi_sta.rs` with the
//! provisioning hooks stripped out — Phase 0.5+ uses compile-time
//! credentials via build.rs (env vars `R2_WIFI_SSID`, `R2_WIFI_PASS`,
//! `R2_GATEWAY_IP`). BLE bootstrap arrives in Phase 6.

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::info;

/// Connect to a WPA2/WPA3 access point and block until DHCP is up.
///
/// Returns the live `BlockingWifi` so the caller can hold ownership
/// (drops disconnect the radio). The IP info is logged on success.
pub fn connect(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Result<BlockingWifi<EspWifi<'static>>> {
    if ssid.is_empty() {
        bail!("no WiFi SSID configured — set wifi_config.toml or R2_WIFI_SSID");
    }

    let esp_wifi = EspWifi::new(modem, sysloop.clone(), Some(nvs))
        .context("EspWifi::new")?;
    let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)
        .context("BlockingWifi::wrap")?;

    let cfg = Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().map_err(|_| anyhow!("ssid too long"))?,
        password: password.try_into().map_err(|_| anyhow!("password too long"))?,
        ..Default::default()
    });

    wifi.set_configuration(&cfg).context("set_configuration")?;
    wifi.start().context("wifi.start()")?;
    info!("wifi: connecting to \"{}\"…", ssid);
    wifi.connect().context("wifi.connect()")?;
    wifi.wait_netif_up().context("wait_netif_up")?;

    if let Ok(ip_info) = wifi.wifi().sta_netif().get_ip_info() {
        info!(
            "wifi: connected — ip={} gw={} subnet={}",
            ip_info.ip, ip_info.subnet.gateway, ip_info.subnet.mask
        );
    }

    Ok(wifi)
}
