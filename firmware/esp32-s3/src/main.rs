//! r2-rocker firmware — Phase 0.5 pre-soldering smoke test.
//!
//! Confirms toolchain, flash, and boot path on the bare DevKitC-1
//! (no ADXL355, no SD, no battery wired up). Loops printing a fake
//! FSM state name once per second so the UART monitor shows a
//! heartbeat. The state names match `SPEC-R2-ROCKER-SENSOR.md` §4.1
//! so the UART output is a useful sanity check that the spec
//! terminology is right.

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys::{esp_mac_type_t_ESP_MAC_WIFI_STA, esp_read_mac, link_patches};
use log::info;

fn main() {
    link_patches();
    EspLogger::initialize_default();

    info!("================================================");
    info!("r2-rocker firmware v{}", env!("CARGO_PKG_VERSION"));
    info!("Phase 0.5 — pre-soldering smoke test");
    info!("================================================");
    info!("");
    info!("This firmware confirms the build, flash, and boot path.");
    info!("It does not yet read sensors or talk to the network.");
    info!("");

    let mac = read_mac();
    info!(
        "Device MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    info!("");
    info!("Beginning FSM-state heartbeat (1 Hz).");
    info!("Press Ctrl-C in the monitor to exit.");
    info!("");

    // State names per SPEC-R2-ROCKER-SENSOR §4.1.
    const STATES: &[&str] = &[
        "BOOT",
        "ADVERTISING",
        "BLE_CONNECTED",
        "WIFI_CONNECTING",
        "STREAMING_LIVE",
        "STREAMING_CATCHUP",
        "CALIBRATING",
        "LOW_BATTERY",
        "OTA",
        "ERROR",
    ];

    let mut tick: u32 = 0;
    loop {
        let state = STATES[(tick as usize) % STATES.len()];
        info!("[t={:>5}s] FSM-demo state: {}", tick, state);
        FreeRtos::delay_ms(1000);
        tick = tick.wrapping_add(1);
    }
}

fn read_mac() -> [u8; 6] {
    let mut mac = [0u8; 6];
    // SAFETY: esp_read_mac writes 6 bytes into the buffer; ESP_MAC_WIFI_STA
    // is always available on ESP32-S3 (the eFuse holds it from factory).
    unsafe {
        esp_read_mac(mac.as_mut_ptr(), esp_mac_type_t_ESP_MAC_WIFI_STA);
    }
    mac
}
