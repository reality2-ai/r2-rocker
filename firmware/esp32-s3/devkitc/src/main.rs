//! r2-rocker firmware — Phase 6 (BLE bootstrap) + simulated sender.
//!
//! Boot sequence per `SPEC-R2-ROCKER-SENSOR.md` §2.1.1:
//!   1. Resolve WiFi creds: NVS → wifi_config.toml fallback → none.
//!   2. If creds: bring up WiFi STA, mark OTA app valid, run sender.
//!   3. Always: advertise R2-BEACON (`nz.ac.auckland.rocker.sensor`,
//!      class hash `0x6A3B0860` per dashboard §6.3) and listen on
//!      L2CAP PSM 0xD2 for `#wifi_offer` events from the controller.
//!   4. On a valid offer: persist creds to NVS and reboot to apply.

mod adxl355;
mod battery;
mod capture;
mod clock;
mod identity;
mod led;
mod ring;
mod sd;
mod sim;
mod wire;
mod sender;

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::IOPin;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{esp_restart, link_patches};
use log::{error, info, warn};
use r2_esp::{beacon, data_tcp, l2cap, log_tcp, ota_tcp, reset_tcp, wifi_prov, wifi_sta};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Canonical R2-BEACON class string (locked at SPEC-R2-ROCKER-DASHBOARD §6.3
/// + SPEC-R2-ROCKER-SENSOR §3.3). FNV-1a-32 hash `0x6A3B0860` is what
/// the dashboard's bootstrap loop matches on.
const SENSOR_CLASS: &str = "nz.ac.auckland.rocker.sensor";

const GATEWAY_IP:   &str = env!("R2_GATEWAY_IP");
const GATEWAY_PORT: u16  = 21042;
/// UDP presence port — matches `r2-bootstrap`'s `PRESENCE_PORT`. Sent
/// once WiFi is up so the dashboard's bootstrap loop can confirm the
/// post-reboot sensor is alive on the offered SSID.
const PRESENCE_PORT: u16 = 21044;

fn main() -> Result<()> {
    link_patches();
    // Install the capturing logger early so every subsequent `info!`
    // / `warn!` is captured for the WiFi-side log fan-out. The TCP
    // listener itself is started AFTER WiFi is up (below, alongside
    // ota_tcp / reset_tcp) — if we bind to 0.0.0.0:21046 before lwIP
    // is initialised the bind never returns and no log_tcp activity
    // ever appears on UART.
    log_tcp::install_logger();

    info!("================================================");
    info!("r2-rocker firmware v{} (Phase 6 — BLE bootstrap)", env!("CARGO_PKG_VERSION"));
    info!("================================================");

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // ── RGB LED (Phase 5L) ───────────────────────────────────────────
    // Onboard WS2812 on GPIO38 (DevKitC-1 v1.1, current production —
    // see HARDWARE-WIRING.md §5). Drives every state transition below.
    // Bring this up FIRST so any error after this point can show ERROR.
    let led_handle = led::start(peripherals.rmt.channel0, peripherals.pins.gpio38)
        .context("LED init")?;
    led_handle.set(led::LedState::Boot);

    // Pull out the peripherals `run()` and the sender thread will need.
    // Anything else from `peripherals` is unused right now.
    let modem   = peripherals.modem;
    let spi2    = peripherals.spi2;
    let sclk    = peripherals.pins.gpio12.downgrade();
    let mosi    = peripherals.pins.gpio11.downgrade();
    let miso    = peripherals.pins.gpio13.downgrade();
    let cs_adxl = peripherals.pins.gpio10.downgrade();
    // SD card CS — dedicated, distinct from ADXL355's. Per
    // HARDWARE-WIRING-DEVKITC.md §3.2. The SD breakout sits on the
    // same MISO/MOSI/SCK lines as the ADXL355; only the CS differs.
    let cs_sd   = peripherals.pins.gpio9.downgrade();
    // Battery-sense divider mid-point on ADC1_CH3 (GPIO 4). Per
    // HARDWARE-WIRING-DEVKITC.md §4.2 / SPEC-R2-ROCKER-SENSOR §8.
    let adc1    = peripherals.adc1;
    let bat_pin = peripherals.pins.gpio4;

    // Top-level error trap — anything below sets the LED red long
    // enough for the operator to see, then resets the chip. The
    // bootloader's rollback partition catches a bad OTA at this
    // point: a buggy new image whose sender never reaches its first
    // successful frame round-trip never marks itself valid, and the
    // next reset rolls back to the previous slot.
    if let Err(e) = run(led_handle.clone(), modem, sysloop, nvs, spi2, sclk, mosi, miso, cs_adxl, cs_sd, adc1, bat_pin) {
        error!("[FATAL] init/runtime error: {e:?}");
        led_handle.set(led::LedState::Error);
        FreeRtos::delay_ms(10_000);
        unsafe { esp_restart(); }
    }
    Ok(())
}

/// Everything between LED-up and the L2CAP poll loop. Returning Err
/// from any `?` here flips the LED to red and triggers a reset; an
/// "unrecoverable" condition therefore manifests as a visible red
/// pulse rather than a silent hang.
fn run(
    led_handle: led::LedHandle,
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    spi2: esp_idf_svc::hal::spi::SPI2,
    sclk: esp_idf_svc::hal::gpio::AnyIOPin,
    mosi: esp_idf_svc::hal::gpio::AnyIOPin,
    miso: esp_idf_svc::hal::gpio::AnyIOPin,
    cs_adxl: esp_idf_svc::hal::gpio::AnyIOPin,
    cs_sd:   esp_idf_svc::hal::gpio::AnyIOPin,
    adc1:    esp_idf_svc::hal::adc::ADC1,
    bat_pin: esp_idf_svc::hal::gpio::Gpio4,
) -> Result<()> {
    // ── Identity (§3.1) — Ed25519 keypair, persisted to NVS. ──────────
    let identity = std::sync::Arc::new(
        identity::Identity::load_or_generate(nvs.clone())
            .context("identity init")?,
    );
    // Stable per-device RBID for R2-BEACON (NVS-persisted; minted on
    // first boot). Stable-across-reboots is the load-bearing property —
    // the dashboard's bootstrap loop matches the *post-reboot* UDP
    // presence packet against the *pre-reboot* RBID it observed during
    // BLE scan, so a regenerated RBID would silently break the loop.
    let rbid = identity::load_or_generate_rbid(nvs.clone())
        .context("rbid init")?;
    info!(
        "tg_pub_key (verify target):  {:02x}{:02x}{:02x}{:02x}…{:02x}{:02x}",
        identity::TG_PUB_KEY[0], identity::TG_PUB_KEY[1],
        identity::TG_PUB_KEY[2], identity::TG_PUB_KEY[3],
        identity::TG_PUB_KEY[30], identity::TG_PUB_KEY[31],
    );

    // ── Synchronised clock (SPEC-R2-ROCKER-TIMESYNC) ──────────────────
    // NVS-backed offset applied to every emitted/persisted ts_ms.
    let clock = clock::Clock::load(nvs.clone()).context("clock init")?;
    // Plumb the synced clock into the LED so every sensor in the rig
    // animates in phase once their time-sync offsets have converged.
    // Until this call, the LED uses its local Instant for animation —
    // boot flash + early advertising aren't synchronised across
    // sensors, but the heartbeat / Recording tick / pulses are.
    led_handle.attach_clock(clock.clone());

    // ── Boot priority WiFi-cred resolution (§2.1.1). ──────────────────
    wifi_prov::init_nvs(nvs.clone());
    let creds = wifi_prov::load_credentials(nvs.clone());

    let (wifi_up, _wifi) = match &creds {
        Some(c) => {
            info!("[boot] WiFi credentials source: {}", c.source);
            led_handle.set(led::LedState::WifiConnecting);
            match wifi_sta::connect(modem, sysloop.clone(), nvs.clone(),
                                    &c.ssid, &c.password) {
                Some(w) => (true, Some(w)),
                None    => {
                    warn!("[boot] wifi_sta::connect returned None — falling through to BLE-only");
                    // Couldn't join the configured AP (gone / wrong PSK /
                    // out of range). Drop the LED to BLE-advertise blue
                    // so the operator can see at a glance we're now
                    // looking for a fresh #wifi_offer over Bluetooth
                    // rather than still trying to associate.
                    led_handle.set(led::LedState::Advertising);
                    (false, None)
                }
            }
        }
        None => {
            warn!("[boot] no WiFi credentials — entering BLE-only ADVERTISING (§4.1)");
            led_handle.set(led::LedState::Advertising);
            (false, None)
        }
    };

    // ── BLE — R2-BEACON advertise + L2CAP server. ────────────────────
    // Always running. The beacon advertises `provisioning=true` while we
    // have no WiFi (signals to the dashboard that we need an offer); once
    // creds are in NVS and WiFi is up, future re-provisioning is still
    // possible by simply sending another `#wifi_offer` over L2CAP.
    let mut beacon_cfg = beacon::BeaconConfig::for_class(SENSOR_CLASS, !wifi_up);
    beacon_cfg.rbid_strategy = beacon::RbidStrategy::Fixed(rbid);
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
        // WiFi up → streaming. The LED state distinguishes healthy
        // (green heartbeat) from sim-fallback (purple slow pulse), set
        // inside the sender thread once we know whether ADXL355 init
        // succeeded. See SPEC-R2-ROCKER-SENSOR-HEALTH §4.

        // OTA-rollback gate is now fired by the sender on first
        // successful TCP frame round-trip (§12.2-tightened). A buggy
        // firmware that joins WiFi but can't reach the dashboard never
        // marks itself valid, so the bootloader rolls back on next boot.

        // Phase 9-light — OTA receive listener on TCP port 21043. Accepts
        // CMD_START preamble (sha256 + size) + firmware stream, writes to
        // the inactive OTA partition, sets it bootable, restarts. Bootloader
        // rollback (CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE) catches a bad
        // image — if the new firmware can't reach `mark_app_valid`, the
        // bootloader reverts on the next reset.
        ota_tcp::start_listener();
        info!("[OTA] receive listener started on TCP 21043");

        // Remote-reset listener on TCP 21044 — accepts a single CMD_RESET byte
        // and calls esp_restart(). Per SPEC-R2-ROCKER-SENSOR-REMOTE-RESET.
        // Refuses while an OTA is in flight (ota_tcp::ota_in_progress()).
        reset_tcp::start_listener();
        info!("[RESET] receive listener started on TCP 21044");

        // Dev log fan-out on TCP 21046. Bind happens here (post-WiFi)
        // rather than at the top of main() — see comment on
        // log_tcp::install_logger above.
        log_tcp::start_listener();

        // Shared handle the sender's CaptureMgr writes ("we're now
        // recording <filename>") and the data_tcp listener reads
        // ("is the requested file currently being written?").
        let current_recording = data_tcp::new_current_recording();

        // Capture-files TCP listener on port 21047 (SPEC-R2-ROCKER-CAPTURE
        // §6). The sender thread (spawned below) builds a CaptureMgr
        // that takes a clone of `current_recording`; data_tcp reads
        // that handle to refuse GET / DEL on the actively-recording
        // file. data_tcp itself is no-op when no SD is mounted —
        // `list_captures` just returns an empty list.
        data_tcp::start_listener(sd::MOUNT_POINT, current_recording.clone());

        // UDP presence — closes the dashboard's bootstrap loop. Spawn a
        // short-lived task that sends ~5 packets at 1 s intervals. UDP
        // is unreliable; one of the burst should reach the dashboard.
        let local_ip = wifi_sta::get_ip().unwrap_or_default();
        let class_hash = r2_core::fnv::r2_hash(SENSOR_CLASS).unwrap_or(0);
        if !local_ip.is_empty() {
            let rbid_for_thread = rbid;
            let ip_for_thread = local_ip.clone();
            std::thread::Builder::new()
                .stack_size(4096)
                .name("presence".into())
                .spawn(move || {
                    broadcast_presence_burst(rbid_for_thread, &ip_for_thread,
                                             class_hash, GATEWAY_PORT, 5);
                })
                .context("spawn presence thread")?;
        }

        let gateway_ip: IpAddr = GATEWAY_IP
            .parse::<Ipv4Addr>()
            .map_err(|_| anyhow!("R2_GATEWAY_IP={:?} not a valid IPv4 address", GATEWAY_IP))?
            .into();
        let gateway = SocketAddr::new(gateway_ip, GATEWAY_PORT);
        let hostname = sender::default_hostname();
        info!("hostname: {}  →  gateway: {}", hostname, gateway);

        // Run the sender on its own thread so the main thread can keep
        // draining BLE L2CAP for re-provisioning offers.
        //
        // The shared SPI2 bus is initialised here (still on the main
        // thread, before the spawn) and Arc-shared into the sender
        // thread. ADXL355 and (optionally) the SD card both attach to
        // the same bus inside the thread — SPI device drivers are not
        // Send, so each device's SpiDeviceDriver / SdSpiHostDriver is
        // constructed on the right side of the thread boundary. If
        // ADXL init fails, the sender falls back to the simulator with
        // a logged warning. If SD init fails, the sender continues
        // streaming-only (no durability) per spec §6.7.
        let id_for_sender = identity.clone();
        let led_for_sender = led_handle.clone();
        let clock_for_sender = clock.clone();
        let current_recording_for_sender = current_recording.clone();
        std::thread::Builder::new()
            .stack_size(16384)
            .name("sender".into())
            .spawn(move || {
                use esp_idf_svc::hal::spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver};
                use std::sync::Arc;
                let bus = match SpiDriver::new(
                    spi2, sclk, mosi, Some(miso),
                    &SpiDriverConfig::new().dma(Dma::Auto(4096)),
                ) {
                    Ok(b) => Arc::new(b),
                    Err(e) => {
                        warn!("[SPI2] bus init failed: {e:?} — sensor cannot stream");
                        return;
                    }
                };
                // Best-effort SD mount on the shared bus FIRST, before the
                // ADXL355 attaches. The ADXL355 attach runs an init
                // sequence (soft reset + WHO_AM_I retries) that generates
                // SCK pulses on the shared bus. If the SD card's CS line
                // hasn't been claimed by its driver yet, the card sees
                // those clock pulses without a valid CS and can end up
                // in an indeterminate state from which CMD0 silently
                // times out. Registering the SD device first lets
                // sdspi_host_init_device drive GPIO9 high before any
                // ADXL355 transactions hit the bus.
                let _sd = sd::SdCard::try_mount(bus.clone(), cs_sd);
                let adxl = match adxl355::Adxl355::new(bus.clone(), cs_adxl) {
                    Ok(a) => Some(a),
                    Err(e) => {
                        warn!("[ADXL355] init failed in sender thread: {e:?} — falling back to simulator");
                        None
                    }
                };
                // If the SD path is up, open the ring writer + run boot
                // recovery (SPEC-R2-ROCKER-SENSOR §6.5). The Sender
                // takes ownership and uses `Ring::append` on every
                // sample + `Ring::tail_seq` to seed its seq counter
                // (resuming from where we left off across reboots).
                let ring = if _sd.is_some() {
                    match ring::Ring::open(sd::MOUNT_POINT) {
                        Ok(r) => {
                            info!("[ring] ready (tail_seq={})", r.tail_seq());
                            Some(r)
                        }
                        Err(e) => {
                            warn!("[ring] open failed: {e:?} — streaming-only this boot");
                            None
                        }
                    }
                } else {
                    None
                };
                // Named-capture state machine — only useful when the
                // SD path is up, since captures land on the same card.
                // CaptureMgr always runs. With no SD, `mark()` refuses
                // (clear error in the log) but `start()` still
                // computes the offset and `observe()` still applies
                // it on the wire path — so the dashboard's Live
                // chart still flatlines when the operator calibrates
                // an SD-less sensor.
                let capture = Some(capture::CaptureMgr::new(
                    sd::MOUNT_POINT,
                    current_recording_for_sender,
                ));
                // Real battery reader on ADC1_CH3 (GPIO 4). Falls back
                // internally to BatterySim if the ADC init fails, so
                // boards without the voltage divider fitted still emit
                // sensible telemetry while we retrofit them.
                let battery = battery::Battery::new(adc1, bat_pin);
                led_for_sender.set(if adxl.is_some() {
                    led::LedState::StreamingLive
                } else {
                    led::LedState::StreamingDegradedSim
                });
                let mut s = sender::Sender::new(
                    gateway, hostname, id_for_sender, led_for_sender, adxl, clock_for_sender, ring, capture, battery,
                );
                s.run();
            })
            .context("spawn sender thread")?;
    } else {
        // BLE-only mode (no WiFi credentials, or WiFi failed) — spawn
        // an ADXL355 diagnostic thread anyway so the operator can
        // verify the SPI wiring and chip enumeration via the serial
        // log before provisioning WiFi. Useful for bench bring-up of a
        // new carrier or fresh solder joints — answers "did the chip
        // come up?" independent of network state.
        //
        // No samples leave the device in this mode; nothing is buffered
        // for later replay. The sender's normal path takes over once
        // WiFi is provisioned and the firmware reboots.
        let led_for_diag = led_handle.clone();
        std::thread::Builder::new()
            .stack_size(8192)
            .name("adxl-diag".into())
            .spawn(move || {
                use esp_idf_svc::hal::spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver};
                use std::sync::Arc;
                // SD CS is unused in BLE-only diagnostic mode (no SD writes
                // happen here) but the pin is still owned by the closure;
                // we drop it explicitly to keep the borrow checker happy.
                let _ = cs_sd;
                let bus = match SpiDriver::new(
                    spi2, sclk, mosi, Some(miso),
                    &SpiDriverConfig::new().dma(Dma::Auto(4096)),
                ) {
                    Ok(b) => Arc::new(b),
                    Err(e) => {
                        warn!("[ADXL355-DIAG] SPI2 bus init failed: {e:?}");
                        return;
                    }
                };
                match adxl355::Adxl355::new(bus, cs_adxl) {
                    Ok(mut adxl) => {
                        info!("[ADXL355-DIAG] BLE-only mode — sensor enumerated; sampling 1 Hz to console");
                        led_for_diag.set(led::LedState::StreamingLive);
                        const LSB_PER_G: f64 = 256_000.0;
                        loop {
                            match adxl.read_xyz_lsb() {
                                Ok((x, y, z)) => info!(
                                    "[ADXL355-DIAG] x={:+.3}g y={:+.3}g z={:+.3}g  (raw lsb {}/{}/{})",
                                    x as f64 / LSB_PER_G,
                                    y as f64 / LSB_PER_G,
                                    z as f64 / LSB_PER_G,
                                    x, y, z,
                                ),
                                Err(e) => warn!("[ADXL355-DIAG] read failed: {e:?}"),
                            }
                            FreeRtos::delay_ms(1000);
                        }
                    }
                    Err(e) => {
                        warn!("[ADXL355-DIAG] init failed: {e:?} — sensor not usable in this boot");
                    }
                }
            })
            .context("spawn adxl-diag thread")?;
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
        // Mirror OTA-in-progress into the LED overlay so the physical
        // and virtual LEDs go to white-strobe while a firmware image
        // is being received + written. The flag is cleared on completion
        // (success or error) by the OTA listener's RAII guard.
        led_handle.set_ota(ota_tcp::ota_in_progress());

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
                // Cyan flash on both physical + virtual LEDs while we
                // process the offer + persist + reboot. Lasts the 1 s
                // post-save sleep — long enough to be visible.
                led_handle.set(led::LedState::BleConnected);
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
            } else if msg.header.event_hash == wire::EVT_DASH_ENROL {
                // Track A — receive + verify + persist the KeyHolder-signed
                // DeviceCertificate. SPEC-R2-ROCKER-SENSOR §3.5 / WIRE §2
                // row 21. Payload layout per `r2-trust::cert::DeviceCertificate`:
                //   [0]     version u8
                //   [1]     sig_algo u8
                //   [2..34] device_public_key (32)
                //   [34..66] trust_group_id (32)
                //   [66]    role u8
                //   [67..75] issued_at u64 BE
                //   [75..83] expires_at u64 BE
                //   [83..147] Ed25519 signature over bytes [0..83]
                info!("[ENROL] r2.dash.enrol received ({} bytes)", msg.payload.len());
                if msg.payload.len() != identity::DEVICE_CERT_LEN {
                    warn!(
                        "[ENROL] payload length {} != expected {} — ignoring",
                        msg.payload.len(), identity::DEVICE_CERT_LEN
                    );
                    continue;
                }
                // 1. Verify the cert's signature under TG_PUB_KEY. The
                //    signed bytes are the first 83 (CERT_DATA_LEN);
                //    the trailing 64 bytes are the signature.
                let signed = &msg.payload[..83];
                let sig_bytes: [u8; 64] = match msg.payload[83..].try_into() {
                    Ok(s) => s,
                    Err(_) => { warn!("[ENROL] sig slice not 64 bytes"); continue; }
                };
                let tg_vk = match ed25519_dalek::VerifyingKey::from_bytes(&identity::TG_PUB_KEY) {
                    Ok(k) => k,
                    Err(e) => {
                        warn!("[ENROL] TG_PUB_KEY invalid: {e:?}");
                        continue;
                    }
                };
                let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
                if let Err(e) = ed25519_dalek::Verifier::verify(&tg_vk, signed, &signature) {
                    warn!("[ENROL] cert signature does not verify under TG_PUB_KEY: {e:?}");
                    continue;
                }
                // 2. Verify the cert's `device_public_key` matches our own.
                //    Otherwise the cert is for a different sensor and
                //    persisting it would impersonate them on announce.
                let our_pk = identity.device_pk();
                let cert_pk = &msg.payload[2..34];
                if cert_pk != our_pk {
                    warn!(
                        "[ENROL] cert is for a different device_pk (cert={:02x}{:02x}{:02x}{:02x}…, ours={:02x}{:02x}{:02x}{:02x}…) — ignoring",
                        cert_pk[0], cert_pk[1], cert_pk[2], cert_pk[3],
                        our_pk[0],  our_pk[1],  our_pk[2],  our_pk[3]
                    );
                    continue;
                }
                // 3. Persist the cert. The next announce will carry it
                //    as CBOR key 8 and the dashboard will switch to
                //    post-cert verification.
                match identity.persist_device_cert(msg.payload) {
                    Ok(()) => info!(
                        "[ENROL] cert verified + persisted; expires_at=ts {} (Unix seconds)",
                        u64::from_be_bytes(msg.payload[75..83].try_into().unwrap_or([0u8; 8]))
                    ),
                    Err(e) => warn!("[ENROL] persist_device_cert failed: {e:?}"),
                }
            } else {
                info!("[BLE] L2CAP event hash=0x{:08x} (unhandled, {} bytes)",
                      msg.header.event_hash, data.len());
            }
        }
        FreeRtos::delay_ms(500);
    }
}

/// Send `count` UDP presence packets to `255.255.255.255:PRESENCE_PORT`
/// at 1 s intervals. Format per `r2-bootstrap::parse_presence_packet`:
/// CBOR `{0: rbid (bytes 8), 1: ip (text), 2: class_hash (u32), 3: port (u16)}`.
fn broadcast_presence_burst(
    rbid: [u8; 8],
    ip: &str,
    class_hash: u32,
    sensor_port: u16,
    count: u32,
) {
    use r2_core::cbor::{encode, CborValue};
    use std::net::UdpSocket;

    let payload = encode(&CborValue::Map(vec![
        (CborValue::UInt(0), CborValue::Bytes(rbid.to_vec())),
        (CborValue::UInt(1), CborValue::Text(ip.to_string())),
        (CborValue::UInt(2), CborValue::UInt(class_hash as u64)),
        (CborValue::UInt(3), CborValue::UInt(sensor_port as u64)),
    ]));

    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { warn!("[presence] bind failed: {e}"); return; }
    };
    if let Err(e) = socket.set_broadcast(true) {
        warn!("[presence] set_broadcast failed: {e}");
        return;
    }
    let dest = format!("255.255.255.255:{}", PRESENCE_PORT);
    info!("[presence] burst — {} packets to {} (rbid={:02x}{:02x}{:02x}{:02x}…, ip={})",
          count, dest, rbid[0], rbid[1], rbid[2], rbid[3], ip);

    for i in 0..count {
        match socket.send_to(&payload, &dest) {
            Ok(n)  => info!("[presence] sent {}/{} ({} bytes)", i + 1, count, n),
            Err(e) => warn!("[presence] send {} failed: {e}", i + 1),
        }
        if i + 1 < count {
            esp_idf_svc::hal::delay::FreeRtos::delay_ms(1000);
        }
    }
}

