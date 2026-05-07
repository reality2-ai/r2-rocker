//! R2 WiFi Provisioning — BLE-based credential management
//!
//! Handles WiFi credential storage (NVS) and BLE provisioning events:
//!   - #wifi_offer: receive SSID + password over BLE L2CAP (R2-WIFI §3.4.2)
//!   - wifi_status: report connection state back over BLE (project ext.)
//!
//! Boot priority:
//!   1. NVS stored credentials (from BLE provisioning or previous session)
//!   2. Compile-time credentials (from wifi_config.toml — dev fallback)
//!   3. BLE-only mode (no WiFi — waiting for provisioning)

use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsDefault};
use log::{info, error};
use std::sync::Mutex;

use r2_core::{cbor, fnv};

const NVS_NAMESPACE: &str = "r2_wifi";
const NVS_KEY_SSID: &str = "ssid";
const NVS_KEY_PASS: &str = "pass";

/// Shared NVS partition handle — stored after first use so we don't need take() again
static NVS_PARTITION: Mutex<Option<EspNvsPartition<NvsDefault>>> = Mutex::new(None);

/// Store the NVS partition for later use by BLE provisioning handlers
pub fn init_nvs(partition: EspNvsPartition<NvsDefault>) {
    if let Ok(mut guard) = NVS_PARTITION.lock() {
        *guard = Some(partition);
    }
}

/// Get a clone of the stored NVS partition
fn get_nvs() -> Option<EspNvsPartition<NvsDefault>> {
    NVS_PARTITION.lock().ok()?.clone()
}

/// WiFi credentials
#[derive(Clone, Debug)]
pub struct WifiCredentials {
    pub ssid: String,
    pub password: String,
    pub source: CredentialSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CredentialSource {
    Nvs,
    CompileTime,
    Ble,
}

impl std::fmt::Display for CredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSource::Nvs => write!(f, "NVS"),
            CredentialSource::CompileTime => write!(f, "compile-time"),
            CredentialSource::Ble => write!(f, "BLE provisioning"),
        }
    }
}

// Compile-time fallback (from wifi_config.toml / env vars)
const COMPILE_SSID: &str = match option_env!("R2_WIFI_SSID") { Some(s) => s, None => "" };
const COMPILE_PASS: &str = match option_env!("R2_WIFI_PASS") { Some(s) => s, None => "" };

/// Load WiFi credentials in priority order: NVS → compile-time → None
pub fn load_credentials(nvs_partition: EspNvsPartition<NvsDefault>) -> Option<WifiCredentials> {
    // Try NVS first
    match load_from_nvs(nvs_partition) {
        Some(creds) => {
            info!("[WIFI-PROV] Loaded credentials from NVS: SSID=\"{}\"", creds.ssid);
            return Some(creds);
        }
        None => {
            info!("[WIFI-PROV] No credentials in NVS");
        }
    }

    // Fall back to compile-time
    if !COMPILE_SSID.is_empty() {
        info!("[WIFI-PROV] Using compile-time credentials: SSID=\"{}\"", COMPILE_SSID);
        return Some(WifiCredentials {
            ssid: COMPILE_SSID.to_string(),
            password: COMPILE_PASS.to_string(),
            source: CredentialSource::CompileTime,
        });
    }

    info!("[WIFI-PROV] No WiFi credentials available — BLE-only mode");
    None
}

/// Save credentials to NVS (persists across reboots and OTA)
pub fn save_to_nvs(
    nvs_partition: EspNvsPartition<NvsDefault>,
    ssid: &str,
    password: &str,
) -> bool {
    let mut nvs = match EspNvs::new(nvs_partition, NVS_NAMESPACE, true) {
        Ok(nvs) => nvs,
        Err(e) => {
            error!("[WIFI-PROV] Failed to open NVS namespace: {}", e);
            return false;
        }
    };

    if let Err(e) = nvs.set_str(NVS_KEY_SSID, ssid) {
        error!("[WIFI-PROV] Failed to write SSID to NVS: {}", e);
        return false;
    }

    if let Err(e) = nvs.set_str(NVS_KEY_PASS, password) {
        error!("[WIFI-PROV] Failed to write password to NVS: {}", e);
        return false;
    }

    info!("[WIFI-PROV] ✅ Credentials saved to NVS: SSID=\"{}\"", ssid);
    true
}

/// Save credentials using the stored NVS partition (for BLE provisioning handler)
pub fn save_credentials(ssid: &str, password: &str) -> bool {
    match get_nvs() {
        Some(nvs) => save_to_nvs(nvs, ssid, password),
        None => {
            error!("[WIFI-PROV] NVS partition not available");
            false
        }
    }
}

/// Clear credentials using the stored NVS partition
pub fn clear_credentials() -> bool {
    match get_nvs() {
        Some(nvs) => clear_nvs(nvs),
        None => {
            error!("[WIFI-PROV] NVS partition not available");
            false
        }
    }
}

/// Clear stored credentials from NVS
pub fn clear_nvs(nvs_partition: EspNvsPartition<NvsDefault>) -> bool {
    let mut nvs = match EspNvs::new(nvs_partition, NVS_NAMESPACE, true) {
        Ok(nvs) => nvs,
        Err(e) => {
            error!("[WIFI-PROV] Failed to open NVS namespace: {}", e);
            return false;
        }
    };

    let _ = nvs.remove(NVS_KEY_SSID);
    let _ = nvs.remove(NVS_KEY_PASS);
    info!("[WIFI-PROV] Credentials cleared from NVS");
    true
}

/// Load credentials from NVS
fn load_from_nvs(nvs_partition: EspNvsPartition<NvsDefault>) -> Option<WifiCredentials> {
    let nvs = EspNvs::new(nvs_partition, NVS_NAMESPACE, false).ok()?;

    let mut ssid_buf = [0u8; 64];
    let mut pass_buf = [0u8; 128];

    let ssid = nvs.get_str(NVS_KEY_SSID, &mut ssid_buf).ok()??;
    let password = nvs.get_str(NVS_KEY_PASS, &mut pass_buf).ok().flatten().unwrap_or("");

    if ssid.is_empty() {
        return None;
    }

    Some(WifiCredentials {
        ssid: ssid.to_string(),
        password: password.to_string(),
        source: CredentialSource::Nvs,
    })
}

// ---------------------------------------------------------------------------
// BLE event handling — #wifi_offer (R2-WIFI §3.4.2) / wifi_clear / wifi_status
// ---------------------------------------------------------------------------

/// FNV-1a-32 of `"#wifi_offer"` (R2-WIFI §3.4.2 — known constant `0x01F77656`).
pub fn wifi_offer_hash() -> u32 {
    fnv::r2_hash("#wifi_offer").unwrap()
}

/// Project-local: clear stored credentials.
pub fn wifi_clear_hash() -> u32 {
    fnv::r2_hash("wifi_clear").unwrap()
}

/// Project-local: response status (sensor → controller).
pub fn wifi_status_hash() -> u32 {
    fnv::r2_hash("wifi_status").unwrap()
}

/// Decode a `#wifi_offer` CBOR payload → (ssid, password).
///
/// Expected format per R2-WIFI §3.4.2: `{0: "ssid", 1: "password", ...}`.
/// Extra fields (gateway_ip, port, ttl) are ignored here — wifi_sta only
/// needs SSID + PSK.
pub fn decode_wifi_offer(payload: &[u8]) -> Option<(String, String)> {
    let decoded = cbor::decode(payload).ok()?;
    match &decoded {
        cbor::CborValue::Map(entries) => {
            let mut ssid = String::new();
            let mut password = String::new();
            for (k, v) in entries {
                match (k, v) {
                    (cbor::CborValue::UInt(0), cbor::CborValue::Text(s)) => ssid = s.clone(),
                    (cbor::CborValue::UInt(1), cbor::CborValue::Text(s)) => password = s.clone(),
                    _ => {}
                }
            }
            if ssid.is_empty() {
                None
            } else {
                Some((ssid, password))
            }
        }
        _ => None,
    }
}

/// Build a wifi_status response event
///
/// Payload: {0: "status", 1: "ip" (optional)}
/// Status: "connected", "connecting", "failed", "no_credentials", "cleared"
pub fn build_wifi_status_payload(status: &str, ip: Option<&str>) -> Vec<u8> {
    let mut map = vec![(
        cbor::CborValue::UInt(0),
        cbor::CborValue::Text(status.to_string()),
    )];
    if let Some(ip) = ip {
        map.push((
            cbor::CborValue::UInt(1),
            cbor::CborValue::Text(ip.to_string()),
        ));
    }
    cbor::encode(&cbor::CborValue::Map(map))
}
