//! R2 WiFi Station Mode — ESP32
//!
//! Connects to a WiFi network using provided credentials.
//! Runs alongside BLE (coexistence enabled in sdkconfig).
//!
//! Once connected, the device is reachable on the local network for:
//!   - TCP OTA firmware push (port 21043)
//!   - Device management API (port 21045, future)
//!   - R2-WIRE event relay (port 21042, future)

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::{error, info, warn};

use std::sync::Mutex;

/// Global IP address — set on successful connection, read by other modules
static CURRENT_IP: Mutex<Option<String>> = Mutex::new(None);

/// WiFi connection handle. Must be kept alive for the duration of the connection.
pub struct WifiConnection {
    wifi: BlockingWifi<EspWifi<'static>>,
}

impl WifiConnection {
    /// Disconnect and reconnect with new credentials.
    /// Returns the new IP address on success.
    pub fn reconnect(&mut self, ssid: &str, password: &str) -> Option<String> {
        info!("[WIFI] Reconnecting to \"{}\"...", ssid);

        // Disconnect current
        let _ = self.wifi.disconnect();

        let config = Configuration::Client(ClientConfiguration {
            ssid: ssid.try_into().unwrap_or_default(),
            password: password.try_into().unwrap_or_default(),
            ..Default::default()
        });

        if let Err(e) = self.wifi.set_configuration(&config) {
            error!("[WIFI] Failed to set new configuration: {}", e);
            return None;
        }

        if let Err(e) = self.wifi.connect() {
            error!("[WIFI] Failed to connect to \"{}\": {}", ssid, e);
            return None;
        }

        if let Err(e) = self.wifi.wait_netif_up() {
            error!("[WIFI] Network interface failed to come up: {}", e);
            return None;
        }

        let ip = log_ip_info(&self.wifi, ssid);
        if let Some(ref ip) = ip {
            set_current_ip(Some(ip.clone()));
        }
        ip
    }
}

/// Initialise WiFi in station mode and connect.
///
/// Returns None if connection fails.
/// The returned WifiConnection must be kept alive — dropping it disconnects.
pub fn connect(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Option<WifiConnection> {
    if ssid.is_empty() {
        warn!("[WIFI] No SSID provided — WiFi disabled");
        return None;
    }

    info!("[WIFI] Connecting to \"{}\"...", ssid);

    let esp_wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs)) {
        Ok(w) => w,
        Err(e) => {
            error!("[WIFI] Failed to create WiFi driver: {}", e);
            return None;
        }
    };

    let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop).ok()?;

    let config = Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().unwrap_or_default(),
        password: password.try_into().unwrap_or_default(),
        ..Default::default()
    });

    if let Err(e) = wifi.set_configuration(&config) {
        error!("[WIFI] Failed to set configuration: {}", e);
        return None;
    }

    if let Err(e) = wifi.start() {
        error!("[WIFI] Failed to start: {}", e);
        return None;
    }

    if let Err(e) = wifi.connect() {
        error!("[WIFI] Failed to connect to \"{}\": {}", ssid, e);
        return None;
    }

    // Wait for DHCP
    if let Err(e) = wifi.wait_netif_up() {
        error!("[WIFI] Network interface failed to come up: {}", e);
        return None;
    }

    let ip = log_ip_info(&wifi, ssid);
    if let Some(ref ip) = ip {
        set_current_ip(Some(ip.clone()));
    }

    Some(WifiConnection { wifi })
}

/// Get the device's current IP address, or None if not connected.
pub fn get_ip() -> Option<String> {
    CURRENT_IP.lock().ok()?.clone()
}

fn set_current_ip(ip: Option<String>) {
    if let Ok(mut guard) = CURRENT_IP.lock() {
        *guard = ip;
    }
}

fn log_ip_info(wifi: &BlockingWifi<EspWifi<'_>>, ssid: &str) -> Option<String> {
    let ip_info = wifi.wifi().sta_netif().get_ip_info();
    match ip_info {
        Ok(info) => {
            let ip_str = format!("{}", info.ip);
            info!("[WIFI] ✅ Connected to \"{}\"", ssid);
            info!("[WIFI]   IP:      {}", info.ip);
            info!("[WIFI]   Gateway: {}", info.subnet.gateway);
            info!("[WIFI]   Mask:    {}", info.subnet.mask);
            Some(ip_str)
        }
        Err(e) => {
            info!("[WIFI] ✅ Connected to \"{}\" (IP info error: {})", ssid, e);
            None
        }
    }
}
