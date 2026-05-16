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

// Legacy M10-demo names retained so a mixed deployment with prior-gen
// sensors still parses. Remove when M10 sensors retire.
const LEGACY_ACCELERATION: u32 = r2_fnv::fnv1a_32(b"acceleration");
const LEGACY_BATTERY:      u32 = r2_fnv::fnv1a_32(b"battery_status");
const LEGACY_RUN_STATE:    u32 = r2_fnv::fnv1a_32(b"run_state");
const LEGACY_GYROSCOPE:    u32 = r2_fnv::fnv1a_32(b"gyroscope");

// Browser → sensor command hashes (kept for backward compat).
const CMD_START:     u32 = r2_fnv::fnv1a_32(b"cmd_start");
const CMD_STOP:      u32 = r2_fnv::fnv1a_32(b"cmd_stop");
const CMD_MARK:      u32 = r2_fnv::fnv1a_32(b"cmd_mark");
const CMD_CALIBRATE: u32 = r2_fnv::fnv1a_32(b"cmd_calibrate");
const SHUTDOWN:      u32 = r2_fnv::fnv1a_32(b"shutdown");

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
        LEGACY_ACCELERATION       => "acceleration",
        LEGACY_BATTERY            => "battery_status",
        LEGACY_RUN_STATE          => "run_state",
        LEGACY_GYROSCOPE          => "gyroscope",
        CMD_START                 => "cmd_start",
        CMD_STOP                  => "cmd_stop",
        CMD_MARK                  => "cmd_mark",
        CMD_CALIBRATE             => "cmd_calibrate",
        SHUTDOWN                  => "shutdown",
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
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let (event_tx, _) = broadcast::channel::<DashboardEvent>(256);
    let (raw_frame_tx, _) = broadcast::channel::<RawFrame>(1024);
    let (ws_broadcast_tx, _) = broadcast::channel::<String>(256);
    let state = Arc::new(AppState {
        event_tx: event_tx.clone(),
        raw_frame_tx: raw_frame_tx.clone(),
        peers: RwLock::new(HashMap::new()),
        ws_broadcast_tx,
        bootstrap_running: Arc::new(AtomicBool::new(false)),
        bootstrap_log: Arc::new(Mutex::new(Vec::new())),
        bootstrap_task: Mutex::new(None),
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
        // log_tcp listener (port 21045) and pipes lines back as WS text
        // frames. Used by the per-card "↓ Logs" panel in the webapp.
        .route("/ws/logs/{addr}", get(ws_logs_handler))
        // Phase 5d: TG public key + KeyHolder enrolment endpoints.
        .route("/api/keyholder/tg-pub", get(tg_pub_handler))
        .route("/api/enrol-init", post(enrol_init_handler))
        .route("/api/enrol-complete", post(enrol_complete_handler))
        .route("/api/bootstrap", post(bootstrap_handler))
        .route("/api/bootstrap/status", get(bootstrap_status_handler))
        // Phase 9-light: stream a firmware .bin to a sensor's OTA listener.
        .route("/api/ota/{addr}", post(ota_push_handler))
        // SPEC-R2-ROCKER-SENSOR-REMOTE-RESET: push a CMD_RESET to a sensor's
        // reset listener (TCP 21044). Triggers esp_restart() on the sensor.
        .route("/api/sensor/{addr}/reset", post(reset_push_handler))
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
    axum::serve(listener, app).await.unwrap();
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
/// Opens a TCP socket to `<addr>:21045` (the firmware's `log_tcp`
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
    let target = format!("{}:21045", ip_only);
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
