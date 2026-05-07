//! r2-rocker firmware — Phase 6 (BLE bootstrap) + simulated sender.
//!
//! Boot sequence per `SPEC-R2-ROCKER-SENSOR.md` §2.1.1:
//!   1. Resolve WiFi creds: NVS → wifi_config.toml fallback → none.
//!   2. If creds: bring up WiFi STA, mark OTA app valid, run sender.
//!   3. Always: advertise R2-BEACON (`nz.ac.auckland.rocker.sensor`,
//!      class hash `0x6A3B0860` per dashboard §6.3) and listen on
//!      L2CAP PSM 0xD2 for `#wifi_offer` events from the controller.
//!   4. On a valid offer: persist creds to NVS and reboot to apply.

mod identity;
mod sim;
mod wire;
mod sender;

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{
    esp_ota_mark_app_valid_cancel_rollback, esp_restart, link_patches, ESP_OK,
};
use log::{info, warn};
use r2_esp::{beacon, l2cap, wifi_prov, wifi_sta};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Canonical R2-BEACON class string (locked at SPEC-R2-ROCKER-DASHBOARD §6.3
/// + SPEC-R2-ROCKER-SENSOR §3.3). FNV-1a-32 hash `0x6A3B0860` is what
/// the dashboard's bootstrap loop matches on.
const SENSOR_CLASS: &str = "nz.ac.auckland.rocker.sensor";

const GATEWAY_IP:   &str = env!("R2_GATEWAY_IP");
const GATEWAY_PORT: u16  = 21042;

fn main() -> Result<()> {
    link_patches();
    EspLogger::initialize_default();

    info!("================================================");
    info!("r2-rocker firmware v{} (Phase 6 — BLE bootstrap)", env!("CARGO_PKG_VERSION"));
    info!("================================================");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // ── Identity (§3.1) — Ed25519 keypair, persisted to NVS. ──────────
    let identity = std::sync::Arc::new(
        identity::Identity::load_or_generate(nvs.clone())
            .context("identity init")?,
    );
    info!(
        "tg_pub_key (verify target):  {:02x}{:02x}{:02x}{:02x}…{:02x}{:02x}",
        identity::TG_PUB_KEY[0], identity::TG_PUB_KEY[1],
        identity::TG_PUB_KEY[2], identity::TG_PUB_KEY[3],
        identity::TG_PUB_KEY[30], identity::TG_PUB_KEY[31],
    );

    // ── Boot priority WiFi-cred resolution (§2.1.1). ──────────────────
    wifi_prov::init_nvs(nvs.clone());
    let creds = wifi_prov::load_credentials(nvs.clone());

    let (wifi_up, _wifi) = match &creds {
        Some(c) => {
            info!("[boot] WiFi credentials source: {}", c.source);
            match wifi_sta::connect(peripherals.modem, sysloop.clone(), nvs.clone(),
                                    &c.ssid, &c.password) {
                Some(w) => (true, Some(w)),
                None    => {
                    warn!("[boot] wifi_sta::connect returned None — falling through to BLE-only");
                    (false, None)
                }
            }
        }
        None => {
            warn!("[boot] no WiFi credentials — entering BLE-only ADVERTISING (§4.1)");
            (false, None)
        }
    };

    // ── BLE — R2-BEACON advertise + L2CAP server. ────────────────────
    // Always running. The beacon advertises `provisioning=true` while we
    // have no WiFi (signals to the dashboard that we need an offer); once
    // creds are in NVS and WiFi is up, future re-provisioning is still
    // possible by simply sending another `#wifi_offer` over L2CAP.
    let beacon_cfg = beacon::BeaconConfig::for_class(SENSOR_CLASS, !wifi_up);
    match beacon::start(beacon_cfg, |peer| {
        info!(
            "[BEACON-RX] peer rbid={:02x}{:02x}{:02x}{:02x}…  class=0x{:08x}  prov={}  rssi={} dBm",
            peer.rbid[0], peer.rbid[1], peer.rbid[2], peer.rbid[3],
            peer.class_hash, peer.flags.provisioning, peer.last_rssi
        );
    }) {
        Ok(_handle) => info!("[BLE] R2-BEACON started (class=\"{}\" prov={})",
                             SENSOR_CLASS, !wifi_up),
        Err(e)      => warn!("[BLE] beacon start failed: {e:?}"),
    }
    l2cap::init();
    info!("[BLE] L2CAP server listening on PSM 0xD2");

    // ── Sender path (only if WiFi came up). ──────────────────────────
    if wifi_up {
        // OTA gate: per SPEC-R2-ROCKER-SENSOR §12.2 a robust gate would
        // also wait for first dashboard ACK; WiFi-up is enough for now.
        mark_app_valid();

        let gateway_ip: IpAddr = GATEWAY_IP
            .parse::<Ipv4Addr>()
            .map_err(|_| anyhow!("R2_GATEWAY_IP={:?} not a valid IPv4 address", GATEWAY_IP))?
            .into();
        let gateway = SocketAddr::new(gateway_ip, GATEWAY_PORT);
        let hostname = sender::default_hostname();
        info!("hostname: {}  →  gateway: {}", hostname, gateway);

        // Run the sender on its own thread so the main thread can keep
        // draining BLE L2CAP for re-provisioning offers.
        let id_for_sender = identity.clone();
        std::thread::Builder::new()
            .stack_size(16384)
            .name("sender".into())
            .spawn(move || {
                let mut s = sender::Sender::new(gateway, hostname, id_for_sender);
                s.run();
            })
            .context("spawn sender thread")?;
    }

    // ── Main loop — drain L2CAP for `#wifi_offer` (§4.2). ────────────
    // Reboot rather than live-reconnect WiFi: simpler, deterministic,
    // and the operator already expects power-cycle behaviour during
    // bootstrap. `wifi_clear` clears NVS without rebooting (next boot
    // will go to ADVERTISING).
    let wifi_offer_hash = wifi_prov::wifi_offer_hash();
    let wifi_clear_hash = wifi_prov::wifi_clear_hash();
    info!("[main-loop] L2CAP poll started — wifi_offer hash 0x{:08x}", wifi_offer_hash);
    loop {
        for (data, _from_addr) in l2cap::drain_received() {
            // r2-bootstrap (controller) prepends a single R2-WIRE FrameHeader
            // byte before the compact frame so it can fragment large payloads
            // over L2CAP. Peel it off first; only `Complete` is supported
            // here — fragment reassembly is out of Phase 6 scope (#wifi_offer
            // is small enough to fit in one L2CAP SDU).
            if data.is_empty() {
                continue;
            }
            let header = r2_wire::FrameHeader::decode(data[0]);
            let body = &data[1..];
            if !matches!(header, r2_wire::FrameHeader::Complete) {
                warn!("[L2CAP] unsupported fragmented frame header={:?}", header);
                continue;
            }
            let msg = match r2_wire::compact::decode_compact(body) {
                Ok(m)  => m,
                Err(e) => {
                    warn!("[L2CAP] decode_compact failed: {e:?} ({} body bytes)", body.len());
                    continue;
                }
            };
            if msg.header.event_hash == wifi_offer_hash {
                info!("[PROV] #wifi_offer received via BLE L2CAP");
                if let Some((ssid, psk)) = wifi_prov::decode_wifi_offer(msg.payload) {
                    info!("[PROV] decoded ssid=\"{}\" — saving to NVS", ssid);
                    if wifi_prov::save_credentials(&ssid, &psk) {
                        info!("[PROV] credentials saved — rebooting in 1 s to apply");
                        FreeRtos::delay_ms(1000);
                        unsafe { esp_restart(); }
                    } else {
                        warn!("[PROV] NVS save failed");
                    }
                } else {
                    warn!("[PROV] failed to decode #wifi_offer payload");
                }
            } else if msg.header.event_hash == wifi_clear_hash {
                warn!("[PROV] wifi_clear — clearing stored credentials");
                wifi_prov::clear_credentials();
            } else {
                info!("[BLE] L2CAP event hash=0x{:08x} (unhandled, {} bytes)",
                      msg.header.event_hash, data.len());
            }
        }
        FreeRtos::delay_ms(500);
    }
}

fn mark_app_valid() {
    let rc = unsafe { esp_ota_mark_app_valid_cancel_rollback() };
    if rc == ESP_OK {
        info!("ota: marked running partition VALID (rollback cancelled)");
    } else {
        warn!("ota: esp_ota_mark_app_valid_cancel_rollback returned {}", rc);
    }
}
