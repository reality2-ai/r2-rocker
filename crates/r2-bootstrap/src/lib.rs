//! R2 Bootstrap Library
//!
//! Discovers R2 sensors via BLE, sends WiFi credentials over L2CAP,
//! waits for UDP presence, then validates TCP R2-WIRE session with test.ping/pong.
//!
//! `run_bootstrap` runs as a **continuous loop**: after the initial scan it retries
//! every 20 s, skipping sensors that are already streaming data to the dashboard.
//! This means a sensor that boots late, restarts, or fails L2CAP is picked up
//! automatically — no second click needed.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use bluer::l2cap::{Socket, SocketAddr as L2capSocketAddr, Security, SecurityLevel};
use bluer::{AdapterEvent, Address, AddressType, DiscoveryFilter, DiscoveryTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use r2_core::{beacon, fnv};
use r2_wire::compact::{decode_compact, encode_compact};
use r2_wire::types::{CompactHeader, CompactMessage, Flags, MsgType};

/// Fixed PSM for R2 event transport (same as l2cap.rs).
const R2_PSM: u16 = bluer::l2cap::PSM_LE_DYN_START + 0x52; // 0xD2

/// RAII guard: aborts the wrapped tokio task when dropped.
/// Use instead of a bare JoinHandle when the task must not outlive its owner.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

/// UDP presence port.
const PRESENCE_PORT: u16 = 21044;
/// TCP R2-WIRE event port.
const EVENT_PORT: u16 = 21042;
/// Fixed hotspot SSID — stable name prevents stale profile accumulation on sensors.
const HOTSPOT_SSID: &str = "R2-rocker";
/// Fixed hotspot PSK — hardcoded to avoid `nmcli -s` secrets read (needs polkit/sudo).
const HOTSPOT_PSK: &str = "r2rocker2026";

/// Configuration for a bootstrap run.
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub ssid: Option<String>,
    pub psk: Option<String>,
    pub scan_secs: u64,
    pub target_class: String,
    /// When true, the engine tears down any existing matching hotspot
    /// before bringing one up, even if one is already active on the
    /// right adapter. Sensors currently joined to that hotspot lose
    /// WiFi and fall back to BLE advertising — which is the only
    /// path back into the bootstrap flow if the operator wants to
    /// re-push credentials to an already-streaming sensor. Set this
    /// when the operator re-presses "Connect Sensors".
    pub cycle_hotspot: bool,
}

/// Events emitted during the bootstrap process.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "data")]
pub enum BootstrapEvent {
    Log(String),
    SensorFound { addr: String, name: String },
    SensorConnected { addr: String, name: String, ip: String },
    Done { count: usize },
    Error(String),
}

/// A discovered sensor's BLE address and RBID.
#[derive(Debug, Clone)]
struct DiscoveredSensor {
    addr: Address,
    addr_type: AddressType,
    rbid: [u8; 8],
}

/// Result of bootstrapping a single sensor.
struct BootstrapResult {
    addr: Address,
    sensor_ip: String,
    rtt_ms: u128,
}

/// A parsed UDP presence packet — broadcast to all per-sensor tasks.
#[derive(Debug, Clone)]
struct PresencePacket {
    rbid: Option<[u8; 8]>,
    ip: String,
}

/// Internal result sent from per-sensor bootstrap tasks back to the main loop.
enum SensorTaskResult {
    Ok { addr: Address, ip: String },
    Failed { addr: Address },
}

/// Track per-sensor state in the main loop.
enum SensorState {
    Bootstrapping,
    Active(String), // hotspot IP
}

/// Run the full bootstrap process, sending progress events on `progress_tx`.
///
/// This function **never returns** (unless the task is aborted). It runs a
/// continuous scan-and-retry loop:
///
/// 1. Creates the WiFi hotspot once.
/// 2. Scans for BLE beacons every `RETRY_INTERVAL_SECS` seconds.
/// Run the full bootstrap process, sending progress events on `progress_tx`.
///
/// Never returns (unless aborted). Runs a continuous scan-and-retry loop:
/// 1. Creates the WiFi hotspot once.
/// 2. Starts a shared UDP presence dispatcher (broadcast channel, filtered by RBID).
/// 3. Scans for BLE beacons every RETRY_INTERVAL_SECS.
/// 4. Spawns a **parallel per-sensor task** for each new sensor — no blocking.
/// 5. Each task: L2CAP connect (with retry) -> #wifi_offer -> wait presence by RBID
///    -> TCP ping/pong -> report result back via mpsc channel.
pub async fn run_bootstrap(
    config: BootstrapConfig,
    progress_tx: mpsc::Sender<BootstrapEvent>,
) -> Result<()> {
    // Shorter than the previous 20s default: when a sensor misses the
    // first BLE scan window (advertise-interval / RSSI variance / post-
    // hotspot-cycle reboot timing), the dashboard rescans faster so the
    // operator doesn't sit through 30+ seconds of "only one sensor
    // shown." Pair this with `scan_secs` in dashboard/src/main.rs
    // — together they make the worst-case detection ~25s (20s scan +
    // 5s wait) instead of ~30s (10s scan + 20s wait).
    const RETRY_INTERVAL_SECS: u64 = 5;

    let target_class_hash = fnv::fnv1a_32(config.target_class.to_lowercase().as_bytes());
    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "Target class: '{}' (hash: 0x{:08X})",
        config.target_class, target_class_hash
    ))).await;

    // One-time WiFi setup
    let (ssid, psk, our_ip) = setup_wifi_credentials(&config, &progress_tx).await?;

    // Shared UDP presence dispatcher: bind socket once, broadcast all packets.
    // Per-sensor tasks subscribe and filter by their own RBID.
    // Use AbortOnDrop so the task is cancelled when run_bootstrap is aborted —
    // a plain JoinHandle drop does NOT abort the task in tokio, which would leave
    // the UDP socket open and cause "Address already in use" on the next bootstrap.
    let (presence_bc, _) = broadcast::channel::<PresencePacket>(64);
    let _presence_dispatcher = AbortOnDrop(tokio::spawn({
        let tx = presence_bc.clone();
        async move { run_presence_dispatcher(PRESENCE_PORT, tx).await; }
    }));

    // Feedback channel: per-sensor tasks report Ok/Failed back to main loop
    let (result_tx, mut result_rx) = mpsc::channel::<SensorTaskResult>(16);

    let mut known: HashMap<Address, SensorState> = HashMap::new();
    let mut task_handles: HashMap<Address, JoinHandle<()>> = HashMap::new();
    // Track last time any sensor was confirmed streaming.
    // BLE scans are suppressed for SCAN_QUIET_SECS after last activity — the
    // RTL8723DS shares its BLE/WiFi radio and an incoming scan/L2CAP request
    // during active streaming causes WiFi disruption -> keepalive fail -> restart.
    let mut last_sensor_active_at: Option<std::time::Instant> = None;
    const SCAN_QUIET_SECS: u64 = 300; // 5 min quiet window after last active sensor
    let mut done_notified = false;
    let mut sensor_index: usize = 0;

    loop {
        // Collect completed task results
        while let Ok(result) = result_rx.try_recv() {
            match result {
                SensorTaskResult::Ok { addr, ip } => {
                    let _ = progress_tx.send(BootstrapEvent::SensorConnected {
                        addr: addr.to_string(),
                        name: addr.to_string(),
                        ip: ip.clone(),
                    }).await;
                    known.insert(addr, SensorState::Active(ip));
                    last_sensor_active_at = Some(std::time::Instant::now());
                    task_handles.remove(&addr);
                    done_notified = false;
                }
                SensorTaskResult::Failed { addr } => {
                    known.remove(&addr);
                    task_handles.remove(&addr);
                    done_notified = false;
                }
            }
        }

        // Prune sensors that dropped their TCP connection
        let active_ips = get_active_sensor_ips(EVENT_PORT);
        let mut dropped = 0usize;
        known.retain(|addr, state| match state {
            SensorState::Active(ip) => {
                if !active_ips.contains(ip.as_str()) {
                    if let Some(h) = task_handles.remove(addr) { h.abort(); }
                    dropped += 1;
                    false
                } else { true }
            }
            SensorState::Bootstrapping => true,
        });
        if dropped > 0 {
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "{} sensor(s) disconnected -- will re-bootstrap", dropped
            ))).await;
            done_notified = false;
        }

        // BLE scan — suppress while sensors were recently streaming.
        // The RTL8723DS shares its radio between BLE and WiFi; a BLE scan
        // (or incoming L2CAP attempt on a cached MAC) disrupts active WiFi
        // -> TCP keepalive fails -> sensor restarts. We track the last time
        // any sensor was Active and suppress scans for SCAN_QUIET_SECS after
        // that point. This persists even through brief TCP drops (sensor
        // reconnects via resume without needing a new BLE scan).
        let active_count = known.values()
            .filter(|s| matches!(s, SensorState::Active(_))).count();
        let pending_count = known.len() - active_count;

        // Refresh quiet timer while sensors are currently active
        if active_count > 0 || active_ips.len() > 0 {
            last_sensor_active_at = Some(std::time::Instant::now());
        }

        let quiet_remaining = last_sensor_active_at
            .map(|t| SCAN_QUIET_SECS.saturating_sub(t.elapsed().as_secs()))
            .unwrap_or(0);

        if quiet_remaining > 0 && pending_count == 0 {
            let total_streaming = active_count + active_ips.len().saturating_sub(active_count);
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "{} sensor(s) streaming — scan quiet for {}s...", total_streaming, quiet_remaining
            ))).await;
            tokio::time::sleep(Duration::from_secs(20)).await;
            continue;
        }

        let scan_label = if known.is_empty() {
            "Scanning for sensors...".to_string()
        } else {
            format!("Scanning -- {} active, {} bootstrapping...", active_count, pending_count)
        };
        let _ = progress_tx.send(BootstrapEvent::Log(scan_label)).await;

        let discovered = match ble_scan(target_class_hash, config.scan_secs, &progress_tx).await {
            Ok(d) => d,
            Err(e) => {
                let _ = progress_tx.send(BootstrapEvent::Log(format!(
                    "BLE scan error: {} -- retrying in {}s", e, RETRY_INTERVAL_SECS
                ))).await;
                tokio::time::sleep(Duration::from_secs(RETRY_INTERVAL_SECS)).await;
                continue;
            }
        };

        let new_sensors: Vec<_> = discovered.into_iter()
            .filter(|s| !known.contains_key(&s.addr))
            .collect();

        if new_sensors.is_empty() {
            if known.is_empty() {
                let _ = progress_tx.send(BootstrapEvent::Log(format!(
                    "No sensors found -- retrying in {}s...", RETRY_INTERVAL_SECS
                ))).await;
            }
        } else {
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "Found {} new sensor(s) -- bootstrapping in parallel...", new_sensors.len()
            ))).await;

            for sensor in new_sensors {
                let addr = sensor.addr;
                let idx = sensor_index;
                sensor_index += 1;
                known.insert(addr, SensorState::Bootstrapping);

                let handle = tokio::spawn(bootstrap_sensor_task(
                    idx, sensor,
                    ssid.clone(), psk.clone(), our_ip.clone(),
                    presence_bc.subscribe(),
                    progress_tx.clone(),
                    result_tx.clone(),
                ));
                task_handles.insert(addr, handle);
            }
        }

        // Send Done once all known sensors are Active
        let all_active = !known.is_empty()
            && known.values().all(|s| matches!(s, SensorState::Active(_)));
        if !done_notified && all_active {
            let _ = progress_tx.send(BootstrapEvent::Done { count: known.len() }).await;
            done_notified = true;
        }

        tokio::time::sleep(Duration::from_secs(RETRY_INTERVAL_SECS)).await;
    }
}

/// Wrapper task: runs bootstrap_sensor, sends result back via result_tx.
async fn bootstrap_sensor_task(
    idx: usize,
    sensor: DiscoveredSensor,
    ssid: String,
    psk: String,
    our_ip: String,
    presence_rx: broadcast::Receiver<PresencePacket>,
    progress_tx: mpsc::Sender<BootstrapEvent>,
    result_tx: mpsc::Sender<SensorTaskResult>,
) {
    let addr = sensor.addr;
    match bootstrap_sensor(idx, sensor, &ssid, &psk, &our_ip, presence_rx, &progress_tx).await {
        Ok(result) => {
            let _ = result_tx.send(SensorTaskResult::Ok {
                addr: result.addr, ip: result.sensor_ip,
            }).await;
        }
        Err(e) => {
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "[SENSOR-{}] Failed: {}", idx, e
            ))).await;
            let _ = result_tx.send(SensorTaskResult::Failed { addr }).await;
        }
    }
}

/// UDP presence dispatcher -- runs forever, broadcasts packets to all subscribers.
async fn run_presence_dispatcher(port: u16, tx: broadcast::Sender<PresencePacket>) {
    let socket = match tokio::net::UdpSocket::bind(("0.0.0.0", port)).await {
        Ok(s) => s,
        Err(e) => { eprintln!("[presence] bind port {} failed: {}", port, e); return; }
    };
    let mut buf = [0u8; 512];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, src)) => { let _ = tx.send(parse_presence_packet(&buf[..n], src)); }
            Err(_) => {}
        }
    }
}

/// Parse a UDP presence CBOR payload.
/// Format: {0: rbid(bytes8), 1: ip(text), 2: class_hash(u32), 3: port(u16)}
fn parse_presence_packet(data: &[u8], src: std::net::SocketAddr) -> PresencePacket {
    let mut rbid: Option<[u8; 8]> = None;
    let mut ip = String::new();
    if let Ok(r2_core::cbor::CborValue::Map(entries)) = r2_core::cbor::decode(data) {
        for (k, v) in &entries {
            match (k, v) {
                (r2_core::cbor::CborValue::UInt(0), r2_core::cbor::CborValue::Bytes(b))
                    if b.len() == 8 =>
                {
                    let mut arr = [0u8; 8]; arr.copy_from_slice(b); rbid = Some(arr);
                }
                (r2_core::cbor::CborValue::UInt(1), r2_core::cbor::CborValue::Text(s)) => {
                    ip = if s.is_empty() || s == "0.0.0.0" { src.ip().to_string() } else { s.clone() };
                }
                _ => {}
            }
        }
    }
    if ip.is_empty() || ip == "0.0.0.0" { ip = src.ip().to_string(); }
    PresencePacket { rbid, ip }
}

/// Wait for a UDP presence packet matching `target_rbid`.
/// Uses a broadcast::Receiver subscription -- parallel-safe, no port fighting.
async fn wait_for_presence_rbid(
    mut rx: broadcast::Receiver<PresencePacket>,
    target_rbid: [u8; 8],
    timeout: Duration,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { bail!("presence timeout -- sensor did not join hotspot in 60s"); }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(pkt)) if pkt.rbid == Some(target_rbid) => return Ok(pkt.ip),
            Ok(Ok(_)) => {} // different sensor, keep waiting
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {} // dropped some, keep going
            Ok(Err(_)) => bail!("presence channel closed"),
            Err(_) => bail!("presence timeout -- sensor did not join hotspot in 60s"),
        }
    }
}


/// One-time WiFi credential setup: use provided ssid/psk or create a hotspot.
async fn setup_wifi_credentials(
    config: &BootstrapConfig,
    progress_tx: &mpsc::Sender<BootstrapEvent>,
) -> Result<(String, String, String)> {
    match (&config.ssid, &config.psk) {
        (Some(s), Some(p)) => {
            let our_ip = get_local_ip().unwrap_or_else(|| "0.0.0.0".to_string());
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "Using provided WiFi: ssid='{}' our_ip={}", s, our_ip
            ))).await;
            Ok((s.clone(), p.clone(), our_ip))
        }
        _ => {
            if config.cycle_hotspot {
                let _ = progress_tx.send(BootstrapEvent::Log(
                    "Cycling hotspot — bringing it down so connected sensors fall back to BLE...".into()
                )).await;
                bring_down_matching_hotspot();
                // Brief settle window so NetworkManager processes the down
                // transition before we ask it to bring a fresh one up.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            let _ = progress_tx.send(BootstrapEvent::Log(
                "No SSID/PSK provided — creating WiFi hotspot...".into()
            )).await;
            let (s, p, ip) = create_hotspot()?;
            let our_ip = ip.unwrap_or_else(|| get_local_ip().unwrap_or_else(|| "0.0.0.0".to_string()));
            let _ = progress_tx.send(BootstrapEvent::Log(format!(
                "Hotspot ready — ssid='{}' our_ip={}", s, our_ip
            ))).await;
            Ok((s, p, our_ip))
        }
    }
}

/// Take down any currently-active AP-mode connection whose SSID is our
/// `HOTSPOT_SSID`. Sensors joined to it will lose WiFi within a few
/// seconds; firmware-side keepalive failure forces them back to BLE
/// advertising where `run_bootstrap` can re-push credentials.
fn bring_down_matching_hotspot() {
    let out = match Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE,STATE", "con", "show", "--active"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() < 3 { continue; }
        let (name, typ, state) = (parts[0], parts[1], parts[2]);
        if typ != "802-11-wireless" || state != "activated" { continue; }
        // Confirm it's an AP — never tear down a station-mode connection.
        let mode_out = Command::new("nmcli")
            .args(["-t", "-g", "802-11-wireless.mode", "con", "show", name])
            .output().ok();
        let mode = mode_out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if mode != "ap" { continue; }
        // Confirm SSID matches our well-known hotspot — don't break
        // the operator's own LAN hotspot if they happen to be hosting one.
        let ssid_out = Command::new("nmcli")
            .args(["-t", "-g", "802-11-wireless.ssid", "con", "show", name])
            .output().ok();
        let ssid = ssid_out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if ssid != HOTSPOT_SSID { continue; }
        eprintln!("[bootstrap] cycling hotspot '{}'", name);
        let _ = Command::new("nmcli").args(["con", "down", name]).output();
    }
}

/// Query which sensor IPs currently have active TCP data connections to the dashboard.
///
/// Runs `ss -tnp` and extracts remote IPs from connections owned by `r2-dashboard`
/// on `EVENT_PORT`. Returns an empty set if the command fails.
///
/// Filters out sockets with backed-up Send-Q. When a sensor disappears
/// abruptly (chip reset, WiFi drop), the dashboard's read-timeout
/// closes its end but the kernel TCP entry lingers in `ESTAB` for the
/// retransmit-timeout window (~15 min default) — leaving ghost
/// connections that look "streaming" to this scan. Real streaming
/// connections have Send-Q ≈ 0 because the sensor ACKs the
/// dashboard's pong/ack frames immediately; a non-zero Send-Q means
/// nothing has been ACKed in a while and the peer is effectively gone.
fn get_active_sensor_ips(port: u16) -> HashSet<String> {
    let output = Command::new("sh")
        .args(["-c", &format!(
            "ss -tnp 2>/dev/null | grep r2-dashboard | grep ':{}'", port
        )])
        .output()
        .ok();
    let stdout = output.map(|o| o.stdout).unwrap_or_default();
    let s = String::from_utf8_lossy(&stdout);
    let mut ips = HashSet::new();
    for line in s.lines() {
        // Line format: "ESTAB 0 0 local_ip:port remote_ip:remote_port ..."
        // Columns: State, Recv-Q, Send-Q, Local, Peer, [Process].
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 { continue; }
        // Skip ghost ESTABs with stuck Send-Q (sensor gone, kernel
        // still retrying retransmits).
        let send_q: u32 = parts[2].parse().unwrap_or(0);
        if send_q > 0 { continue; }
        let remote = parts[4];
        if let Some(ip) = remote.rsplitn(2, ':').nth(1) {
            ips.insert(ip.to_string());
        }
    }
    ips
}

/// BLE passive scan for R2 beacons matching target class hash.
async fn ble_scan(
    target_class_hash: u32,
    scan_secs: u64,
    progress_tx: &mpsc::Sender<BootstrapEvent>,
) -> Result<Vec<DiscoveredSensor>> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    // Flush BlueZ device cache before scanning.
    // Without this, DeviceAdded fires immediately for previously-seen devices
    // (using cached manufacturer_data) even if they are powered off.
    // Removing all known devices forces BlueZ to only report live advertisements.
    if let Ok(cached) = adapter.device_addresses().await {
        for addr in cached {
            let _ = adapter.remove_device(addr).await;
        }
    }

    let filter = DiscoveryFilter {
        transport: DiscoveryTransport::Le,
        ..Default::default()
    };
    adapter.set_discovery_filter(filter).await?;
    let mut disco = adapter.discover_devices().await?;
    let mut found: HashMap<Address, DiscoveredSensor> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(scan_secs);

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, disco.next()).await {
            Ok(Some(AdapterEvent::DeviceAdded(addr))) => {
                let device = match adapter.device(addr) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let mfr_data = match device.manufacturer_data().await {
                    Ok(Some(m)) => m,
                    _ => continue,
                };

                for &company_id in &[0xFFFFu16, 0xFFFE] {
                    if let Some(data) = mfr_data.get(&company_id) {
                        if data.len() >= 11 && data[0] == beacon::R2_BEACON_MAGIC {
                            if data.len() >= 15 {
                                let class_hash = u32::from_be_bytes([
                                    data[11], data[12], data[13], data[14],
                                ]);
                                if class_hash == target_class_hash {
                                    let mut rbid = [0u8; 8];
                                    rbid.copy_from_slice(&data[3..11]);
                                    if !found.contains_key(&addr) {
                                        let addr_type = device.address_type().await
                                            .unwrap_or(AddressType::LePublic);
                                        let _ = progress_tx.send(BootstrapEvent::SensorFound {
                                            addr: addr.to_string(),
                                            name: format!("RBID:{}", hex::encode(rbid)),
                                        }).await;
                                        found.insert(
                                            addr,
                                            DiscoveredSensor { addr, addr_type, rbid },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }

    drop(disco);
    Ok(found.into_values().collect())
}

/// Bootstrap a single sensor: L2CAP → WiFi offer → UDP presence → TCP ping/pong.
async fn bootstrap_sensor(
    idx: usize,
    sensor: DiscoveredSensor,
    ssid: &str,
    psk: &str,
    our_ip: &str,
    presence_rx: broadcast::Receiver<PresencePacket>,
    progress_tx: &mpsc::Sender<BootstrapEvent>,
) -> Result<BootstrapResult> {
    let tag = format!("SENSOR-{}", idx);

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] Connecting L2CAP to {}...", tag, sensor.addr
    ))).await;

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    // Retry L2CAP connect — sensor's listener may not be bound yet on fresh boot.
    // RTL8723DS BLE firmware takes a few seconds to initialise after power-on.
    let mut stream = {
        let mut last_err = anyhow::anyhow!("not attempted");
        let mut result = None;
        for attempt in 1..=5u32 {
            match l2cap_connect_low_security(sensor.addr, sensor.addr_type, R2_PSM).await {
                Ok(s) => { result = Some(s); break; }
                Err(e) => {
                    let _ = progress_tx.send(BootstrapEvent::Log(format!(
                        "[{}] L2CAP attempt {}/5 failed: {} — retrying in 3s", tag, attempt, e
                    ))).await;
                    last_err = e;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
        result.ok_or(last_err).context(format!("[{}] L2CAP connect failed after 5 attempts", tag))?
    };

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] Sending #wifi_offer...", tag
    ))).await;
    let wifi_offer_frame = build_wifi_offer(ssid, psk, our_ip, EVENT_PORT, 120)?;
    let mut l2cap_payload = vec![r2_wire::FrameHeader::Complete.encode()];
    l2cap_payload.extend_from_slice(&wifi_offer_frame);
    let len = (l2cap_payload.len() as u16).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&l2cap_payload).await?;

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] #wifi_offer sent ({} bytes)", tag, l2cap_payload.len()
    ))).await;

    drop(stream);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] Waiting for UDP presence (RBID match)...", tag
    ))).await;

    let sensor_ip = wait_for_presence_rbid(presence_rx, sensor.rbid, Duration::from_secs(60))
        .await
        .context(format!("[{}] No presence received within 60s", tag))?;

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] Sensor presence received — IP: {}", tag, sensor_ip
    ))).await;

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] TCP connecting to {}:{}...", tag, sensor_ip, EVENT_PORT
    ))).await;

    let ping_start = Instant::now();

    let mut tcp = tokio::time::timeout(
        Duration::from_secs(5),
        TcpStream::connect(format!("{}:{}", sensor_ip, EVENT_PORT)),
    )
    .await
    .context(format!("[{}] TCP connect timed out", tag))?
    .context(format!("[{}] TCP connect failed", tag))?;

    let ping_frame = build_test_ping()?;
    let len = (ping_frame.len() as u16).to_be_bytes();
    tcp.write_all(&len).await?;
    tcp.write_all(&ping_frame).await?;
    tcp.flush().await?;

    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] test.ping sent", tag
    ))).await;

    let pong = tokio::time::timeout(Duration::from_secs(10), async {
        let mut len_buf = [0u8; 2];
        tcp.read_exact(&mut len_buf).await?;
        let frame_len = u16::from_be_bytes(len_buf) as usize;
        if frame_len == 0 || frame_len > 4096 {
            bail!("invalid response frame length: {}", frame_len);
        }
        let mut data = vec![0u8; frame_len];
        tcp.read_exact(&mut data).await?;
        let msg = decode_compact(&data)
            .map_err(|e| anyhow::anyhow!("wire decode: {:?}", e))?;
        let test_pong_hash =
            fnv::r2_hash("test.pong").map_err(|e| anyhow::anyhow!("hash: {:?}", e))?;
        if msg.header.event_hash != test_pong_hash {
            bail!(
                "expected test.pong (0x{:08X}), got 0x{:08X}",
                test_pong_hash,
                msg.header.event_hash
            );
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context(format!("[{}] test.pong timed out (10s)", tag))?;
    pong?;

    let rtt_ms = ping_start.elapsed().as_millis();
    let _ = progress_tx.send(BootstrapEvent::Log(format!(
        "[{}] Bootstrap OK: {} — ping RTT: {}ms", tag, sensor.addr, rtt_ms
    ))).await;

    Ok(BootstrapResult {
        addr: sensor.addr,
        sensor_ip,
        rtt_ms,
    })
}

/// Open L2CAP CoC stream with BT_SECURITY_LOW.
async fn l2cap_connect_low_security(
    target: Address,
    known_addr_type: AddressType,
    psm: u16,
) -> Result<bluer::l2cap::Stream> {
    let types = if known_addr_type == AddressType::LePublic {
        [AddressType::LePublic, AddressType::LeRandom]
    } else {
        [AddressType::LeRandom, AddressType::LePublic]
    };

    for addr_type in types {
        let sa = L2capSocketAddr::new(target, addr_type, psm);

        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let socket = Socket::<bluer::l2cap::Stream>::new_stream()?;
            socket.set_security(Security {
                level: SecurityLevel::Low,
                key_size: 0,
            })?;
            socket.bind(L2capSocketAddr::any_le())?;
            socket.connect(sa).await
        })
        .await;

        match result {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(_)) => {}
            Err(_) => {}
        }
    }

    bail!("L2CAP connect to {} failed (both address types)", target)
}

/// Build a #wifi_offer R2-WIRE compact frame.
fn build_wifi_offer(
    ssid: &str,
    psk: &str,
    ip: &str,
    port: u16,
    ttl_secs: u16,
) -> Result<Vec<u8>> {
    let event_hash = fnv::r2_hash("#wifi_offer")
        .map_err(|e| anyhow::anyhow!("hash error: {:?}", e))?;

    let payload = build_cbor_payload(&[
        (0, CborVal::Text(ssid)),
        (1, CborVal::Text(psk)),
        (2, CborVal::Text(ip)),
        (3, CborVal::UInt(port as u64)),
        (4, CborVal::UInt(ttl_secs as u64)),
    ]);

    let header = CompactHeader {
        version: 0,
        msg_type: MsgType::Event,
        flags: Flags {
            has_route: false,
            has_hmac: false,
            mcu_origin: false,
        },
        ttl: 5,
        k: 3,
        msg_id: rand_msg_id(),
        event_hash,
        target: 0x00000000,
    };

    let msg = CompactMessage {
        header,
        route: None,
        payload: &payload,
        hmac_tag: None,
    };

    let mut buf = [0u8; 254];
    let len = encode_compact(&msg, &mut buf)
        .map_err(|e| anyhow::anyhow!("encode: {:?}", e))?;
    Ok(buf[..len].to_vec())
}

/// Build a test.ping R2-WIRE compact frame with empty payload.
fn build_test_ping() -> Result<Vec<u8>> {
    let event_hash = fnv::r2_hash("test.ping")
        .map_err(|e| anyhow::anyhow!("hash error: {:?}", e))?;

    let payload = [0xA0u8];

    let header = CompactHeader {
        version: 0,
        msg_type: MsgType::Event,
        flags: Flags {
            has_route: false,
            has_hmac: false,
            mcu_origin: false,
        },
        ttl: 5,
        k: 0,
        msg_id: rand_msg_id(),
        event_hash,
        target: 0x00000000,
    };

    let msg = CompactMessage {
        header,
        route: None,
        payload: &payload,
        hmac_tag: None,
    };

    let mut buf = [0u8; 64];
    let len = encode_compact(&msg, &mut buf)
        .map_err(|e| anyhow::anyhow!("encode: {:?}", e))?;
    Ok(buf[..len].to_vec())
}

/// Wait for a UDP presence broadcast on the given port.
async fn wait_for_presence(port: u16, timeout: Duration) -> Result<String> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
        .context("bind UDP presence socket")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .ok();

    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            bail!("presence timeout");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = tokio::time::timeout(remaining, tokio::task::spawn_blocking({
            let socket = socket.try_clone()?;
            move || {
                let mut buf = [0u8; 512];
                socket.recv_from(&mut buf).map(|(n, addr)| (buf[..n].to_vec(), addr))
            }
        }))
        .await;

        match result {
            Ok(Ok(Ok((data, src_addr)))) => {
                if let Ok(val) = r2_core::cbor::decode(&data) {
                    if let r2_core::cbor::CborValue::Map(entries) = &val {
                        for (k, v) in entries {
                            if let (
                                r2_core::cbor::CborValue::UInt(1),
                                r2_core::cbor::CborValue::Text(payload_ip),
                            ) = (k, v)
                            {
                                let ip = if payload_ip == "0.0.0.0" || payload_ip.is_empty() {
                                    src_addr.ip().to_string()
                                } else {
                                    payload_ip.clone()
                                };

                                if ip == "0.0.0.0" {
                                    break;
                                }

                                return Ok(ip);
                            }
                        }
                    }
                }
            }
            Ok(Ok(Err(_))) => continue,
            Ok(Err(_)) => continue,
            Err(_) => bail!("presence timeout"),
        }
    }
}

/// Find the WiFi interface NOT carrying the default internet route.
fn find_free_wifi_interface() -> Option<String> {
    let route_out = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let route_str = String::from_utf8_lossy(&route_out.stdout);
    let internet_ifaces: Vec<&str> = route_str
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            while let Some(tok) = it.next() {
                if tok == "dev" {
                    return it.next();
                }
            }
            None
        })
        .collect();

    let nmcli_out = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE,STATE", "dev", "status"])
        .output()
        .ok()?;
    let nmcli_str = String::from_utf8_lossy(&nmcli_out.stdout);

    for line in nmcli_str.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 2 && parts[1] == "wifi" {
            let iface = parts[0];
            if !internet_ifaces.contains(&iface) {
                return Some(iface.to_string());
            }
        }
    }

    None
}

/// Get the IP address of the hotspot interface after creation.
fn get_hotspot_ip(iface: &str) -> Option<String> {
    std::thread::sleep(Duration::from_secs(2));
    let out = Command::new("nmcli")
        .args(["-t", "-f", "IP4.ADDRESS", "dev", "show", iface])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        if line.contains("IP4.ADDRESS") {
            let addr = line.split(':').nth(1)?;
            return Some(addr.split('/').next()?.to_string());
        }
    }
    None
}

/// Check if a WiFi hotspot is already active on `want_device` and return
/// (ssid, psk, ip) if so. The adapter filter is critical — a hotspot
/// landing on the WRONG (internet-carrying) adapter is precisely the
/// "vicious circle" bug where the spare adapter then joins it as client.
/// We only reuse a hotspot that's on the intended (spare) device.
fn find_active_hotspot_on(want_device: &str) -> Option<(String, String, Option<String>)> {
    // List active connections: NAME:TYPE:STATE:DEVICE
    let out = Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE,STATE,DEVICE", "con", "show", "--active"])
        .output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);

    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        if parts.len() < 4 { continue; }
        let (name, typ, state, device) = (parts[0], parts[1], parts[2], parts[3]);
        if typ != "802-11-wireless" || state != "activated" { continue; }
        if device != want_device { continue; }
        let mode_out = Command::new("nmcli")
            .args(["-t", "-g", "802-11-wireless.mode", "con", "show", name])
            .output().ok()?;
        let mode = String::from_utf8_lossy(&mode_out.stdout).trim().to_string();
        if mode != "ap" { continue; }

        let ssid_out = Command::new("nmcli")
            .args(["-t", "-g", "802-11-wireless.ssid", "con", "show", name])
            .output().ok()?;
        let ssid = String::from_utf8_lossy(&ssid_out.stdout).trim().to_string();
        if ssid.is_empty() { continue; }

        let psk = if ssid == HOTSPOT_SSID {
            HOTSPOT_PSK.to_string()
        } else {
            let psk_out = Command::new("nmcli")
                .args(["-s", "-t", "-g", "802-11-wireless-security.psk", "con", "show", name])
                .output().ok()?;
            let p = String::from_utf8_lossy(&psk_out.stdout).trim().to_string();
            if p.is_empty() { continue; }
            p
        };

        let ip = get_hotspot_ip(device);
        println!("[bootstrap] Reusing existing hotspot '{}' on {} (IP: {:?})", ssid, device, ip);
        return Some((ssid, psk, ip));
    }
    None
}

/// Tear down any active wifi-AP connection that is NOT on `want_device`.
/// Belt-and-braces against the "hotspot grabbed the internet adapter" bug:
/// if a previous activation landed on the wrong adapter, deactivate it so
/// (a) the internet adapter can re-associate to its real network, and
/// (b) the spare adapter is free to host a fresh hotspot.
fn teardown_misplaced_hotspots(want_device: &str) {
    let out = match Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE,STATE,DEVICE", "con", "show", "--active"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        if parts.len() < 4 { continue; }
        let (name, typ, state, device) = (parts[0], parts[1], parts[2], parts[3]);
        if typ != "802-11-wireless" || state != "activated" { continue; }
        if device == want_device { continue; }
        // Confirm it's actually an AP — don't tear down a station-mode connection.
        let mode_out = Command::new("nmcli")
            .args(["-t", "-g", "802-11-wireless.mode", "con", "show", name])
            .output().ok();
        let mode = mode_out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if mode != "ap" { continue; }
        eprintln!(
            "[bootstrap] tearing down misplaced hotspot '{}' on {} (should be on {})",
            name, device, want_device
        );
        let _ = Command::new("nmcli").args(["con", "down", name]).output();
    }
}

fn create_hotspot() -> Result<(String, String, Option<String>)> {
    // Pick the adapter that ISN'T carrying the default route. If both
    // wifi adapters are internet-connected, or only one wifi exists,
    // there's no safe place to host the hotspot — bail with a clear
    // message rather than risk taking the internet adapter offline.
    let free_iface = find_free_wifi_interface().ok_or_else(|| {
        anyhow!(
            "no spare wifi adapter free to host the hotspot \
             — every wifi interface is currently carrying the default route. \
             Disconnect one before retrying."
        )
    })?;

    // Reuse existing hotspot only if it's on the CORRECT adapter — a
    // misplaced one (e.g. NetworkManager auto-connected R2-rocker on the
    // internet adapter) is the exact failure we're guarding against, so
    // don't silently inherit it.
    if let Some((ssid, psk, ip)) = find_active_hotspot_on(&free_iface) {
        return Ok((ssid, psk, ip));
    }

    // Note: callers that want to cycle the hotspot (e.g. operator
    // re-pressing "Connect Sensors" to force every currently-connected
    // sensor to drop WiFi and re-bootstrap) call `cycle_hotspot` BEFORE
    // calling this function, so by the time we get here the previous
    // hotspot is already down and the fall-through below creates a
    // fresh one.

    // If a hotspot is active on the wrong adapter, tear it down first.
    // Otherwise NM will refuse to start a second hotspot on the spare
    // adapter while a duplicate SSID is broadcasting elsewhere.
    teardown_misplaced_hotspots(&free_iface);

    // Disconnect the spare adapter — if it's in client mode (which is
    // how the "vicious circle" begins, the spare auto-joining R2-rocker
    // hosted on the wrong adapter), nmcli won't repurpose it cleanly.
    let _ = Command::new("nmcli").args(["dev", "disconnect", &free_iface]).output();

    let ssid = HOTSPOT_SSID.to_string();
    let psk = HOTSPOT_PSK.to_string();

    // Always use `nmcli dev wifi hotspot ifname X` — never `con up <name>`
    // without ifname, because NM picks an adapter on its own and can land
    // on the wrong one. The explicit ifname binds the new hotspot to the
    // spare adapter regardless of any cached profile preferences.
    let nmcli_args: Vec<String> = vec![
        "dev".into(), "wifi".into(), "hotspot".into(),
        "ifname".into(), free_iface.clone(),
        "ssid".into(), ssid.clone(),
        "password".into(), psk.clone(),
    ];

    let output = Command::new("nmcli")
        .args(&nmcli_args)
        .output()
        .context("failed to run nmcli hotspot")?;

    let output = if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Not authorized") || stderr.contains("authorization") {
            let mut sudo_args = vec!["nmcli".to_string()];
            sudo_args.extend(nmcli_args.iter().cloned());
            Command::new("sudo")
                .args(&sudo_args)
                .output()
                .context("failed to run sudo nmcli hotspot")?
        } else {
            bail!("nmcli hotspot failed: {}", stderr.trim());
        }
    } else {
        output
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nmcli hotspot failed: {}", stderr.trim());
    }

    let hotspot_ip = get_hotspot_ip(&free_iface);
    Ok((ssid, psk, hotspot_ip))
}

/// Get first non-loopback IPv4 address.
fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// Generate a random 16-bit message ID.
fn rand_msg_id() -> u16 {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    (t.subsec_nanos() & 0xFFFF) as u16
}

/// Generate N random hex bytes as a lowercase hex string.
fn rand_hex(n: usize) -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut state = seed ^ 0xDEADBEEFCAFEBABE;
    let mut out = String::with_capacity(n * 2);
    for _ in 0..n {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let byte = (state >> 33) as u8;
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Generate a random alphanumeric string of length N.
fn rand_alphanum(n: usize) -> String {
    const CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKMNPQRSTUVWXYZ23456789";
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut state = seed ^ 0x0123456789ABCDEF;
    let mut out = String::with_capacity(n);
    for _ in 0..n {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((state >> 33) as usize) % CHARS.len();
        out.push(CHARS[idx] as char);
    }
    out
}

// ── Inline CBOR builder ──

enum CborVal<'a> {
    UInt(u64),
    Text(&'a str),
    #[allow(dead_code)]
    Bytes(&'a [u8]),
}

fn build_cbor_payload(pairs: &[(u64, CborVal<'_>)]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(128);

    let n = pairs.len();
    if n <= 23 {
        buf.push(0xA0 | n as u8);
    } else {
        buf.push(0xB8);
        buf.push(n as u8);
    }

    for (key, val) in pairs {
        cbor_encode_uint(&mut buf, *key);
        match val {
            CborVal::UInt(v) => cbor_encode_uint(&mut buf, *v),
            CborVal::Text(s) => {
                let bytes = s.as_bytes();
                if bytes.len() <= 23 {
                    buf.push(0x60 | bytes.len() as u8);
                } else if bytes.len() <= 255 {
                    buf.push(0x78);
                    buf.push(bytes.len() as u8);
                } else {
                    buf.push(0x79);
                    buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                }
                buf.extend_from_slice(bytes);
            }
            CborVal::Bytes(b) => {
                if b.len() <= 23 {
                    buf.push(0x40 | b.len() as u8);
                } else {
                    buf.push(0x58);
                    buf.push(b.len() as u8);
                }
                buf.extend_from_slice(b);
            }
        }
    }

    buf
}

fn cbor_encode_uint(buf: &mut Vec<u8>, v: u64) {
    if v <= 23 {
        buf.push(v as u8);
    } else if v <= 0xFF {
        buf.push(0x18);
        buf.push(v as u8);
    } else if v <= 0xFFFF {
        buf.push(0x19);
        buf.extend_from_slice(&(v as u16).to_be_bytes());
    } else if v <= 0xFFFF_FFFF {
        buf.push(0x1A);
        buf.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        buf.push(0x1B);
        buf.extend_from_slice(&v.to_be_bytes());
    }
}
