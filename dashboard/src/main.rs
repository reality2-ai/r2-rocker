//! R2 Dashboard Gateway
//!
//! Receives R2-WIRE event frames over TCP from sensor nodes,
//! serves a live web dashboard, and pushes data to browsers via WebSocket.
//! Integrates r2-bootstrap to trigger sensor discovery from the browser.
//!
//! Architecture:
//!   Sensor (M10/ESP32) --TCP:21042--> Gateway --WebSocket--> Browser
//!   Browser --WebSocket--> Gateway --TCP--> Sensor (commands)
//!   Browser --POST /api/bootstrap--> Gateway --BLE--> Sensor discovery

mod access;

use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use sha2::{Digest, Sha256};
use clap::Parser;
use r2_bootstrap::{BootstrapConfig, BootstrapEvent};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex, RwLock};

// ── r2-rocker event hashes ────────────────────────────────────────────────
//
// Per `SPEC-R2-ROCKER-WIRE.md` §2. Forked from the M10 demo dashboard's
// flat names ("acceleration", "battery_status") to the `r2.sensor.*`
// namespace defined in our wire spec, so multiple R2 applications can
// coexist on a hub without hash collisions.

const ACCELERATION:        u32 = r2_fnv::fnv1a_32(b"r2.sensor.acceleration");
const ACCELERATION_BATCH:  u32 = r2_fnv::fnv1a_32(b"r2.sensor.acceleration.batch");
const BATTERY:             u32 = r2_fnv::fnv1a_32(b"r2.sensor.battery");
const SENSOR_STATUS:       u32 = r2_fnv::fnv1a_32(b"r2.sensor.status");
const SENSOR_EVENT_LOG:    u32 = r2_fnv::fnv1a_32(b"r2.sensor.event.log");
const SENSOR_CAL_RESP:     u32 = r2_fnv::fnv1a_32(b"r2.sensor.cal.sample.resp");
const SENSOR_SYNC_PONG:    u32 = r2_fnv::fnv1a_32(b"r2.sensor.sync_pong");
const SENSOR_ANNOUNCE:     u32 = r2_fnv::fnv1a_32(b"r2.sensor.announce");

// Dashboard → sensor commands (SPEC-R2-ROCKER-WIRE §4 + SPEC-R2-ROCKER-TIMESYNC §4).
const DASH_ACK:               u32 = r2_fnv::fnv1a_32(b"r2.dash.ack");
const DASH_SYNC_PULSE:        u32 = r2_fnv::fnv1a_32(b"r2.dash.sync_pulse");
const DASH_SET_CLOCK_OFFSET:  u32 = r2_fnv::fnv1a_32(b"r2.dash.set_clock_offset");
// Capture session (SPEC-R2-ROCKER-CAPTURE §3).
const DASH_CAPTURE_START:     u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.start");
const DASH_CAPTURE_MARK:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.mark");
const DASH_CAPTURE_STOP:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.stop");
const SENSOR_CAPTURE_STATE:   u32 = r2_fnv::fnv1a_32(b"r2.sensor.capture.state");

/// Map hash → human-readable name shipped to the browser.
fn event_name(hash: u32) -> &'static str {
    match hash {
        ACCELERATION              => "r2.sensor.acceleration",
        ACCELERATION_BATCH        => "r2.sensor.acceleration.batch",
        BATTERY                   => "r2.sensor.battery",
        SENSOR_STATUS             => "r2.sensor.status",
        SENSOR_EVENT_LOG          => "r2.sensor.event.log",
        SENSOR_CAL_RESP           => "r2.sensor.cal.sample.resp",
        SENSOR_SYNC_PONG          => "r2.sensor.sync_pong",
        SENSOR_ANNOUNCE           => "r2.sensor.announce",
        SENSOR_CAPTURE_STATE      => "r2.sensor.capture.state",
        _                         => "unknown",
    }
}

/// ADXL355 raw-LSB → g conversion, per the datasheet at ±2 g range.
/// Used by the server-side payload remap so the browser sees g-values
/// directly. When we add per-frame range tagging (WIRE §3.2 key 10),
/// switch to indexing this by the announced range.
const LSB_PER_G_AT_2G: f64 = 256_000.0;

/// Server-side remap of integer-keyed CBOR payloads into named-key JSON
/// per `SPEC-R2-ROCKER-WIRE.md`. The browser expects friendly key names
/// ({"x":42}) rather than {"2":42} so this is where the per-event
/// schema knowledge lives. For acceleration, we also scale raw ADXL355
/// LSB values to g-units here so the chart code stays simple.
fn remap_payload(event_hash: u32, raw: serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let obj = match raw {
        Value::Object(m) => m,
        other => return other, // not a map — pass through
    };
    let take = |m: &Map<String, Value>, k: &str| -> Option<Value> { m.get(k).cloned() };
    let mut out = Map::new();

    // ── Acceleration: scale + rename ─────────────────────────────────────
    if event_hash == ACCELERATION {
        let scale = |v: Option<&Value>| -> Value {
            v.and_then(|x| x.as_i64())
                .map(|raw_lsb| (raw_lsb as f64 / LSB_PER_G_AT_2G))
                .and_then(|g| serde_json::Number::from_f64(g).map(Value::Number))
                .unwrap_or(Value::Null)
        };
        if let Some(v) = take(&obj, "0") { out.insert("seq".into(), v); }
        if let Some(v) = take(&obj, "1") { out.insert("ts_ms".into(), v); }
        out.insert("x".into(), scale(obj.get("2")));
        out.insert("y".into(), scale(obj.get("3")));
        out.insert("z".into(), scale(obj.get("4")));
        if let Some(v) = take(&obj, "10") { out.insert("range".into(), v); }
        return Value::Object(out);
    }

    let map_keys: &[(&str, &str)] = match event_hash {
        BATTERY      => &[("0", "voltage_mv"), ("1", "percent"), ("2", "charging"), ("3", "ts_ms"), ("10", "temp_c")],
        SENSOR_ANNOUNCE => &[
            ("0", "device_pk"),
            ("1", "hostname"),
            ("2", "fw_ver"),
            ("3", "last_seq"),
            ("4", "boot_ts_ms"),
            ("5", "nonce"),
            ("6", "sig"),
            ("10", "mounting_role"),
        ],
        SENSOR_STATUS => &[
            ("0", "state"),
            ("1", "uptime_ms"),
            ("2", "samples_total"),
            ("3", "samples_acked"),
            ("4", "sd_pct_used"),
            ("5", "rate_hz_active"),
            ("6", "range_active"),
            ("10", "error_code"),
        ],
        _ => {
            // Unknown event — return the raw map as-is.
            return Value::Object(obj);
        }
    };
    let mut consumed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (k_int, k_named) in map_keys {
        if let Some(v) = take(&obj, k_int) {
            out.insert((*k_named).to_string(), v);
            consumed.insert(*k_int);
        }
    }
    // Preserve any unmapped keys (forwards-compat per WIRE §1.3) — but
    // skip the integer keys we already turned into named ones.
    for (k, v) in obj {
        if !consumed.contains(k.as_str()) {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

#[derive(Parser)]
#[command(name = "r2-dashboard", about = "R2 sensor dashboard gateway")]
struct Args {
    /// HTTP port for the web dashboard
    #[arg(long, default_value = "8080")]
    http_port: u16,

    /// TCP port to listen for R2-WIRE events from sensors
    #[arg(long, default_value = "21042")]
    event_port: u16,

    /// Bind address
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// Phase 5 / SPEC-R2-ROCKER-ACCESS §3.4 — optional R2 relay URL
    /// embedded in invite tokens for off-network viewer enrolment.
    /// When unset, only the same-WiFi enrolment path is advertised.
    #[arg(long)]
    relay_url: Option<String>,

    /// Path to the rocker's WiFi config TOML (auto-generated by
    /// `tools/setup-hotspot.sh`). When set, the Link tab's invite
    /// modal shows a second QR encoding the hotspot's SSID + PSK
    /// in the standard `WIFI:T:WPA;...` form so a phone can join
    /// the hotspot before scanning the invite QR. Default
    /// `firmware/esp32-s3/devkitc/wifi_config.toml` — set explicitly
    /// to override or skip.
    #[arg(long, default_value = "firmware/esp32-s3/devkitc/wifi_config.toml")]
    wifi_config: String,
}

/// Build-stamped version string. Reported via /api/version, the startup
/// banner, and used by sensors / OTA logic to decide if an update is
/// needed (compare against `r2.sensor.announce.fw_ver`).
const DASHBOARD_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("R2_GIT_SHA"),
);

/// JSON for /api/version.
#[derive(Serialize)]
struct VersionInfo {
    version:   &'static str,
    git_sha:   &'static str,
    built_at:  &'static str,
    component: &'static str,
}

async fn version_handler() -> axum::Json<VersionInfo> {
    axum::Json(VersionInfo {
        version:   env!("CARGO_PKG_VERSION"),
        git_sha:   env!("R2_GIT_SHA"),
        built_at:  env!("R2_BUILD_TIMESTAMP"),
        component: "r2-rocker-dashboard",
    })
}

/// JSON message sent to browser via WebSocket
#[derive(Serialize, Clone, Debug)]
struct DashboardEvent {
    event: String,
    hash: String,
    timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
}

/// Connected sensor peer
#[derive(Debug)]
struct SensorPeer {
    #[allow(dead_code)]
    addr: SocketAddr,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    name: Option<String>,
    /// Most-recent `r2.sensor.announce` raw R2-WIRE frame bytes,
    /// cached so a freshly-connected /ws/raw viewer can be replayed
    /// the announce — otherwise it never sees `fw_ver`, `device_pk`,
    /// or `boot_ts_ms` because the announce only fires on TCP
    /// (re)connect, which already happened before the viewer arrived.
    last_announce: Option<Vec<u8>>,
    /// Per-peer time-sync state per SPEC-R2-ROCKER-TIMESYNC §3.
    /// Updated by both the sync_pulse-sender task and the sync_pong
    /// handler in the read loop, hence Mutex-wrapped.
    sync: Arc<Mutex<PeerSyncState>>,
}

/// Cristian's-algorithm time-sync state, per peer. The dashboard sends
/// `r2.dash.sync_pulse` on a schedule and processes incoming
/// `r2.sensor.sync_pong` to refine an exponentially-smoothed offset
/// estimate. When the estimate stabilises (or drifts past a threshold)
/// the dashboard pushes `r2.dash.set_clock_offset` so the sensor's
/// emitted `ts_ms` snaps onto the wall-clock timeline.
#[derive(Debug)]
struct PeerSyncState {
    connected_at: Instant,
    /// req_id → dashboard wall-clock at send time. Lookup on pong arrival
    /// gives us T1 for Cristian's math.
    pending: HashMap<u32, u64>,
    /// Recent offset_estimate values (in ms, as f64). Used for the
    /// stability check at calibration time.
    estimates: VecDeque<f64>,
    /// Exponential-smoothed residual offset, in ms. None until the
    /// first pong has been processed. Reset to 0 after each
    /// set_clock_offset push so it represents the residual on top of
    /// what the sensor has already applied.
    smoothed_offset_ms: Option<f64>,
    /// Total delta_ms pushed to this peer so far. Logged in timesync.log
    /// so analysis can reconstruct the boundary timing.
    cumulative_pushed_ms: i64,
    /// Has the initial calibration push happened yet?
    baseline_pushed: bool,
    /// Monotonically increasing req_id (wraps at u32 — irrelevant for
    /// our purposes since pending is rotated every sync round).
    next_req_id: u32,
}

impl PeerSyncState {
    fn new() -> Self {
        Self {
            connected_at: Instant::now(),
            pending: HashMap::new(),
            estimates: VecDeque::with_capacity(5),
            smoothed_offset_ms: None,
            cumulative_pushed_ms: 0,
            baseline_pushed: false,
            next_req_id: 1,
        }
    }
}

/// Build a dashboard → sensor R2-WIRE compact frame, TCP-framed (2-byte
/// length prefix). Mirrors the firmware's `wire::frame_for_tcp`, minus
/// the `mcu_origin` flag (we're the controller).
fn build_dash_frame(event_hash: u32, msg_id: u16, payload: &[u8]) -> Vec<u8> {
    let frame_len = 12 + payload.len();
    let mut out = Vec::with_capacity(2 + frame_len);
    out.extend_from_slice(&(frame_len as u16).to_be_bytes());
    out.push(0x00); // version=0, msg_type=Event=0, flags=0
    out.push((5 << 4) | (3 & 0x0F)); // ttl=5, k=3
    out.extend_from_slice(&msg_id.to_be_bytes());
    out.extend_from_slice(&event_hash.to_be_bytes());
    out.extend_from_slice(&[0u8; 4]); // target = broadcast
    out.extend_from_slice(payload);
    out
}

/// Encode `r2.dash.ack` payload `{0: through_seq, 1: dash_ts_ms}` per WIRE §4.1.
fn encode_dash_ack(through_seq: u32, dash_ts_ms: u64) -> Vec<u8> {
    let mut buf = [0u8; 32];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(2);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(through_seq as u64));
        let _ = enc.kv(1, &r2_cbor::Value::UInt(dash_ts_ms));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.sync_pulse` payload `{0: req_id, 1: dash_ts_ms}`.
fn encode_sync_pulse(req_id: u32, dash_ts_ms: u64) -> Vec<u8> {
    let mut buf = [0u8; 32];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(2);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(req_id as u64));
        let _ = enc.kv(1, &r2_cbor::Value::UInt(dash_ts_ms));
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.capture.mark` payload per SPEC-R2-ROCKER-CAPTURE §3.
///   `{0: ts_ms i64, 1: name str, 2: prefix str}` when a date prefix
///   like `"2026-05-18_13-35-00"` is supplied; otherwise the prefix
///   key is omitted and firmware falls back to `{ts_ms:016}` as the
///   filename stem.
fn encode_capture_mark(ts_ms: i64, name: &str, prefix: Option<&str>) -> Vec<u8> {
    let prefix_len = prefix.map(|p| p.len() + 4).unwrap_or(0);
    let mut buf = vec![0u8; 8 + 8 + name.len() + prefix_len + 8];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(if prefix.is_some() { 3 } else { 2 });
        let v_ts = if ts_ms >= 0 {
            r2_cbor::Value::UInt(ts_ms as u64)
        } else {
            r2_cbor::Value::NegInt(ts_ms)
        };
        let _ = enc.kv(0, &v_ts);
        let _ = enc.kv(1, &r2_cbor::Value::Text(name));
        if let Some(p) = prefix {
            let _ = enc.kv(2, &r2_cbor::Value::Text(p));
        }
        enc.len()
    };
    buf.truncate(used);
    buf
}

/// Encode `r2.dash.capture.start` / `r2.dash.capture.stop` empty payload (`{}`).
fn encode_empty_map() -> Vec<u8> {
    let mut buf = [0u8; 4];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(0);
        enc.len()
    };
    buf[..used].to_vec()
}

/// Encode `r2.dash.set_clock_offset` payload `{0: delta_ms}` (i64 signed).
fn encode_set_clock_offset(delta_ms: i64) -> Vec<u8> {
    let mut buf = [0u8; 16];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let v = if delta_ms >= 0 {
            r2_cbor::Value::UInt(delta_ms as u64)
        } else {
            r2_cbor::Value::NegInt(delta_ms)
        };
        let _ = enc.kv(0, &v);
        enc.len()
    };
    buf[..used].to_vec()
}

/// Decode `r2.sensor.sync_pong` payload `{0: req_id, 1: sensor_ts_ms}`.
/// Returns `(req_id, sensor_ts_ms)` on success.
fn decode_sync_pong(payload: &[u8]) -> Option<(u32, u64)> {
    let val = decode_cbor_payload(payload)?;
    let req_id = val.get("0").and_then(|v| v.as_u64())? as u32;
    let sensor_ts_ms = val.get("1").and_then(|v| v.as_u64())?;
    Some((req_id, sensor_ts_ms))
}

/// Current wall-clock ms since UNIX epoch — the dashboard's reference
/// timeline for sync_pulse / set_clock_offset math.
fn dash_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// SPEC-R2-ROCKER-TIMESYNC §3.1 + §3.2 — process an inbound sync_pong,
/// update the peer's smoothed offset, and push `r2.dash.set_clock_offset`
/// on the calibration or drift-threshold triggers.
async fn handle_sync_pong(
    addr: SocketAddr,
    req_id: u32,
    sensor_ts_ms: u64,
    state: &Arc<AppState>,
) {
    // Look up the matching pending pulse (T1) and the peer's mutable
    // sync state in one step. Then unconditionally drop the peers read
    // lock before pushing further frames so we don't hold it across an
    // await that could re-enter the same peer map.
    let (sync_arc, cmd_tx_opt) = {
        let peers = state.peers.read().await;
        match peers.get(&addr) {
            Some(p) => (p.sync.clone(), Some(p.tx.clone())),
            None    => return, // peer disappeared between read loop and here
        }
    };
    let mut s = sync_arc.lock().await;

    let t1 = match s.pending.remove(&req_id) {
        Some(t) => t,
        None => {
            // Stale or unexpected req_id — pruned by the 120 s window in
            // the sender task, or duplicated pong. Either way ignore.
            return;
        }
    };
    let t3 = dash_wall_ms();
    let rtt = t3.saturating_sub(t1) as f64;
    // Cristian's: offset = T1 + RTT/2 - T2
    let offset_estimate = (t1 as f64) + rtt / 2.0 - (sensor_ts_ms as f64);

    // Exponential smoothing per spec §3.1 (α = 0.2).
    const ALPHA: f64 = 0.2;
    let smoothed = match s.smoothed_offset_ms {
        Some(prev) => ALPHA * offset_estimate + (1.0 - ALPHA) * prev,
        None       => offset_estimate,
    };
    s.smoothed_offset_ms = Some(smoothed);

    // Track recent raw estimates for the stability check.
    if s.estimates.len() == 5 {
        s.estimates.pop_front();
    }
    s.estimates.push_back(offset_estimate);

    let elapsed = s.connected_at.elapsed();
    eprintln!(
        "[time-sync] {} rtt={:.1}ms est={:+.1}ms smoothed={:+.1}ms (round {}, {:.0}s since connect)",
        addr,
        rtt,
        offset_estimate,
        smoothed,
        s.estimates.len(),
        elapsed.as_secs_f64()
    );

    // Decide whether to push a correction (SPEC-R2-ROCKER-TIMESYNC §3.2).
    let push_decision: Option<(i64, &'static str)> = if !s.baseline_pushed {
        // Initial baseline. Need ≥ 5 rounds and a stable estimate
        // (std-dev of the last 3 estimates < 5 ms).
        if s.estimates.len() >= 5 && std_dev_last_n(&s.estimates, 3) < 5.0 {
            Some((smoothed.round() as i64, "baseline"))
        } else {
            None
        }
    } else if smoothed.abs() >= 10.0 {
        // Drift correction.
        Some((smoothed.round() as i64, "drift"))
    } else {
        None
    };

    if let Some((delta_ms, reason)) = push_decision {
        s.cumulative_pushed_ms = s.cumulative_pushed_ms.wrapping_add(delta_ms);
        s.baseline_pushed = true;
        // After pushing, the residual is zero by construction.
        s.smoothed_offset_ms = Some(0.0);
        s.estimates.clear();
        let cumulative = s.cumulative_pushed_ms;
        drop(s); // release the per-peer lock before awaiting the cmd send

        let payload = encode_set_clock_offset(delta_ms);
        let frame = build_dash_frame(
            DASH_SET_CLOCK_OFFSET,
            (req_id & 0xFFFF) as u16, // reuse the pong's req_id for trace
            &payload,
        );
        if let Some(tx) = cmd_tx_opt {
            if tx.send(frame).await.is_err() {
                eprintln!("[time-sync] {} push failed — cmd channel closed", addr);
            } else {
                eprintln!(
                    "[time-sync] {} pushed set_clock_offset delta={:+} ms ({}); cumulative={}",
                    addr, delta_ms, reason, cumulative
                );
                append_timesync_log(addr, delta_ms, reason, cumulative);
            }
        }
    }
}

fn std_dev_last_n(estimates: &VecDeque<f64>, n: usize) -> f64 {
    let take = estimates.len().min(n);
    if take < 2 {
        return f64::INFINITY;
    }
    let slice: Vec<f64> = estimates.iter().rev().take(take).copied().collect();
    let mean = slice.iter().sum::<f64>() / (take as f64);
    let var = slice.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (take as f64);
    var.sqrt()
}

/// Append one line to the per-process timesync log per SPEC §3.3.
/// JSON-per-line at the path under /tmp; later we'll move it into
/// `<data_root>/<experiment_id>/timesync.log` once data-root config
/// lands.
fn append_timesync_log(addr: SocketAddr, delta_ms: i64, reason: &str, cumulative_ms: i64) {
    use std::io::Write;
    let line = serde_json::json!({
        "ts_ms": dash_wall_ms(),
        "peer": addr.to_string(),
        "delta_ms": delta_ms,
        "cumulative_ms": cumulative_ms,
        "reason": reason,
    }).to_string();
    let path = "/tmp/r2-rocker-timesync.log";
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => { let _ = writeln!(f, "{}", line); }
        Err(e)    => eprintln!("[time-sync] failed to write {}: {}", path, e),
    }
}

/// One R2-WIRE frame as it arrived on the TCP listener, plus metadata
/// needed by the WASM viewer to know which peer it came from. This is
/// the message shape pushed on the `/ws/raw` WebSocket — the WASM hive
/// in the browser parses the envelope, then hands the inner frame to
/// `decode_compact_frame()`.
#[derive(Clone)]
struct RawFrame {
    /// Source socket address (e.g. "10.42.0.103:57768"), UTF-8.
    src: String,
    /// Wall-clock arrival time at the controller (ms since epoch).
    ts_ms: u64,
    /// The R2-WIRE compact frame bytes — same bytes the existing
    /// JSON-decoding path is fed (no length prefix).
    frame: Vec<u8>,
}

/// Shared application state
struct AppState {
    /// Broadcast channel for dashboard events → all (legacy) WebSocket clients
    event_tx: broadcast::Sender<DashboardEvent>,
    /// Phase 5d: broadcast channel for RAW R2-WIRE frames → WASM viewers.
    /// Same source frames, different output: raw bytes wrapped in a small
    /// envelope so the browser's WASM hive can decode in-process.
    raw_frame_tx: broadcast::Sender<RawFrame>,
    /// Connected sensor peers (for sending commands back)
    peers: RwLock<HashMap<SocketAddr, SensorPeer>>,
    /// Broadcast channel for raw JSON strings (used for bootstrap events → WS)
    ws_broadcast_tx: broadcast::Sender<String>,
    /// Bootstrap state
    bootstrap_running: Arc<AtomicBool>,
    bootstrap_log: Arc<Mutex<Vec<String>>>,
    /// Handle to the running bootstrap task — aborted on re-press
    bootstrap_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Cached snapshot of the latest available firmware (GitHub
    /// Releases tag + asset URLs, with local releases dir as fallback).
    /// Refreshed lazily by `firmware_available_handler` when older
    /// than `FIRMWARE_CACHE_TTL_SECS`.
    firmware_cache: Mutex<Option<FirmwareAvailable>>,
    /// Phase 5 — SPEC-R2-ROCKER-ACCESS state: TrustGroup, invite
    /// tokens, member side-cache. `None` when the operator hasn't
    /// generated a KeyHolder key yet; the /api/access/* routes
    /// return 503 in that case so the dashboard still boots.
    access: Option<access::AccessHandle>,
}

const FIRMWARE_CACHE_TTL_SECS: u64 = 300;
const GITHUB_OWNER_REPO: &str = "reality2-ai/r2-rocker";

#[derive(Clone, serde::Serialize)]
struct FirmwareAsset {
    carrier: String,    // "devkitc" or "xiao"
    version: String,    // exact fw_ver string baked in the .bin
    url: String,        // proxy URL the webapp fetches from (/api/firmware/...)
    size: Option<u64>,
}

#[derive(Clone, serde::Serialize)]
struct FirmwareAvailable {
    /// "github" if the GitHub query succeeded; "local" if only the
    /// on-disk releases directory had hits; "none" if neither.
    source: String,
    /// Common version string across the assets — typically the
    /// GitHub release tag, or the highest-mtime fw_ver in the local
    /// releases dir.
    version: String,
    /// One entry per carrier.
    assets: Vec<FirmwareAsset>,
    /// Optional error/warning when GitHub was tried but failed.
    note: Option<String>,
    /// Unix-ms when this snapshot was taken, for cache age display.
    fetched_at_ms: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let (event_tx, _) = broadcast::channel::<DashboardEvent>(256);
    let (raw_frame_tx, _) = broadcast::channel::<RawFrame>(1024);
    let (ws_broadcast_tx, _) = broadcast::channel::<String>(256);

    // Phase 5: try to load the KeyHolder signing key. A successful load
    // unlocks /api/access/*; a failure logs + leaves Access disabled.
    // local_origin is what we'll embed in `url_local` per
    // SPEC-R2-ROCKER-ACCESS §4.1 step 4 — same host:port the webapp is
    // served on.
    let local_origin = format!("http://{}:{}", args.bind, args.http_port);
    let wifi_config_path = if args.wifi_config.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&args.wifi_config))
    };
    let access_handle = access::maybe_load(
        local_origin,
        args.relay_url.clone(),
        wifi_config_path,
    ).await;

    let state = Arc::new(AppState {
        event_tx: event_tx.clone(),
        raw_frame_tx: raw_frame_tx.clone(),
        peers: RwLock::new(HashMap::new()),
        ws_broadcast_tx,
        bootstrap_running: Arc::new(AtomicBool::new(false)),
        bootstrap_log: Arc::new(Mutex::new(Vec::new())),
        bootstrap_task: Mutex::new(None),
        firmware_cache: Mutex::new(None),
        access: access_handle,
    });

    // Spawn TCP listener for R2-WIRE events
    let event_state = state.clone();
    let event_bind = format!("{}:{}", args.bind, args.event_port);
    tokio::spawn(async move {
        run_event_listener(&event_bind, event_state).await;
    });

    // HTTP server with WASM viewer + WebSocket + bootstrap API.
    // The legacy `/` HTML dashboard and `/ws` bidirectional channel were
    // removed once the WASM viewer at the repo's webapp/ became feature-
    // complete. The WASM viewer consumes /ws/raw + /ws/status instead.
    let mut app = Router::new()
        // Phase 5d: raw R2-WIRE frame forwarder for WASM viewers.
        .route("/ws/raw", get({
            let ws_state = state.clone();
            move |ws| ws_raw_handler(ws, ws_state)
        }))
        // Phase 5d: text-JSON status channel — bootstrap progress, hotspot
        // lifecycle, server warnings. WASM viewers open this alongside
        // /ws/raw (per SPEC-R2-ROCKER-DASHBOARD §5.3).
        .route("/ws/status", get({
            let ws_state = state.clone();
            move |ws| ws_status_handler(ws, ws_state)
        }))
        // Per-sensor live log tail. Opens a TCP connection to the sensor's
        // log_tcp listener (port 21046) and pipes lines back as WS text
        // frames. Used by the per-card "↓ Logs" panel in the webapp.
        .route("/ws/logs/{addr}", get(ws_logs_handler))
        // Phase 5d: TG public key + KeyHolder enrolment endpoints.
        .route("/api/keyholder/tg-pub", get(tg_pub_handler))
        // SPEC-R2-ROCKER-ACCESS §4 — viewer enrolment lifecycle.
        // KeyHolder-only routes (invite, members, revoke) are gated by
        // a localhost check in v0.1 per §11.1 (2); claim is public
        // because the token IS the auth.
        .route("/api/access/invite", post(access_invite_handler))
        .route("/api/access/claim", post(access_claim_handler))
        .route("/api/access/members", get(access_members_handler))
        .route("/api/access/revoke/{device_pk}", post(access_revoke_handler))
        // Legacy stubs from before the ACCESS spec landed — still
        // marked DEPRECATED in SPEC-R2-ROCKER-DASHBOARD §5.1.
        .route("/api/enrol-init", post(enrol_init_handler))
        .route("/api/enrol-complete", post(enrol_complete_handler))
        .route("/api/bootstrap", post(bootstrap_handler))
        .route("/api/bootstrap/status", get(bootstrap_status_handler))
        // Phase 9-light: stream a firmware .bin to a sensor's OTA listener.
        .route("/api/ota/{addr}", post(ota_push_handler))
        // SPEC-R2-ROCKER-SENSOR-REMOTE-RESET: push a CMD_RESET to a sensor's
        // reset listener (TCP 21044). Triggers esp_restart() on the sensor.
        .route("/api/sensor/{addr}/reset", post(reset_push_handler))
        // Firmware availability: returns the latest release per
        // carrier (GitHub Releases primary, local releases/ dir
        // fallback). 5-minute cache. Webapp diffs against each peer's
        // announce fw_ver for the "needs update" dot.
        .route("/api/firmware/available", get(firmware_available_handler))
        .route("/api/firmware/{carrier}/binary", get(firmware_binary_handler))
        // SPEC-R2-ROCKER-CAPTURE — named experimental captures.
        // Start triggers an immediate sync_pulse round (per §7.1) +
        // capture.start fan-out; Mark fans out capture.mark with the
        // dashboard's chosen ts_ms; Stop fans out capture.stop.
        .route("/api/capture/start", post(capture_start_handler))
        .route("/api/capture/mark",  post(capture_mark_handler))
        .route("/api/capture/stop",  post(capture_stop_handler))
        // SPEC-R2-ROCKER-CAPTURE — capture-file listing / fetch / delete.
        // Each route opens a fresh TCP connection to <addr>:21047 on
        // the sensor and proxies the data_tcp wire protocol.
        .route("/api/data/{addr}/list",        get(data_list_handler))
        .route("/api/data/{addr}/file/{name}", get(data_get_handler).delete(data_delete_handler))
        .route("/api/data/{addr}/all",         axum::routing::delete(data_delete_all_handler))
        .route("/api/data/merged",             get(data_merged_handler))
        .route("/api/version", get(version_handler));

    // Serve the WASM viewer (webapp/) as the dashboard root if the
    // directory exists. Same-origin with the dashboard's WS endpoints
    // means no CORS dance for the browser. fallback_service ensures
    // the explicit /api/ and /ws/ routes win; everything else falls
    // through to the static asset server.
    let viewer_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("webapp"));
    if let Some(dir) = viewer_dir.as_ref().filter(|d| d.is_dir()) {
        app = app.fallback_service(tower_http::services::ServeDir::new(dir));
        eprintln!("[webapp] mounted webapp/ at /  ({})", dir.display());
    } else {
        eprintln!("[webapp] webapp/ not found — UI disabled");
    }

    let app = app.with_state(state.clone());

    let http_addr: SocketAddr = format!("{}:{}", args.bind, args.http_port)
        .parse()
        .expect("valid bind address");

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║              r2-rocker dashboard                              ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  version:    {:<48}║", DASHBOARD_VERSION);
    eprintln!("║  built:      {:<48}║", env!("R2_BUILD_TIMESTAMP"));
    eprintln!("║  dashboard:  http://{:<41}║", http_addr.to_string());
    eprintln!("║  events:     tcp/{:<44}║", args.event_port);
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    let listener = tokio::net::TcpListener::bind(http_addr).await
        .unwrap_or_else(|e| {
            eprintln!("ERROR: Cannot bind HTTP port {} — {}", http_addr, e);
            eprintln!("Is another r2-dashboard already running? Kill it first: pkill r2-dashboard");
            std::process::exit(1);
        });
    // ConnectInfo<SocketAddr> is required by the /api/access/* handlers
    // (KeyHolder gating is a localhost-only check in v0.1 per
    // SPEC-R2-ROCKER-ACCESS §11.1). Without `with_connect_info`, axum
    // panics when those handlers try to extract.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

/// POST /api/bootstrap — trigger sensor bootstrap (re-pressing cancels and restarts)
async fn bootstrap_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Abort any existing bootstrap task and wait for it to clean up
    {
        let mut task = state.bootstrap_task.lock().await;
        if let Some(handle) = task.take() {
            handle.abort();
            // Small delay so the task drops cleanly before we restart
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }
    }

    state.bootstrap_running.store(true, Ordering::SeqCst);

    // Clear previous log and broadcast a reset event so the browser clears its panel
    {
        let mut log = state.bootstrap_log.lock().await;
        log.clear();
    }
    let reset_msg = serde_json::json!({ "type": "bootstrap", "event": { "Reset": null } }).to_string();
    let _ = state.ws_broadcast_tx.send(reset_msg);

    let config = BootstrapConfig {
        ssid: None,
        psk: None,
        scan_secs: 10,
        // Reverse-DNS class identifier (R2-BEACON §4); FNV-1a-32 hashed
        // on the wire to 0x6A3B0860. Sensor firmware (Phase 6) MUST
        // advertise the same string. See SPEC-R2-ROCKER-DASHBOARD §6.3.
        target_class: "nz.ac.auckland.rocker.sensor".to_string(),
        // Always cycle the hotspot on a fresh bootstrap press. Sensors
        // currently joined to the existing hotspot will lose WiFi for
        // a few seconds and fall back to BLE advertising, which is the
        // only path through which `run_bootstrap` can re-push
        // credentials. Without this, pressing "Connect Sensors" while
        // a sensor is already streaming does nothing for that sensor.
        cycle_hotspot: true,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<BootstrapEvent>(64);
    let ws_tx = state.ws_broadcast_tx.clone();
    let log_store = state.bootstrap_log.clone();
    let running_flag = state.bootstrap_running.clone();

    // Spawn the event relay task
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            // Build WS message
            let ws_msg = serde_json::json!({
                "type": "bootstrap",
                "event": event,
            });
            let json_str = serde_json::to_string(&ws_msg).unwrap_or_default();

            // Append to log
            {
                let mut log = log_store.lock().await;
                log.push(json_str.clone());
            }

            // Broadcast to all WS clients
            let _ = ws_tx.send(json_str);
        }

        running_flag.store(false, Ordering::SeqCst);
    });

    // Spawn the bootstrap task and store the handle for cancellation
    let bootstrap_handle = tokio::spawn(async move {
        if let Err(e) = r2_bootstrap::run_bootstrap(config, tx.clone()).await {
            let _ = tx.send(BootstrapEvent::Error(format!("{}", e))).await;
        }
        // Drop tx to signal the relay task to finish
    });
    *state.bootstrap_task.lock().await = Some(bootstrap_handle);

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({ "status": "started" })),
    )
}

/// POST /api/ota/{addr} — Phase 9-light, push a firmware binary to a sensor's
/// OTA listener (TCP 21043). Body is the raw `.bin`. Returns JSON describing
/// the result: bytes sent, sha256 hex, the receiver's status code + message.
///
/// `addr` may be either an IP ("10.42.0.103") or `ip:port` from the connected-
/// peers list (the port is replaced with 21043 in either case).
async fn ota_push_handler(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    use std::net::ToSocketAddrs;

    // Strip any sensor TCP port if the caller pasted in `ip:port` from
    // the peers list — OTA always lands on the well-known port.
    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let ota_target = format!("{}:21043", ip_only);

    if body.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "empty body"})),
        );
    }

    eprintln!("[ota] push to {} ({} bytes)", ota_target, body.len());
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "ota",
        "phase": "uploading",
        "target": ota_target,
        "size": body.len(),
    }).to_string());

    // Resolve so DNS errors fail fast (we expect numeric IPs but be safe).
    let socket = match ota_target.to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None    => return ota_err(&state, &ota_target, "no addr resolved"),
        },
        Err(e) => return ota_err(&state, &ota_target, &format!("resolve: {e}")),
    };

    // Pre-compute the SHA-256 over the full firmware blob.
    let sha: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&body);
        h.finalize().into()
    };

    // 60 s should be ample for a ~1.4 MB blob over 802.11 + write into flash.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        push_firmware(socket, &body, &sha),
    )
    .await;

    match result {
        Ok(Ok((status_byte, msg))) => {
            let ok = status_byte == 0x00; // STATUS_OK in r2-esp::ota_tcp
            let phase = if ok { "applied" } else { "rejected" };
            let _ = state.ws_broadcast_tx.send(serde_json::json!({
                "type": "ota",
                "phase": phase,
                "target": ota_target,
                "message": msg,
            }).to_string());
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "ok": ok,
                    "size": body.len(),
                    "sha256": hex::encode(&sha),
                    "status_byte": status_byte,
                    "message": msg,
                })),
            )
        }
        Ok(Err(e))  => ota_err(&state, &ota_target, &format!("push: {e}")),
        Err(_)      => ota_err(&state, &ota_target, "timed out after 60 s"),
    }
}

fn ota_err(state: &Arc<AppState>, target: &str, msg: &str) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    eprintln!("[ota] {} — {}", target, msg);
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "ota",
        "phase": "error",
        "target": target,
        "message": msg,
    }).to_string());
    (
        axum::http::StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({"ok": false, "error": msg})),
    )
}

/// Drives the OTA-receive protocol from `r2-esp::ota_tcp` (R2-OTA TCP):
///   START preamble: cmd(1) + size_le(4) + sha256(32)
///   firmware bytes
///   half-close (write shutdown) → receiver flushes + writes partition
///   response: status(1) + len_le(2) + utf-8 message
async fn push_firmware(
    target: SocketAddr,
    body: &[u8],
    sha: &[u8; 32],
) -> std::io::Result<(u8, String)> {
    const CMD_START: u8 = 0x01;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;

    // Preamble
    let mut preamble = Vec::with_capacity(37);
    preamble.push(CMD_START);
    preamble.extend_from_slice(&(body.len() as u32).to_le_bytes());
    preamble.extend_from_slice(sha);
    stream.write_all(&preamble).await?;

    // Stream in 64 KiB chunks.
    for chunk in body.chunks(65536) {
        stream.write_all(chunk).await?;
    }
    stream.flush().await?;

    // Half-close write side; receiver uses this as EOF for the firmware
    // stream, then writes the partition + sends a response.
    let _ = stream.shutdown().await;

    // Response: status(1) + len(2 LE) + message
    let mut hdr = [0u8; 3];
    stream.read_exact(&mut hdr).await?;
    let status = hdr[0];
    let msg_len = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
    let mut msg = vec![0u8; msg_len];
    if msg_len > 0 {
        stream.read_exact(&mut msg).await?;
    }
    Ok((status, String::from_utf8_lossy(&msg).into_owned()))
}

/// POST /api/sensor/{addr}/reset — per SPEC-R2-ROCKER-SENSOR-REMOTE-RESET.
/// Sends a single CMD_RESET (0x10) byte to the sensor's reset listener
/// (TCP 21044) and returns the receiver's status + message. The sensor
/// reboots ~100 ms after responding.
///
/// `addr` may be `ip` or `ip:port`; the streaming port is stripped and
/// 21044 is always used.
async fn reset_push_handler(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
) -> impl IntoResponse {
    use std::net::ToSocketAddrs;

    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let reset_target = format!("{}:21044", ip_only);

    eprintln!("[reset] push to {}", reset_target);
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "reset",
        "phase": "requested",
        "target": reset_target,
    }).to_string());

    let socket = match reset_target.to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None    => return reset_err(&state, &reset_target, "no addr resolved"),
        },
        Err(e) => return reset_err(&state, &reset_target, &format!("resolve: {e}")),
    };

    // 8 s is generous — a healthy sensor responds in <100 ms.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        push_reset(socket),
    )
    .await;

    match result {
        Ok(Ok((status_byte, msg))) => {
            let ok = status_byte == 0x00; // STATUS_OK in r2-esp::reset_tcp
            let phase = if ok { "applied" } else { "error" };
            let _ = state.ws_broadcast_tx.send(serde_json::json!({
                "type": "reset",
                "phase": phase,
                "target": reset_target,
                "message": msg,
            }).to_string());
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "ok": ok,
                    "status_byte": status_byte,
                    "message": msg,
                })),
            )
        }
        Ok(Err(e)) => reset_err(&state, &reset_target, &format!("push: {e}")),
        Err(_)     => reset_err(&state, &reset_target, "timed out after 8 s"),
    }
}

fn reset_err(state: &Arc<AppState>, target: &str, msg: &str) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    eprintln!("[reset] {} — {}", target, msg);
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "reset",
        "phase": "error",
        "target": target,
        "message": msg,
    }).to_string());
    (
        axum::http::StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({"ok": false, "error": msg})),
    )
}

/// Drives the reset protocol from `r2-esp::reset_tcp`:
///   CMD_RESET(1) → status(1) + len_le(2) + message
async fn push_reset(target: SocketAddr) -> std::io::Result<(u8, String)> {
    const CMD_RESET: u8 = 0x10;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;

    stream.write_all(&[CMD_RESET]).await?;
    stream.flush().await?;

    let mut hdr = [0u8; 3];
    stream.read_exact(&mut hdr).await?;
    let status = hdr[0];
    let msg_len = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
    let mut msg = vec![0u8; msg_len];
    if msg_len > 0 {
        stream.read_exact(&mut msg).await?;
    }
    Ok((status, String::from_utf8_lossy(&msg).into_owned()))
}

/// GET /api/bootstrap/status — return bootstrap state
async fn bootstrap_status_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let running = state.bootstrap_running.load(Ordering::SeqCst);
    let log = state.bootstrap_log.lock().await;
    Json(serde_json::json!({
        "running": running,
        "log": *log,
    }))
}

/// Listen for TCP connections from sensor nodes
async fn run_event_listener(bind: &str, state: Arc<AppState>) {
    let listener = TcpListener::bind(bind).await.unwrap_or_else(|e| {
        eprintln!("ERROR: Cannot bind event port {} — {}", bind, e);
        eprintln!("Is another r2-dashboard already running? Kill it first: pkill r2-dashboard");
        std::process::exit(1);
    });
    eprintln!("[events] listening on {}", bind);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                eprintln!("[events] sensor connected: {}", addr);
                // Enable TCP keepalive so zombie connections (sensor dropped without
                // FIN) are detected within ~60s rather than the 2-hour OS default.
                let stream = {
                    let std_stream = match stream.into_std() {
                        Ok(s) => s,
                        Err(_) => { continue; }
                    };
                    let sock = socket2::Socket::from(std_stream);
                    sock.set_keepalive(true).ok();
                    let ka = socket2::TcpKeepalive::new()
                        .with_time(std::time::Duration::from_secs(15))
                        .with_interval(std::time::Duration::from_secs(5));
                    sock.set_tcp_keepalive(&ka).ok();
                    let std_stream: std::net::TcpStream = sock.into();
                    std_stream.set_nonblocking(true).ok();
                    match tokio::net::TcpStream::from_std(std_stream) {
                        Ok(s) => s,
                        Err(_) => { continue; }
                    }
                };
                let peer_state = state.clone();
                tokio::spawn(async move {
                    handle_sensor_connection(stream, addr, peer_state).await;
                });
            }
            Err(e) => eprintln!("[events] accept error: {}", e),
        }
    }
}

/// Handle a single sensor TCP connection
async fn handle_sensor_connection(stream: TcpStream, addr: SocketAddr, state: Arc<AppState>) {
    let (mut reader, mut writer) = stream.into_split();

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    let sync_state = Arc::new(Mutex::new(PeerSyncState::new()));
    {
        let mut peers = state.peers.write().await;
        peers.insert(addr, SensorPeer {
            addr,
            tx: cmd_tx.clone(),
            name: None,
            last_announce: None,
            sync: sync_state.clone(),
        });
    }

    // Per-peer sync_pulse task. Per SPEC-R2-ROCKER-TIMESYNC §3.1 cadence:
    // 1 Hz for the first 30 s after this TCP connect, then 30 s thereafter.
    // Exits when the cmd_tx send fails (peer disconnected, channel closed).
    let sync_tx = cmd_tx.clone();
    let sync_state_for_task = sync_state.clone();
    let sync_addr = addr;
    let _sync_handle = tokio::spawn(async move {
        let fast_until = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let interval = if Instant::now() < fast_until {
                std::time::Duration::from_secs(1)
            } else {
                std::time::Duration::from_secs(30)
            };
            // Acquire a req_id and record the dashboard-side T1 before
            // sending, so the pong handler can look it up by req_id.
            let (req_id, dash_ts) = {
                let mut s = sync_state_for_task.lock().await;
                let id = s.next_req_id;
                s.next_req_id = s.next_req_id.wrapping_add(1);
                let t1 = dash_wall_ms();
                s.pending.insert(id, t1);
                // Prune very old entries (>120 s) to avoid leaking
                // memory if pongs are persistently dropped.
                let cutoff = t1.saturating_sub(120_000);
                s.pending.retain(|_, t| *t >= cutoff);
                (id, t1)
            };
            let payload = encode_sync_pulse(req_id, dash_ts);
            let frame = build_dash_frame(
                DASH_SYNC_PULSE,
                (req_id & 0xFFFF) as u16,
                &payload,
            );
            if sync_tx.send(frame).await.is_err() {
                // cmd_rx side closed — peer is gone.
                eprintln!("[time-sync] {} cmd channel closed; sync task exiting", sync_addr);
                return;
            }
            tokio::time::sleep(interval).await;
        }
    });

    let _timestamp_start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let read_state = state.clone();
    let read_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        let mut frame_buf = Vec::new();
        // Decimate live acceleration to ~10 Hz for the browser per
        // SPEC-R2-ROCKER-DASHBOARD §5.2 ("at most 10 samples/sec per
        // peer per browser tab on the live wire, with the rest
        // decimated"). Source rate is 100 Hz from the firmware, so
        // we push every 10th. The full stream lands in the SD ring
        // when Phase 3 is implemented; until then dropped samples
        // are simply not displayed (gaps are harmless for a sine
        // wave demo).
        const ACCEL_DECIMATION: u32 = 10;
        let mut accel_n: u32 = 0;

        // ACK tracking per SPEC-R2-ROCKER-WIRE §4.1. We emit a
        // `r2.dash.ack {through_seq, dash_ts_ms}` to the sensor at
        // most every ACK_PERIOD_MS or every ACK_SAMPLES received
        // acceleration frames, whichever first. The firmware uses
        // `through_seq` to free SD ring segments
        // (SPEC-R2-ROCKER-SENSOR §7.4); without these acks the ring
        // fills up. We track max_seq_seen locally so a stuck/
        // out-of-order frame can't cause us to ack the wrong
        // through_seq.
        const ACK_PERIOD_MS: u64 = 200;
        const ACK_SAMPLES: u32 = 100;
        let mut max_seq_seen: u32 = 0;
        let mut samples_since_ack: u32 = 0;
        let mut next_ack_at = tokio::time::Instant::now()
            + std::time::Duration::from_millis(ACK_PERIOD_MS);
        let mut ack_msg_id: u16 = 1;

        loop {
            // 5 s read deadline. The sensor sends `r2.sensor.status` every
            // 2 s plus continuous 10 Hz acceleration; if 5 s pass with no
            // bytes, the peer is gone (chip reset / WiFi drop / hard
            // crash). Forces the read loop to exit fast so the caller's
            // `peer_disconnected` broadcast goes out, rather than waiting
            // for the kernel's 60 s TCP keepalive timeout.
            let read_result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                reader.read(&mut buf),
            ).await;
            let read_outcome = match read_result {
                Ok(r) => r,
                Err(_) => {
                    eprintln!("[events] read timeout from {} (no traffic in 5 s) — closing", addr);
                    break;
                }
            };
            match read_outcome {
                Ok(0) => break,
                Ok(n) => {
                    frame_buf.extend_from_slice(&buf[..n]);

                    while frame_buf.len() >= 2 {
                        let frame_len = ((frame_buf[0] as usize) << 8) | (frame_buf[1] as usize);
                        if frame_buf.len() < 2 + frame_len {
                            break;
                        }

                        let frame = frame_buf[2..2 + frame_len].to_vec();
                        frame_buf.drain(..2 + frame_len);

                        // Phase 5d: broadcast the raw frame to any connected
                        // WASM viewers BEFORE we decimate, so they get the
                        // full stream and decimate themselves if they want
                        // (the WASM hive owns its own throttling). The
                        // legacy JSON path below still decimates per
                        // ACCEL_DECIMATION.
                        let _ = read_state.raw_frame_tx.send(RawFrame {
                            src: addr.to_string(),
                            ts_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            frame: frame.clone(),
                        });

                        // R2-WIRE compact frame (SPEC-R2-ROCKER-WIRE §1.4):
                        // byte 0:    version|msg_type|flags
                        // byte 1:    ttl|k
                        // bytes 2-3: msg_id (BE u16)
                        // bytes 4-7: event_hash (BE u32)
                        // bytes 8-11: target (BE u32)
                        // bytes 12+: payload
                        let event_hash = if frame.len() >= 8 {
                            Some(((frame[4] as u32) << 24)
                                | ((frame[5] as u32) << 16)
                                | ((frame[6] as u32) << 8)
                                | (frame[7] as u32))
                        } else {
                            None
                        };

                        // SPEC-R2-ROCKER-WIRE §4.1 — observe ACCELERATION frames
                        // to track max_seq_seen for periodic r2.dash.ack
                        // emission. Triggers a send when ACK_SAMPLES (or
                        // ACK_PERIOD_MS, below) has passed.
                        if event_hash == Some(ACCELERATION) && frame.len() > 12 {
                            if let Some(payload) = decode_cbor_payload(&frame[12..]) {
                                if let Some(seq) = payload.get("0").and_then(|v| v.as_u64()) {
                                    let seq32 = seq as u32;
                                    if seq32 > max_seq_seen {
                                        max_seq_seen = seq32;
                                    }
                                    samples_since_ack = samples_since_ack.saturating_add(1);
                                }
                            }
                        }
                        if event_hash == Some(ACCELERATION_BATCH) && frame.len() > 12 {
                            // For batched frames we'd ideally walk the
                            // inner records to pick up the LAST seq. v0.1
                            // sensors don't emit batches yet (catch-up
                            // mode is deferred); leave a TODO once they do.
                        }

                        // SPEC-R2-ROCKER-TIMESYNC §3 — handle sync_pong inline,
                        // update peer's smoothed offset, push set_clock_offset
                        // when stable or when drift threshold exceeded.
                        if event_hash == Some(SENSOR_SYNC_PONG) && frame.len() > 12 {
                            if let Some((req_id, sensor_ts_ms)) = decode_sync_pong(&frame[12..]) {
                                handle_sync_pong(
                                    addr,
                                    req_id,
                                    sensor_ts_ms,
                                    &read_state,
                                ).await;
                            }
                        }

                        if event_hash == Some(SENSOR_ANNOUNCE) {
                            let payload = if frame.len() > 12 {
                                decode_cbor_payload(&frame[12..])
                                    .map(|p| remap_payload(SENSOR_ANNOUNCE, p))
                            } else {
                                None
                            };
                            // Our spec calls the friendly label "hostname" (per
                            // SPEC-R2-ROCKER-WIRE §3.1 key 1); the legacy M10
                            // schema used "name". Try both.
                            let device_name = payload.as_ref()
                                .and_then(|p| p.get("hostname").or_else(|| p.get("name")))
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string());

                            // Phase 5b — Ed25519-verify the announce signature.
                            // TOFU policy in v0.1: log-only; don't reject yet
                            // (the legacy M10 sender doesn't sign, so a strict
                            // policy would lock us out of mixed deployments).
                            // Migrate to reject-on-bad-sig once all sensors
                            // are r2-rocker-spec firmware.
                            let sig_ok = payload
                                .as_ref()
                                .map(verify_announce_signature)
                                .unwrap_or(SigStatus::NoPayload);

                            eprintln!(
                                "[events] sensor.announce from {}: name={:?} sig={:?} payload={:?}",
                                addr, device_name, sig_ok, payload
                            );

                            // Cache the announce frame bytes per peer so a
                            // /ws/raw viewer that connects later can be
                            // replayed — otherwise it misses `fw_ver` /
                            // `device_pk` / `boot_ts_ms` until the next
                            // sensor reboot.
                            {
                                let mut peers = read_state.peers.write().await;
                                if let Some(peer) = peers.get_mut(&addr) {
                                    if let Some(ref name_str) = device_name {
                                        peer.name = Some(name_str.clone());
                                    }
                                    peer.last_announce = Some(frame.clone());
                                }
                            }

                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            let event = DashboardEvent {
                                event: "sensor.connected".to_string(),
                                hash: format!("0x{:08X}", SENSOR_ANNOUNCE),
                                timestamp_ms: now,
                                // Pass the announce payload through so the browser
                                // can display fw_ver / device_pk / boot_ts_ms.
                                // Required for OTA decision logic later.
                                payload: payload.clone(),
                                source_addr: Some(addr.to_string()),
                                device_name,
                            };
                            let _ = read_state.event_tx.send(event);
                        } else if let Some(mut event) = decode_event_frame(&frame, &addr) {
                            {
                                let peers = read_state.peers.read().await;
                                if let Some(peer) = peers.get(&addr) {
                                    event.device_name = peer.name.clone();
                                }
                            }

                            // Decimate live acceleration before broadcasting
                            // and before logging — high-rate events (100 Hz)
                            // would otherwise overrun the browser's render
                            // budget AND spam the console.
                            let is_accel = event.event == "r2.sensor.acceleration";
                            let emit_live = if is_accel {
                                let due = accel_n == 0;
                                accel_n = (accel_n + 1) % ACCEL_DECIMATION;
                                due
                            } else {
                                true
                            };
                            if emit_live {
                                // Per-frame logging removed — at 10 Hz live + 0.5 Hz status
                                // it filled the log faster than anyone could read it. Frames
                                // are still observable via /ws/raw (binary) or /ws (legacy
                                // JSON). Keep stderr for connection-lifecycle events only.
                                let _ = read_state.event_tx.send(event);
                            }
                        }

                        // Per WIRE §4.1: send r2.dash.ack at the
                        // earlier of ACK_PERIOD_MS or ACK_SAMPLES
                        // received. Frees the firmware's SD ring
                        // (SPEC-R2-ROCKER-SENSOR §7.4). No-op if we
                        // haven't observed any acceleration frames
                        // yet (max_seq_seen still 0).
                        let now = tokio::time::Instant::now();
                        let should_ack = max_seq_seen > 0
                            && (samples_since_ack >= ACK_SAMPLES || now >= next_ack_at);
                        if should_ack {
                            let dash_ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let payload = encode_dash_ack(max_seq_seen, dash_ts);
                            let frame_bytes = build_dash_frame(
                                DASH_ACK,
                                ack_msg_id,
                                &payload,
                            );
                            ack_msg_id = ack_msg_id.wrapping_add(1);
                            // Send via the peer's writer mpsc. Don't
                            // hold the peers lock across await; collect
                            // the tx once if available.
                            let tx = {
                                let peers = read_state.peers.read().await;
                                peers.get(&addr).map(|p| p.tx.clone())
                            };
                            if let Some(tx) = tx {
                                if tx.send(frame_bytes).await.is_err() {
                                    // Writer half died — peer is gone.
                                    // The session will tear down via the
                                    // top-level select on read/write handles.
                                }
                            }
                            samples_since_ack = 0;
                            next_ack_at = now
                                + std::time::Duration::from_millis(ACK_PERIOD_MS);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[events] read error from {}: {}", addr, e);
                    break;
                }
            }
        }
    });

    let write_handle = tokio::spawn(async move {
        while let Some(frame) = cmd_rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = read_handle => {}
        _ = write_handle => {}
    }

    {
        let mut peers = state.peers.write().await;
        peers.remove(&addr);
    }
    eprintln!("[events] sensor disconnected: {}", addr);
    // Push a `peer_disconnected` message on /ws/status so the WASM
    // viewer can flip the sensor's virtual LED to inert grey instantly,
    // rather than waiting for its OFFLINE_MS timeout (~6 s) to fire.
    let msg = serde_json::json!({
        "type": "peer_disconnected",
        "addr": addr.to_string(),
    }).to_string();
    let _ = state.ws_broadcast_tx.send(msg);
}

/// Decode an R2-WIRE event frame into a DashboardEvent
fn decode_event_frame(frame: &[u8], addr: &SocketAddr) -> Option<DashboardEvent> {
    if frame.len() < 7 {
        return None;
    }

    // R2-WIRE compact frame (12-byte fixed header, SPEC-R2-ROCKER-WIRE §1.4):
    //   byte 0:    version|msg_type|flags
    //   byte 1:    ttl|k
    //   bytes 2-3: msg_id (BE u16)
    //   bytes 4-7: event_hash (BE u32)
    //   bytes 8-11: target (BE u32)
    //   bytes 12+: payload
    if frame.len() < 12 {
        return None;
    }
    let _byte0 = frame[0];
    let _byte1 = frame[1];
    let _msg_id = ((frame[2] as u16) << 8) | (frame[3] as u16);
    let event_hash = ((frame[4] as u32) << 24)
        | ((frame[5] as u32) << 16)
        | ((frame[6] as u32) << 8)
        | (frame[7] as u32);
    // bytes 8-11 = target (broadcast 0 for r2-rocker — see firmware/src/wire.rs)

    let payload_bytes = &frame[12..];

    let payload = if !payload_bytes.is_empty() {
        decode_cbor_payload(payload_bytes).map(|p| remap_payload(event_hash, p))
    } else {
        None
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Some(DashboardEvent {
        event: event_name(event_hash).to_string(),
        hash: format!("0x{:08X}", event_hash),
        timestamp_ms: now,
        payload,
        source_addr: Some(addr.to_string()),
        device_name: None,
    })
}

/// Result of `verify_announce_signature` — `Valid` is the only "good"
/// state. Other variants log loudly so misconfiguration is visible.
#[derive(Debug, Clone, Copy)]
enum SigStatus {
    Valid,
    /// Signature bytes don't verify against the announced device_pk.
    /// Means either the firmware is buggy, the network is forging
    /// announces, or the canonical CBOR re-encoding doesn't match.
    BadSignature,
    /// Required field missing / wrong type. Often a legacy M10 announce
    /// (no signature field at all) — log-and-accept under TOFU for now.
    Malformed,
    /// No payload at all — same as legacy.
    NoPayload,
}

/// Phase 5b — re-encode the canonical body (keys 0..5) per
/// SPEC-R2-ROCKER-WIRE §3.1 and Ed25519-verify the signature at key 6.
///
/// The firmware signs over the canonical CBOR encoding of keys 0..5.
/// Both sides use deterministic CBOR (smallest-form heads, ascending
/// integer keys), so a fresh encode here MUST match the firmware's
/// signed bytes exactly.
fn verify_announce_signature(payload: &serde_json::Value) -> SigStatus {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let obj = match payload.as_object() {
        Some(o) => o,
        None => return SigStatus::Malformed,
    };

    let hex_field = |key: &str, len: usize| -> Option<Vec<u8>> {
        let s = obj.get(key)?.as_str()?;
        let b = hex::decode(s).ok()?;
        if b.len() == len { Some(b) } else { None }
    };
    let text_field = |key: &str| -> Option<&str> {
        obj.get(key)?.as_str()
    };
    let uint_field = |key: &str| -> Option<u64> {
        obj.get(key)?.as_u64()
    };

    let (Some(device_pk), Some(hostname), Some(fw_ver), Some(last_seq), Some(boot_ts_ms), Some(nonce), Some(sig)) = (
        hex_field("device_pk", 32),
        text_field("hostname"),
        text_field("fw_ver"),
        uint_field("last_seq"),
        uint_field("boot_ts_ms"),
        hex_field("nonce", 16),
        hex_field("sig", 64),
    ) else {
        return SigStatus::Malformed;
    };

    // Refuse the all-zero placeholder sig that pre-Phase-5a firmware emits.
    if sig.iter().all(|b| *b == 0) {
        return SigStatus::Malformed;
    }

    // Re-encode the canonical body bytes. Keys MUST be in ascending order
    // (we write 0..5 directly) and integer-keyed for byte-identical output
    // with the firmware's inline encoder.
    let mut body_buf = vec![0u8; 256 + hostname.len() + fw_ver.len()];
    let mut enc = r2_cbor::Encoder::new(&mut body_buf);
    if enc.map(6).is_err()
        || enc.kv(0, &r2_cbor::Value::Bytes(&device_pk)).is_err()
        || enc.kv(1, &r2_cbor::Value::Text(hostname)).is_err()
        || enc.kv(2, &r2_cbor::Value::Text(fw_ver)).is_err()
        || enc.kv(3, &r2_cbor::Value::UInt(last_seq)).is_err()
        || enc.kv(4, &r2_cbor::Value::UInt(boot_ts_ms)).is_err()
        || enc.kv(5, &r2_cbor::Value::Bytes(&nonce)).is_err()
    {
        return SigStatus::Malformed;
    }
    let body = enc.as_bytes();

    let pk_arr: [u8; 32] = device_pk.as_slice().try_into().unwrap();
    let sig_arr: [u8; 64] = sig.as_slice().try_into().unwrap();
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_arr) else {
        return SigStatus::Malformed;
    };
    let signature = Signature::from_bytes(&sig_arr);
    if verifying_key.verify(body, &signature).is_ok() {
        SigStatus::Valid
    } else {
        SigStatus::BadSignature
    }
}

/// Decode CBOR payload into JSON
fn decode_cbor_payload(data: &[u8]) -> Option<serde_json::Value> {
    let mut decoder = r2_cbor::Decoder::new(data);
    cbor_to_json(&mut decoder).ok()
}

/// Recursively convert CBOR items to serde_json::Value
fn cbor_to_json(decoder: &mut r2_cbor::Decoder) -> Result<serde_json::Value, ()> {
    match decoder.next().map_err(|_| ())? {
        r2_cbor::Item::UInt(v) => Ok(serde_json::Value::Number(v.into())),
        r2_cbor::Item::NegInt(v) => Ok(serde_json::Value::Number(v.into())),
        r2_cbor::Item::Bytes(b) => {
            Ok(serde_json::Value::String(hex::encode(b)))
        }
        r2_cbor::Item::Text(s) => {
            Ok(serde_json::Value::String(String::from_utf8_lossy(s).into_owned()))
        }
        r2_cbor::Item::Array(n) => {
            let mut arr = Vec::new();
            for _ in 0..n {
                arr.push(cbor_to_json(decoder)?);
            }
            Ok(serde_json::Value::Array(arr))
        }
        r2_cbor::Item::Map(n) => {
            let mut map = serde_json::Map::new();
            for _ in 0..n {
                let key = cbor_to_json(decoder)?;
                let val = cbor_to_json(decoder)?;
                let key_str = match key {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                map.insert(key_str, val);
            }
            Ok(serde_json::Value::Object(map))
        }
        r2_cbor::Item::Bool(b) => Ok(serde_json::Value::Bool(b)),
        r2_cbor::Item::Null => Ok(serde_json::Value::Null),
        r2_cbor::Item::Float16Raw(bits) => {
            let f = f32::from_bits(half_to_f32_bits(bits)) as f64;
            Ok(serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        r2_cbor::Item::Float32(f) => {
            Ok(serde_json::Number::from_f64(f as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        r2_cbor::Item::Float64(f) => {
            Ok(serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
    }
}

/// Convert IEEE 754 half-precision (16-bit) to single-precision (32-bit) bits
fn half_to_f32_bits(h: u16) -> u32 {
    let sign = (h >> 15) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;

    if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            let mut e = 0u32;
            let mut m = mant;
            while (m & 0x400) == 0 {
                m <<= 1;
                e += 1;
            }
            (sign << 31) | ((127 - 15 - e) << 23) | ((m & 0x3FF) << 13)
        }
    } else if exp == 31 {
        (sign << 31) | (0xFF << 23) | (mant << 13)
    } else {
        (sign << 31) | ((exp + 112) << 23) | (mant << 13)
    }
}

/// Hex helpers (kept local to avoid pulling in the external crate just
/// for these two functions; the `hex` 0.4 dep IS in Cargo.toml for
/// other consumers but this local module shadows it inside main.rs).
mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Decode a hex string to bytes. Accepts lowercase or uppercase. Returns
    /// `Err(())` on any non-hex character or odd length.
    pub fn decode(s: &str) -> Result<Vec<u8>, ()> {
        let bytes = s.as_bytes();
        if bytes.len() % 2 != 0 { return Err(()); }
        let nibble = |c: u8| -> Result<u8, ()> {
            match c {
                b'0'..=b'9' => Ok(c - b'0'),
                b'a'..=b'f' => Ok(c - b'a' + 10),
                b'A'..=b'F' => Ok(c - b'A' + 10),
                _ => Err(()),
            }
        };
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            out.push((nibble(chunk[0])? << 4) | nibble(chunk[1])?);
        }
        Ok(out)
    }
}

// ── Phase 5d — endpoints for the WASM viewer ──────────────────────────────

/// `/ws/raw` — push raw R2-WIRE frame bytes to a connected WASM viewer.
///
/// Each WS binary message is one frame, wrapped in a small TLV envelope:
///
/// ```
///   [u16 BE: src_addr length n]
///   [n bytes UTF-8: src_addr]
///   [u32 BE: ts_ms_low32]
///   [u16 BE: frame length m]
///   [m bytes:  R2-WIRE compact frame]
/// ```
///
/// Source addr lets the browser key per-peer state. ts_ms is the
/// controller's wall-clock arrival time (low 32 bits — wraps every
/// ~49 days, matches the firmware's ts_ms field width). Frame is the
/// raw R2-WIRE compact frame: header + payload, no transport prefix.
async fn ws_raw_handler(
    ws: WebSocketUpgrade,
    state: Arc<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_raw(socket, state))
}

async fn handle_ws_raw(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.raw_frame_tx.subscribe();
    eprintln!("[ws/raw] viewer connected");

    // Replay cached announce frames per peer so a freshly-connected
    // viewer sees `fw_ver` / `device_pk` / `boot_ts_ms` immediately,
    // not "after the next sensor reboot." The announce only fires on
    // TCP (re)connect, so without replay a viewer that arrives mid-
    // session never learns these fields. Use the actual reception
    // timestamp where we have it; fall back to "now" otherwise.
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let peers = state.peers.read().await;
        for (addr, peer) in peers.iter() {
            if let Some(ref frame) = peer.last_announce {
                let envelope = encode_raw_frame_envelope(&RawFrame {
                    src: addr.to_string(),
                    ts_ms: now_ms,
                    frame: frame.clone(),
                });
                if socket.send(Message::Binary(envelope.into())).await.is_err() {
                    return;
                }
            }
        }
    }

    loop {
        tokio::select! {
            // Inbound: viewer might send pings or commands later. For
            // now we just drain and discard; close on close-frame.
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // ignore other inbound messages for now
                }
            }
            // Outbound: a fresh raw frame from the TCP listener.
            frame_msg = rx.recv() => {
                match frame_msg {
                    Ok(rf) => {
                        let envelope = encode_raw_frame_envelope(&rf);
                        if socket.send(Message::Binary(envelope.into())).await.is_err() {
                            break;
                        }
                    }
                    // Lagged — viewer fell behind. Skip the gap; live data
                    // is preferred over backfill on the live wire (the
                    // SD ring is the durability layer).
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    eprintln!("[ws/raw] viewer disconnected");
}

/// `/ws/status` — text JSON status channel for the WASM viewer.
///
/// Carries non-frame events: bootstrap progress (BLE scan / `#wifi_offer`
/// send / sensor online / completion), hotspot lifecycle, server warnings.
/// Frame data is on `/ws/raw` (binary). Per SPEC-R2-ROCKER-DASHBOARD §5.3.
async fn ws_status_handler(
    ws: WebSocketUpgrade,
    state: Arc<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_status(socket, state))
}

async fn handle_ws_status(mut socket: WebSocket, state: Arc<AppState>) {
    eprintln!("[ws/status] viewer connected");

    // Replay the persisted bootstrap log so a late-joining viewer sees
    // the in-flight discovery progress immediately, rather than waiting
    // for the next event.
    {
        let log = state.bootstrap_log.lock().await;
        for entry in log.iter() {
            if socket.send(Message::Text(entry.clone().into())).await.is_err() {
                return;
            }
        }
    }

    let mut rx = state.ws_broadcast_tx.subscribe();
    loop {
        tokio::select! {
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // browser → server reserved for future use
                }
            }
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    eprintln!("[ws/status] viewer disconnected");
}

/// `/ws/logs/{addr}` — per-sensor live log tail.
///
/// Opens a TCP socket to `<addr>:21046` (the firmware's `log_tcp`
/// listener) and pipes each newline-terminated line back to the WS
/// client as a text frame. Closes when either side disconnects.
///
/// `addr` may be either a bare IP or `ip:port`; the sensor port suffix
/// is stripped since the log listener is on the well-known port.
async fn ws_logs_handler(
    Path(addr): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_logs(socket, addr))
}

async fn handle_ws_logs(mut socket: WebSocket, addr: String) {
    let ip_only: &str = addr.split(':').next().unwrap_or(&addr);
    let target = format!("{}:21046", ip_only);
    eprintln!("[ws/logs] viewer requested tail of {}", target);

    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        TcpStream::connect(&target),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let _ = socket
                .send(Message::Text(
                    format!("[ws/logs] connect to {} failed: {}\n", target, e).into(),
                ))
                .await;
            return;
        }
        Err(_) => {
            let _ = socket
                .send(Message::Text(
                    format!("[ws/logs] connect to {} timed out\n", target).into(),
                ))
                .await;
            return;
        }
    };

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        tokio::select! {
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // ignore client → server messages
                }
            }
            n = reader.read_line(&mut line) => {
                match n {
                    Ok(0) => break, // sensor closed the socket
                    Ok(_) => {
                        if socket.send(Message::Text(line.clone().into())).await.is_err() {
                            break;
                        }
                        line.clear();
                    }
                    Err(_) => break,
                }
            }
        }
    }
    eprintln!("[ws/logs] tail of {} closed", target);
}

/// `GET /api/firmware/available` — latest firmware snapshot.
///
/// Tries GitHub Releases first (latest non-draft release on the
/// `reality2-ai/r2-rocker` repo); falls back to the highest-mtime
/// .bin in `firmware/esp32-s3/<carrier>/releases/`. Cached for
/// `FIRMWARE_CACHE_TTL_SECS` so the webapp can poll every few
/// seconds without hammering the GitHub API rate limit (60/hr
/// unauthenticated per IP).
async fn firmware_available_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    {
        let cache = state.firmware_cache.lock().await;
        if let Some(ref entry) = *cache {
            let age_s = (now_ms.saturating_sub(entry.fetched_at_ms)) / 1000;
            if age_s < FIRMWARE_CACHE_TTL_SECS {
                return (axum::http::StatusCode::OK, Json(serde_json::to_value(entry).unwrap_or(serde_json::json!({})))).into_response();
            }
        }
    }

    let snapshot = build_firmware_snapshot(now_ms).await;

    {
        let mut cache = state.firmware_cache.lock().await;
        *cache = Some(snapshot.clone());
    }

    (axum::http::StatusCode::OK, Json(serde_json::to_value(&snapshot).unwrap_or(serde_json::json!({})))).into_response()
}

/// `GET /api/firmware/{carrier}/binary` — fetch the matching .bin.
///
/// If the cached snapshot was sourced from GitHub, redirects (302) to
/// the release asset URL — the browser then fetches the bytes from
/// GitHub's CDN directly. If sourced from a local releases dir, the
/// dashboard streams the file from disk.
async fn firmware_binary_handler(
    State(state): State<Arc<AppState>>,
    Path(carrier): Path<String>,
) -> impl IntoResponse {
    let snapshot = {
        let cache = state.firmware_cache.lock().await;
        cache.clone()
    };
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            // No cache yet — synthesise one. Webapp normally hits
            // /available before /binary, so this is a corner case.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let snap = build_firmware_snapshot(now_ms).await;
            let mut cache = state.firmware_cache.lock().await;
            *cache = Some(snap.clone());
            snap
        }
    };

    let asset = snapshot.assets.iter().find(|a| a.carrier == carrier);
    let asset = match asset {
        Some(a) => a,
        None => return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("no firmware available for carrier {}", carrier) })),
        ).into_response(),
    };

    if snapshot.source == "github" {
        // Proxy the asset through the dashboard rather than 302-ing
        // the browser to GitHub's CDN — GitHub release-download URLs
        // don't include `Access-Control-Allow-Origin`, so a redirect
        // from a webapp `fetch()` gets blocked by CORS. Streaming
        // via curl here keeps the request same-origin from the
        // browser's perspective.
        let asset_url = asset.url.clone();
        let output = tokio::process::Command::new("curl")
            .args([
                "-sSL",                // follow redirects (GH issues a redirect to S3)
                "--max-time", "60",
                "-H", "User-Agent: r2-rocker-dashboard",
                &asset_url,
            ])
            .output()
            .await;
        return match output {
            Ok(out) if out.status.success() => (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                out.stdout,
            ).into_response(),
            Ok(out) => (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("curl proxy of {} failed: status {}", asset_url, out.status),
                    "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
                })),
            ).into_response(),
            Err(e) => (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("curl spawn failed: {}", e) })),
            ).into_response(),
        };
    }

    // Local source — read the file from disk and stream it back.
    let path = std::path::PathBuf::from(&asset.url); // "url" is the local path for local source
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        ).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("read {:?}: {}", path, e) })),
        ).into_response(),
    }
}

/// Build a fresh FirmwareAvailable snapshot by querying GitHub then
/// falling back to the local releases dir.
async fn build_firmware_snapshot(now_ms: u64) -> FirmwareAvailable {
    // Query GitHub Releases AND scan the local releases/ dir in
    // parallel, then pick whichever is newer. v0.1 of this endpoint
    // preferred GitHub unconditionally, which broke the day-to-day
    // dev loop: every fresh local build was ignored in favour of the
    // stale GitHub Release tag. v0.2 compares the local newest-mtime
    // against the GitHub release's `published_at` and picks the
    // newer one, so a freshly-built local .bin always wins until the
    // operator cuts a fresh tag.

    let local = local_firmware_snapshot();
    let github = github_firmware_snapshot().await;

    let prefer_local = match (&local, &github) {
        (Some((_, l_mtime, _)), Some((_, g_secs, _))) => *l_mtime > *g_secs,
        (Some(_), None)  => true,
        (None,    Some(_)) => false,
        (None, None) => return FirmwareAvailable {
            source: "none".to_string(),
            version: String::new(),
            assets: Vec::new(),
            note: Some("No firmware found on GitHub or in local releases/".to_string()),
            fetched_at_ms: now_ms,
        },
    };

    if prefer_local {
        let (assets, _, latest_version) = local.expect("checked above");
        FirmwareAvailable {
            source: "local".to_string(),
            version: latest_version,
            assets,
            note: github.as_ref().map(|(tag, _, _)| {
                format!("Local build is newer than GitHub release {} — preferring local.", tag)
            }),
            fetched_at_ms: now_ms,
        }
    } else {
        let (tag, _, assets) = github.expect("checked above");
        FirmwareAvailable {
            source: "github".to_string(),
            version: tag,
            assets,
            note: None,
            fetched_at_ms: now_ms,
        }
    }
}

/// Pick the newest `.bin` per carrier under
/// `firmware/esp32-s3/<carrier>/releases/`. Returns
/// `(assets, max_mtime_unix_secs, version_string)` or `None` if
/// neither carrier has any local builds.
fn local_firmware_snapshot() -> Option<(Vec<FirmwareAsset>, i64, String)> {
    let mut assets = Vec::new();
    let mut latest_version = String::new();
    let mut max_mtime_secs: i64 = i64::MIN;

    for carrier in &["devkitc", "xiao"] {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("firmware/esp32-s3").join(carrier).join("releases"));
        let Some(dir) = dir else { continue };
        if !dir.is_dir() { continue; }

        let mut best: Option<(std::time::SystemTime, std::path::PathBuf, u64)> = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("bin") { continue; }
                let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                let size = meta.len();
                let pick = match &best {
                    Some((t, _, _)) => mtime > *t,
                    None => true,
                };
                if pick { best = Some((mtime, path, size)); }
            }
        }
        if let Some((mtime, path, size)) = best {
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let version = fname
                .strip_prefix("r2-rocker-firmware-")
                .and_then(|s| s.strip_suffix(".bin"))
                .unwrap_or(fname)
                .to_string();
            if version > latest_version { latest_version = version.clone(); }
            let mtime_secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if mtime_secs > max_mtime_secs { max_mtime_secs = mtime_secs; }
            assets.push(FirmwareAsset {
                carrier: carrier.to_string(),
                version,
                url: path.to_string_lossy().into_owned(),
                size: Some(size),
            });
        }
    }

    if assets.is_empty() { None } else { Some((assets, max_mtime_secs, latest_version)) }
}

/// Query GitHub Releases. Returns `(tag, published_at_unix_secs,
/// assets)` or `None` if the request failed / the latest release has
/// no matching `.bin` assets.
async fn github_firmware_snapshot() -> Option<(String, i64, Vec<FirmwareAsset>)> {
    let gh_url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_OWNER_REPO,
    );
    let output = tokio::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time", "5",
            "-H", "Accept: application/vnd.github+json",
            "-H", "User-Agent: r2-rocker-dashboard",
            &gh_url,
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() { return None; }
    let body = String::from_utf8(output.stdout).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name").and_then(|v| v.as_str())?.to_string();
    let published_secs = json
        .get("published_at")
        .and_then(|v| v.as_str())
        .and_then(iso_to_unix_secs)
        .unwrap_or(0);

    let mut assets = Vec::new();
    if let Some(arr) = json.get("assets").and_then(|v| v.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = a.get("browser_download_url").and_then(|v| v.as_str()).unwrap_or("");
            let size = a.get("size").and_then(|v| v.as_u64());
            if !name.ends_with(".bin") { continue; }
            let carrier = if name.contains("devkitc") {
                "devkitc"
            } else if name.contains("xiao") {
                "xiao"
            } else {
                continue;
            };
            assets.push(FirmwareAsset {
                carrier: carrier.to_string(),
                version: tag.clone(),
                url: url.to_string(),
                size,
            });
        }
    }
    if assets.is_empty() { return None; }
    Some((tag, published_secs, assets))
}

/// Tiny ISO-8601 ("2026-05-18T07:36:42Z") → unix seconds parser.
/// We pull this in rather than adding chrono just for one date
/// field. Returns `None` on malformed input.
fn iso_to_unix_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 { return None; }
    let y  = std::str::from_utf8(&b[0..4]).ok()?.parse::<i32>().ok()?;
    let mo = std::str::from_utf8(&b[5..7]).ok()?.parse::<u32>().ok()?;
    let d  = std::str::from_utf8(&b[8..10]).ok()?.parse::<u32>().ok()?;
    let h  = std::str::from_utf8(&b[11..13]).ok()?.parse::<u32>().ok()?;
    let mi = std::str::from_utf8(&b[14..16]).ok()?.parse::<u32>().ok()?;
    let se = std::str::from_utf8(&b[17..19]).ok()?.parse::<u32>().ok()?;
    if mo < 1 || mo > 12 { return None; }
    // Howard Hinnant's days-from-civil algorithm.
    let y_adj = y - if mo <= 2 { 1 } else { 0 };
    let era = if y_adj >= 0 { y_adj / 400 } else { (y_adj - 399) / 400 };
    let yoe = (y_adj - era * 400) as u32;
    let m_num = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * m_num + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era as i64) * 146097 + doe as i64 - 719468;
    Some(days * 86400 + (h as i64) * 3600 + (mi as i64) * 60 + se as i64)
}

#[cfg(test)]
mod firmware_snapshot_tests {
    use super::iso_to_unix_secs;
    #[test]
    fn iso_epoch() { assert_eq!(iso_to_unix_secs("1970-01-01T00:00:00Z"), Some(0)); }
    #[test]
    fn iso_known() {
        // 2026-05-18T07:36:42Z = 1779435402
        assert_eq!(iso_to_unix_secs("2026-05-18T07:36:42Z"), Some(1779435402));
    }
}

// ── SPEC-R2-ROCKER-CAPTURE handlers ───────────────────────────────────

#[derive(serde::Deserialize)]
struct CaptureMarkBody {
    name: String,
    /// Optional pre-formatted local-time stem like `"2026-05-18_13-35-00"`.
    /// The webapp builds this with `Intl.DateTimeFormat` so the operator
    /// sees the file dated in their local timezone (the dashboard avoids
    /// the localtime/TZ rabbit hole by trusting the browser). Firmware
    /// uses `<prefix>-<name>.csv`; if absent, falls back to ts_ms-padded.
    #[serde(default)]
    prefix: Option<String>,
}

/// Fan a frame out to every connected peer's tx channel. Returns the
/// count of peers reached. Failures (channel full / closed) are
/// logged but do not abort the fan-out — fleet ops are best-effort.
async fn fan_out_dash_frame(
    state: &AppState,
    event_hash: u32,
    msg_id: u16,
    payload: Vec<u8>,
) -> usize {
    let frame = build_dash_frame(event_hash, msg_id, &payload);
    let peers = state.peers.read().await;
    let mut sent = 0;
    for (addr, peer) in peers.iter() {
        match peer.tx.send(frame.clone()).await {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[capture] fan-out to {} failed: {}", addr, e),
        }
    }
    sent
}

/// `POST /api/capture/start` — refresh time-sync, then send
/// `r2.dash.capture.start` to every peer (SPEC-R2-ROCKER-CAPTURE §7.1).
async fn capture_start_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Fire an immediate sync_pulse round to every peer so capture
    // timestamps in the upcoming session share a tightly-refreshed
    // baseline. The dashboard's existing sync_pong handler smooths
    // the result via Cristian's algorithm and emits
    // r2.dash.set_clock_offset to each peer asynchronously — we
    // don't await pongs here, just kick the round.
    let dash_ts_ms = dash_wall_ms();
    {
        let peers = state.peers.read().await;
        for (addr, peer) in peers.iter() {
            // Per-peer req_id is the low 32 bits of the
            // dashboard wall-clock — collision-free within a session.
            let req_id = (dash_ts_ms & 0xFFFF_FFFF) as u32;
            let payload = encode_sync_pulse(req_id, dash_ts_ms);
            let frame = build_dash_frame(
                DASH_SYNC_PULSE,
                (req_id & 0xFFFF) as u16,
                &payload,
            );
            let _ = peer.tx.send(frame).await;
            // Note: track this req_id on the peer's PeerSyncState so
            // the pong handler can match. We use the regular periodic
            // task's state map here. Without registering, the pong
            // smoothing still works but RTT is wrong for THIS round.
            // For a forced refresh we accept that tradeoff — the
            // periodic task picks up the next round anyway.
            let _ = addr;
        }
    }

    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(&state, DASH_CAPTURE_START, 0x0001, payload).await;
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "capture",
        "phase": "start",
        "peers": sent,
    }).to_string());
    (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "peers": sent})))
}

/// `POST /api/capture/mark {name}` — fan out capture.mark with the
/// dashboard's chosen ts_ms (one value shared across the fleet so
/// every sensor's file shares the same name; SPEC §7.2).
async fn capture_mark_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CaptureMarkBody>,
) -> impl IntoResponse {
    if !is_valid_capture_name(&body.name) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error": "invalid name (use [A-Za-z0-9_-]{1,32})",
            })),
        );
    }
    if let Some(p) = body.prefix.as_deref() {
        if !is_valid_capture_prefix(p) {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "error": "invalid prefix (use [0-9_-]{1,32})",
                })),
            );
        }
    }
    let ts_ms = dash_wall_ms() as i64;
    let payload = encode_capture_mark(ts_ms, &body.name, body.prefix.as_deref());
    let sent = fan_out_dash_frame(&state, DASH_CAPTURE_MARK, 0x0002, payload).await;
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "capture",
        "phase": "mark",
        "name": body.name,
        "prefix": body.prefix,
        "ts_ms": ts_ms,
        "peers": sent,
    }).to_string());
    (axum::http::StatusCode::OK, Json(serde_json::json!({
        "ok": true,
        "ts_ms": ts_ms,
        "name": body.name,
        "prefix": body.prefix,
        "peers": sent,
    })))
}

/// `POST /api/capture/stop` — fan out capture.stop.
async fn capture_stop_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(&state, DASH_CAPTURE_STOP, 0x0003, payload).await;
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "capture",
        "phase": "stop",
        "peers": sent,
    }).to_string());
    (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "peers": sent})))
}

fn is_valid_capture_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|b| matches!(
        b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
    ))
}

fn is_valid_capture_prefix(p: &str) -> bool {
    !p.is_empty() && p.len() <= 32 && p.bytes().all(|b| matches!(
        b, b'0'..=b'9' | b'_' | b'-'
    ))
}

// ── data_tcp client (port 21047) ──────────────────────────────────────

const DATA_PORT: u16 = 21047;
const ST_OK: u8 = 0x00;
const ST_ERROR: u8 = 0x01;
const ST_BUSY: u8 = 0x02;

/// Open a fresh TCP connection to <ip>:21047 on the named peer.
/// Strips any trailing port suffix from `addr` (the webapp keys by IP
/// alone but tolerates `ip:port`).
async fn dial_data_tcp(addr: &str) -> std::io::Result<TcpStream> {
    let ip_only: &str = addr.split(':').next().unwrap_or(addr);
    let target = format!("{}:{}", ip_only, DATA_PORT);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&target),
    ).await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "data_tcp connect timeout"))?
}

/// `GET /api/data/{addr}/list` — proxy a LIST opcode to the sensor.
async fn data_list_handler(Path(addr): Path<String>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("connect: {}", e)})),
        ).into_response(),
    };
    if let Err(e) = s.write_all(&[0x01u8]).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("write LIST: {}", e)})),
        ).into_response();
    }
    let mut status = [0u8; 1];
    if let Err(e) = s.read_exact(&mut status).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("read status: {}", e)})),
        ).into_response();
    }
    if status[0] != ST_OK {
        let err_msg = read_err_msg(&mut s).await.unwrap_or_default();
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": err_msg, "status_byte": status[0]})),
        ).into_response();
    }
    let mut count_buf = [0u8; 4];
    if let Err(e) = s.read_exact(&mut count_buf).await {
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("read count: {}", e)})),
        ).into_response();
    }
    let count = u32::from_be_bytes(count_buf) as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let mut nl = [0u8; 2];
        if s.read_exact(&mut nl).await.is_err() { break; }
        let nlen = u16::from_be_bytes(nl) as usize;
        let mut name_buf = vec![0u8; nlen];
        if s.read_exact(&mut name_buf).await.is_err() { break; }
        let mut size_buf = [0u8; 8];
        if s.read_exact(&mut size_buf).await.is_err() { break; }
        let mut mtime_buf = [0u8; 8];
        if s.read_exact(&mut mtime_buf).await.is_err() { break; }
        let name = String::from_utf8_lossy(&name_buf).into_owned();
        let size = u64::from_be_bytes(size_buf);
        let mtime = i64::from_be_bytes(mtime_buf);
        entries.push(serde_json::json!({
            "name": name, "size": size, "mtime_ms": mtime,
        }));
    }
    (axum::http::StatusCode::OK, Json(serde_json::json!({"files": entries}))).into_response()
}

/// `GET /api/data/{addr}/file/{name}` — proxy a GET opcode and stream
/// the file bytes back to the client.
async fn data_get_handler(Path((addr, name)): Path<(String, String)>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x02);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    if s.write_all(&req).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "write GET".to_string()).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read status".to_string()).into_response();
    }
    if status[0] != ST_OK {
        let err_msg = read_err_msg(&mut s).await.unwrap_or_default();
        let code = match status[0] {
            ST_BUSY => axum::http::StatusCode::CONFLICT,
            _ => axum::http::StatusCode::NOT_FOUND,
        };
        return (code, err_msg).into_response();
    }
    let mut size_buf = [0u8; 8];
    if s.read_exact(&mut size_buf).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read size".to_string()).into_response();
    }
    let size = u64::from_be_bytes(size_buf) as usize;
    let mut body = vec![0u8; size];
    if s.read_exact(&mut body).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, "read body".to_string()).into_response();
    }
    // Prepend a CSV header — the on-SD file is raw fixed-width rows
    // (SPEC-R2-ROCKER-SENSOR §6.2) so the firmware doesn't have to do
    // any extra work per capture, but a spreadsheet reader expects
    // column titles. We splice the header here so the user-facing
    // download is self-describing without changing the on-disk shape.
    const HEADER: &[u8] = b"seq,ts_ms,x,y,z\n";
    let mut out = Vec::with_capacity(HEADER.len() + body.len());
    out.extend_from_slice(HEADER);
    out.extend_from_slice(&body);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, out).into_response()
}

/// `DELETE /api/data/{addr}/file/{name}` — proxy a DEL opcode.
async fn data_delete_handler(Path((addr, name)): Path<(String, String)>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x03);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    if s.write_all(&req).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "write"}))).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "read status"}))).into_response();
    }
    if status[0] == ST_OK {
        return (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response();
    }
    let msg = read_err_msg(&mut s).await.unwrap_or_default();
    let code = if status[0] == ST_BUSY { axum::http::StatusCode::CONFLICT } else { axum::http::StatusCode::BAD_GATEWAY };
    (code, Json(serde_json::json!({"ok": false, "error": msg, "status_byte": status[0]}))).into_response()
}

/// `DELETE /api/data/{addr}/all` — proxy a DEL_ALL opcode.
async fn data_delete_all_handler(Path(addr): Path<String>) -> impl IntoResponse {
    let mut s = match dial_data_tcp(&addr).await {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    };
    if s.write_all(&[0x04u8]).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "write"}))).into_response();
    }
    let mut status = [0u8; 1];
    if s.read_exact(&mut status).await.is_err() {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": "read status"}))).into_response();
    }
    if status[0] != ST_OK {
        let msg = read_err_msg(&mut s).await.unwrap_or_default();
        return (axum::http::StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": msg}))).into_response();
    }
    let mut count_buf = [0u8; 4];
    let _ = s.read_exact(&mut count_buf).await;
    let count = u32::from_be_bytes(count_buf);
    (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "deleted": count}))).into_response()
}

/// `GET /api/data/merged?file=<basename>` — fetch the named file from
/// every connected peer and emit a wide-format CSV: one row per unique
/// `ts_ms` across the fleet, with three columns per sensor
/// (`<ip>_x, <ip>_y, <ip>_z`). Cells are blank when that sensor has
/// no sample at that ts_ms — handy when sample timestamps don't line
/// up exactly across the fleet (clock-sync jitter, dropped samples).
async fn data_merged_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(name) = q.get("file").cloned() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing ?file= parameter"})),
        ).into_response();
    };

    // Sort peer IPs so column order is stable across runs (the natural
    // peer-map order is hash-based and would shuffle headers between
    // downloads, breaking any downstream tooling pinned to column
    // positions).
    let mut peer_addrs: Vec<String> = {
        let peers = state.peers.read().await;
        peers.keys().map(|a| a.ip().to_string()).collect()
    };
    peer_addrs.sort();

    let mut fetched: Vec<(String, Vec<u8>)> = Vec::with_capacity(peer_addrs.len());
    for addr in &peer_addrs {
        match fetch_capture_bytes(addr, &name).await {
            Ok(bytes) => fetched.push((addr.clone(), bytes)),
            Err(e) => eprintln!("[merge] {} {}: {}", addr, name, e),
        }
    }
    if fetched.is_empty() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no sensor returned this file"})),
        ).into_response();
    }

    // Fixed-width capture row (SPEC-R2-ROCKER-CAPTURE §4 +
    // SPEC-R2-ROCKER-SENSOR §6.2):
    //   bytes  0..10  : seq (right-aligned)
    //   bytes 11..25  : ts_ms (right-aligned)
    //   bytes 26..37  : x   (right-aligned)
    //   bytes 38..49  : y
    //   bytes 50..61  : z
    //   byte  61      : '\n' (counted into ROW_BYTES below — last byte)
    const ROW_BYTES: usize = 62;

    // Build a sorted ts_ms → [per-sensor (x,y,z) Option] map. BTreeMap
    // gives us ascending iteration for free. Each sensor contributes
    // its samples; if two sensors share a ts_ms, both fill the same
    // row; if their timestamps diverge by even 1 ms, separate rows.
    //
    // The expected scale here is small: 10 Hz × minutes × ~2 sensors
    // = a few thousand rows. BTreeMap is fine.
    type Triplet = (String, String, String);
    let n_peers = fetched.len();
    let mut by_ts: std::collections::BTreeMap<i64, Vec<Option<Triplet>>> =
        std::collections::BTreeMap::new();

    for (peer_idx, (_, bytes)) in fetched.iter().enumerate() {
        for row in bytes.chunks_exact(ROW_BYTES) {
            let Some(ts) = parse_row_ts_ms(row) else { continue; };
            // Column layout offsets — adjacent fields are separated by
            // single commas, captured by the +1 shifts below.
            let x_str = trim_str(&row[26..37]);
            let y_str = trim_str(&row[38..49]);
            let z_str = trim_str(&row[50..61]);
            by_ts.entry(ts)
                .or_insert_with(|| vec![None; n_peers])
                [peer_idx] = Some((x_str, y_str, z_str));
        }
    }

    let mut output = String::with_capacity(64 * 1024);
    // Header: ts_ms then three columns per sensor in sorted-IP order.
    output.push_str("ts_ms");
    for (sensor_name, _) in &fetched {
        // Sanitize IPs for spreadsheet-friendly column names — dots
        // upset some downstream consumers.
        let safe = sensor_name.replace('.', "_");
        output.push(','); output.push_str(&safe); output.push_str("_x");
        output.push(','); output.push_str(&safe); output.push_str("_y");
        output.push(','); output.push_str(&safe); output.push_str("_z");
    }
    output.push('\n');

    for (ts, slots) in &by_ts {
        output.push_str(&ts.to_string());
        for slot in slots {
            output.push(',');
            if let Some((x, y, z)) = slot {
                output.push_str(x); output.push(',');
                output.push_str(y); output.push(',');
                output.push_str(z);
            } else {
                // Three empty cells — blank means "no reading for this
                // ts_ms from this sensor", per the wide-merge contract.
                output.push(','); output.push(',');
            }
        }
        output.push('\n');
    }

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"merged-{}\"", name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, output).into_response()
}

fn parse_row_ts_ms(row: &[u8]) -> Option<i64> {
    if row.len() < 26 { return None; }
    // bytes 11..25 carry ts_ms (right-aligned). Trim ASCII spaces, parse.
    let ts_field = &row[11..25];
    let s = std::str::from_utf8(ts_field).ok()?;
    s.trim().parse::<i64>().ok()
}

fn trim_str(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Fetch one capture file from `<addr>:21047` over data_tcp GET.
async fn fetch_capture_bytes(addr: &str, name: &str) -> std::io::Result<Vec<u8>> {
    let mut s = dial_data_tcp(addr).await?;
    let mut req = Vec::with_capacity(3 + name.len());
    req.push(0x02);
    req.extend_from_slice(&(name.len() as u16).to_be_bytes());
    req.extend_from_slice(name.as_bytes());
    s.write_all(&req).await?;
    let mut status = [0u8; 1];
    s.read_exact(&mut status).await?;
    if status[0] != ST_OK {
        let _ = read_err_msg(&mut s).await;
        return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("status {}", status[0])));
    }
    let mut size_buf = [0u8; 8];
    s.read_exact(&mut size_buf).await?;
    let size = u64::from_be_bytes(size_buf) as usize;
    let mut body = vec![0u8; size];
    s.read_exact(&mut body).await?;
    Ok(body)
}

async fn read_err_msg(s: &mut TcpStream) -> Option<String> {
    let mut ml = [0u8; 2];
    s.read_exact(&mut ml).await.ok()?;
    let len = u16::from_be_bytes(ml) as usize;
    let mut msg = vec![0u8; len];
    s.read_exact(&mut msg).await.ok()?;
    Some(String::from_utf8_lossy(&msg).into_owned())
}

fn encode_raw_frame_envelope(rf: &RawFrame) -> Vec<u8> {
    let src = rf.src.as_bytes();
    let mut out = Vec::with_capacity(2 + src.len() + 4 + 2 + rf.frame.len());
    out.extend_from_slice(&(src.len() as u16).to_be_bytes());
    out.extend_from_slice(src);
    out.extend_from_slice(&(rf.ts_ms as u32).to_be_bytes());
    out.extend_from_slice(&(rf.frame.len() as u16).to_be_bytes());
    out.extend_from_slice(&rf.frame);
    out
}

/// `/api/keyholder/tg-pub` — return the trust-group public key (hex).
///
/// Used by browsers during enrolment to confirm they're talking to the
/// expected TG (cross-check against the QR-code-encoded TG fingerprint).
async fn tg_pub_handler() -> impl IntoResponse {
    // trust_keys/tg_pub.bin sits at the repo root, two levels up from
    // dashboard/src/main.rs.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("trust_keys/tg_pub.bin"));
    let bytes = match path.and_then(|p| std::fs::read(p).ok()) {
        Some(b) if b.len() == 32 => b,
        _ => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "tg_pub.bin not found or wrong length",
                    "hint": "run tools/r2-rocker-tg keygen and copy tg_pub.bin to trust_keys/",
                })),
            )
                .into_response();
        }
    };
    let hex_str: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    Json(serde_json::json!({
        "tg_public_key_hex": hex_str,
        "tg_public_key_len": 32,
    }))
    .into_response()
}

// ── SPEC-R2-ROCKER-ACCESS handlers ────────────────────────────────────
//
// All four routes share one helper that fetches the AccessHandle from
// state. The handlers themselves are small — the heavy lifting lives in
// `access.rs` (TrustGroup wrangling, token table, QR rendering).

/// Returns the `AccessHandle` or a 503 response describing why Access
/// is offline.
async fn require_access(state: &Arc<AppState>) -> std::result::Result<
    access::AccessHandle,
    axum::response::Response,
> {
    match state.access.as_ref() {
        Some(h) => Ok(h.clone()),
        None => Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Access is not configured on this dashboard.",
                "hint": "Run tools/r2-rocker-tg keygen to generate tg_priv.bin under ~/.config/r2-rocker/tg_signer/, then restart.",
            })),
        ).into_response()),
    }
}

/// KeyHolder gate for v0.1 per SPEC-R2-ROCKER-ACCESS §11.1 (2): only
/// the controller's own browser may invite, list, or revoke. The check
/// is "the request came in over a loopback address." A cert-handshake
/// gate replaces this in v1.
fn is_keyholder(connect: SocketAddr) -> bool {
    connect.ip().is_loopback()
}

async fn access_invite_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "only the KeyHolder (localhost) may issue invitations in v0.1",
            })),
        ).into_response();
    }
    // The operator's browser is loading the dashboard from some
    // host:port the viewer can also reach — use that as the URL we
    // bake into the invite, rather than the dashboard's bind
    // address (which is often 0.0.0.0 and useless to a phone).
    let host_override = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mut access = handle.lock().await;
    match access.mint_invite_with_host(host_override.as_deref()) {
        Ok(env) => (axum::http::StatusCode::OK, Json(serde_json::to_value(&env).unwrap_or_default())).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("mint_invite: {e:#}")})),
        ).into_response(),
    }
}

async fn access_claim_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<access::ClaimRequest>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let device_pk_for_broadcast = body.device_pk.clone();
    let device_name_for_broadcast = body.device_name.clone();
    let outcome = {
        let mut access = handle.lock().await;
        access.process_claim(&body)
    };
    use access::ClaimOutcome::*;
    match outcome {
        Success(resp) => {
            // §4.2 step 8: broadcast member_added on /ws/status so
            // every connected member can refresh its Access tab.
            let _ = state.ws_broadcast_tx.send(serde_json::json!({
                "type": "access",
                "event": "member_added",
                "device_pk": device_pk_for_broadcast,
                "name": device_name_for_broadcast,
                "role": "viewer",
                "paired_at_ms": resp.get("paired_at_ms"),
            }).to_string());
            (axum::http::StatusCode::OK, Json(resp)).into_response()
        }
        BadRequest(msg) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        ).into_response(),
        BadRequestBoxed(msg) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        ).into_response(),
        NotFound => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no such invitation (wrong TG, wrong token, or never issued)"})),
        ).into_response(),
        Conflict => (
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "invitation already claimed by a different device"})),
        ).into_response(),
        Gone => (
            axum::http::StatusCode::GONE,
            Json(serde_json::json!({"error": "invitation expired — ask the operator to issue a fresh one"})),
        ).into_response(),
    }
}

async fn access_members_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "only the KeyHolder (localhost) may list members in v0.1"})),
        ).into_response();
    }
    let access = handle.lock().await;
    let rows = access.members();
    (axum::http::StatusCode::OK, Json(serde_json::json!({"members": rows}))).into_response()
}

async fn access_revoke_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "only the KeyHolder (localhost) may revoke members in v0.1"})),
        ).into_response();
    }
    let outcome = {
        let mut access = handle.lock().await;
        access.revoke(&device_pk)
    };
    use access::RevokeOutcome::*;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    match outcome {
        Revoked(pk) => {
            // §7.2 — broadcast on /ws/status so any currently-connected
            // viewer can react. §7.4 server-side TCP teardown for the
            // revoked peer's connections is implementation-side work
            // that lands with the v1 cert-handshake variant; for v0.1
            // the broadcast + future-connection-rejection is the
            // operative guarantee.
            let _ = state.ws_broadcast_tx.send(serde_json::json!({
                "type": "access",
                "event": "revoked",
                "device_pk": hex::encode(&pk[..]),
                "revoked_at_ms": now_ms,
            }).to_string());
            (axum::http::StatusCode::OK, Json(serde_json::json!({
                "ok": true,
                "revoked_at_ms": now_ms,
            }))).into_response()
        }
        NotFound => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no such member (already revoked, or never paired)"})),
        ).into_response(),
        BadRequest => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "device_pk must be 64 hex chars"})),
        ).into_response(),
        Other(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ).into_response(),
    }
}

/// `/api/enrol-init` — KeyHolder generates a one-time join token.
/// **Stub** until Phase 5d-enrol; returns 501 NotImplemented for now.
/// When implemented: returns `{ token, qr_payload, expires_at }`.
async fn enrol_init_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "enrolment not yet implemented",
            "phase": "5d-enrol",
        })),
    )
}

/// `/api/enrol-complete` — browser submits its public key + token; KeyHolder
/// verifies, issues a TG-signed device cert. **Stub** until Phase 5d-enrol.
async fn enrol_complete_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "enrolment not yet implemented",
            "phase": "5d-enrol",
        })),
    )
}
