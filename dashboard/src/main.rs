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
mod relay;

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
// Controller-synthesised peer-lifecycle events (BRIDGE §3.1). Today
// only `r2.peer.disconnected` is emitted; `r2.peer.connected` is
// covered by the existing announce replay and not yet a separate
// event — see SPEC-R2-ROCKER-VIEWER-SENTANT §6 outbound roadmap.
const PEER_DISCONNECTED:   u32 = r2_fnv::fnv1a_32(b"r2.peer.disconnected");

// Tracks B+C operator-plane status notifications. These wrap the
// legacy `/ws/status` JSON messages of the same names (preserved
// for one release for backward compat with un-upgraded browsers).
// Each event is a CBOR map with small integer keys; the per-event
// shape is documented at the encode helper. Hash names align with
// SPEC-R2-ROCKER-BRIDGE.md §3.1 where applicable.
const DASH_OTA_PROGRESS:       u32 = r2_fnv::fnv1a_32(b"r2.dash.ota.progress");
const DASH_RESET_PROGRESS:     u32 = r2_fnv::fnv1a_32(b"r2.dash.reset.progress");
const DASH_CAPTURE_PROGRESS:   u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.progress");
const DASH_ACCESS_EVENT:       u32 = r2_fnv::fnv1a_32(b"r2.dash.access.event");
const DASH_BOOTSTRAP_PROGRESS: u32 = r2_fnv::fnv1a_32(b"r2.dash.bootstrap.progress");
const DASH_DEVICE_ALIAS_CHANGED: u32 = r2_fnv::fnv1a_32(b"r2.dash.device.alias.changed");

// Dashboard → sensor commands (SPEC-R2-ROCKER-WIRE §4 + SPEC-R2-ROCKER-TIMESYNC §4).
const DASH_ACK:               u32 = r2_fnv::fnv1a_32(b"r2.dash.ack");
const DASH_SYNC_PULSE:        u32 = r2_fnv::fnv1a_32(b"r2.dash.sync_pulse");
const DASH_SET_CLOCK_OFFSET:  u32 = r2_fnv::fnv1a_32(b"r2.dash.set_clock_offset");
const DASH_IDENTIFY_SET:      u32 = r2_fnv::fnv1a_32(b"r2.dash.identify_set");
// Capture session (SPEC-R2-ROCKER-CAPTURE §3).
const DASH_CAPTURE_START:     u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.start");
const DASH_CAPTURE_MARK:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.mark");
const DASH_CAPTURE_STOP:      u32 = r2_fnv::fnv1a_32(b"r2.dash.capture.stop");
const SENSOR_CAPTURE_STATE:   u32 = r2_fnv::fnv1a_32(b"r2.sensor.capture.state");

// Track C operator-plane events (viewer → controller). Per
// SPEC-R2-ROCKER-WIRE §2.1, viewer hives send these inbound on
// /ws/raw; the dashboard validates and fans the corresponding
// downstream `r2.dash.<action>` to all sensors, then emits a
// `r2.dash.cmd.response` correlated by `req_id`.
const DASH_CMD_CAPTURE_START: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.start");
const DASH_CMD_CAPTURE_MARK:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.mark");
const DASH_CMD_CAPTURE_STOP:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.capture.stop");
const DASH_CMD_RESET:         u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.reset");
const DASH_CMD_IDENTIFY:      u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.identify");
const DASH_CMD_BOOTSTRAP:     u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.bootstrap");
const DASH_CMD_DEVICE_ALIAS_SET: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.device.alias.set");
const DASH_CMD_ACCESS_MEMBERS_QUERY: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.members.query");
const DASH_CMD_ACCESS_PENDING_QUERY: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.pending.query");
const DASH_CMD_ACCESS_CHECK:  u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.check");
const DASH_CMD_ACCESS_APPROVE: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.approve");
const DASH_CMD_ACCESS_DENY:   u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.deny");
const DASH_CMD_ACCESS_REVOKE: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.revoke");
const DASH_CMD_ACCESS_REQUEST: u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.access.request");
const DASH_CMD_RESPONSE:      u32 = r2_fnv::fnv1a_32(b"r2.dash.cmd.response");

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
            // Track A — KeyHolder-signed DeviceCertificate (147 bytes,
            // hex-encoded after remap). verify_announce_signature
            // reads this and switches to cert-anchored mode.
            ("8", "device_cert"),
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
    /// Unified R2 port — carries R2-WIRE events from sensors (raw TCP,
    /// length-prefixed per R2-WIRE §13.4) AND the browser-facing HTTP +
    /// WebSocket server (R2-WIRE-over-WS per R2-TRANSPORT §3.5). Per
    /// R2-WIRE §13.5, both encodings live on the canonical port 21042.
    /// Each accepted connection is peek-dispatched: HTTP-looking → axum;
    /// otherwise → raw R2-WIRE sensor handler.
    #[arg(long, default_value = "21042")]
    port: u16,

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
    /// 64-hex-char Ed25519 public key from the most recent announce.
    /// Used as the alias-map lookup key in `/api/data/merged` and
    /// anywhere else we want to address a sensor by *device* identity
    /// rather than its (ephemeral) IP. Pulled out of the announce
    /// payload at decode time so downstream code doesn't have to
    /// re-parse the cached CBOR frame.
    device_pk: Option<String>,
    /// Most-recent `r2.sensor.announce` raw R2-WIRE frame bytes,
    /// cached so a freshly-connected /ws/raw viewer can be replayed
    /// the announce — otherwise it never sees `fw_ver`, `device_pk`,
    /// or `boot_ts_ms` because the announce only fires on TCP
    /// (re)connect, which already happened before the viewer arrived.
    last_announce: Option<Vec<u8>>,
    /// Most-recent `r2.sensor.capture.state` raw frame, cached for the
    /// same reason: capture.state only fires on transitions
    /// (start/mark/stop), so a viewer that hard-refreshes mid-recording
    /// would see the Run-Control buttons reset to the IDLE defaults.
    /// Replaying the cached state on /ws/raw open re-syncs the UI
    /// without needing a round-trip to the sensor.
    last_capture_state: Option<Vec<u8>>,
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
/// Build a sensor-bound TCP frame: the R2-WIRE compact body, prefixed
/// with a u16 BE length per the TCP framing convention. Suitable for
/// `peer.tx.send(...)` (sensor sockets).
fn build_dash_frame(event_hash: u32, msg_id: u16, payload: &[u8]) -> Vec<u8> {
    let frame_len = 12 + payload.len();
    let mut out = Vec::with_capacity(2 + frame_len);
    out.extend_from_slice(&(frame_len as u16).to_be_bytes());
    out.extend_from_slice(&build_dash_frame_body(event_hash, msg_id, payload));
    out
}

/// Build a bare R2-WIRE compact body (no leading TCP length prefix).
/// Suitable for storing in `RawFrame.frame` — the envelope's own
/// `frame_len` field provides framing for /ws/raw consumers, and the
/// webapp's `decode_compact_frame` reads from byte 0 of this body
/// (version/type/flags). Putting a TCP-style length prefix here
/// corrupts the decode: the first two bytes would be parsed as the
/// header byte, leaving event_hash off by two and silently dropped
/// by the viewer sentant.
fn build_dash_frame_body(event_hash: u32, msg_id: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
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

/// Encode `r2.dash.identify_set` payload `{0: u8 on}`.
fn encode_identify_set(on: bool) -> Vec<u8> {
    let mut buf = [0u8; 8];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let _ = enc.kv(0, &r2_cbor::Value::UInt(if on { 1 } else { 0 }));
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
    //
    // Normal-case baseline waits ≥ 5 rounds + std-dev of the last 3
    // estimates < 5 ms so RTT jitter doesn't get baked into the offset.
    // But when the sensor's clock is grossly out (cold boot with no
    // NVS offset, or NVS-stale-by-minutes after an OTA), the smoothed
    // estimate is ≫ any plausible RTT jitter. Push that immediately —
    // the wall-clock-driven LED animation (and SD-card mtimes) read
    // wrong until baseline lands, and waiting 5+ rounds at that scale
    // is just operator confusion ("my LEDs are out of sync").
    const BASELINE_FAST_PATH_MS: f64 = 500.0;
    let push_decision: Option<(i64, &'static str)> = if !s.baseline_pushed {
        if smoothed.abs() >= BASELINE_FAST_PATH_MS {
            Some((smoothed.round() as i64, "baseline (fast)"))
        } else if s.estimates.len() >= 5 && std_dev_last_n(&s.estimates, 3) < 5.0 {
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
pub(crate) struct RawFrame {
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
    /// Outbound JSON text frames bound for the R2 relay session.
    /// Anyone (e.g. access::approve_request) pushes a string here;
    /// relay.rs subscribes and forwards each string verbatim as a
    /// WS text frame. None when the dashboard isn't running with
    /// `--relay-url`.
    /// Broadcasts JOIN_RESPONSE frames (notekeeper wire format —
    /// `[0xFF, 0x02, devicePk(32), tgPk(32), encrypted]`) for the
    /// relay session to forward to the joining device. `Some` only
    /// when `--relay-url` is configured.
    relay_binary_tx: Option<broadcast::Sender<Vec<u8>>>,
    /// Operator-assigned device aliases (device_pk hex → friendly
    /// name). Persisted to `~/.config/r2-rocker/device_aliases.json`
    /// so renames survive dashboard restarts and propagate to every
    /// dashboard browser session. v0.1 limitation: the sensor's own
    /// hostname / SD-card filename still uses its hardware-derived
    /// name — pushing aliases into firmware NVS is a follow-up task
    /// (see project memory `heterogeneous-fleet-open-question.md`).
    device_aliases: Arc<Mutex<HashMap<String, String>>>,
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
    // Relay outbound-binary channel for JOIN_RESPONSE frames
    // (notekeeper wire format `[0xFF, 0x02, ...]`). Only allocated
    // when --relay-url is set so the access handlers can branch on
    // `state.relay_binary_tx.is_some()`.
    let relay_binary_tx: Option<broadcast::Sender<Vec<u8>>> = if args.relay_url.is_some() {
        let (tx, _) = broadcast::channel::<Vec<u8>>(256);
        Some(tx)
    } else { None };

    // Phase 5: try to load the KeyHolder signing key. A successful load
    // unlocks /api/access/*; a failure logs + leaves Access disabled.
    // local_origin is what we'll embed in `url_local` per
    // SPEC-R2-ROCKER-ACCESS §4.1 step 4 — same host:port the webapp is
    // served on.
    let local_origin = format!("http://{}:{}", args.bind, args.port);
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

    // Load persisted device aliases (renames survive dashboard restarts).
    let device_aliases = Arc::new(Mutex::new(load_device_aliases()));

    let state = Arc::new(AppState {
        event_tx: event_tx.clone(),
        raw_frame_tx: raw_frame_tx.clone(),
        peers: RwLock::new(HashMap::new()),
        ws_broadcast_tx,
        bootstrap_running: Arc::new(AtomicBool::new(false)),
        bootstrap_log: Arc::new(Mutex::new(Vec::new())),
        bootstrap_task: Mutex::new(None),
        firmware_cache: Mutex::new(None),
        access: access_handle.clone(),
        relay_binary_tx: relay_binary_tx.clone(),
        device_aliases,
    });

    // Phase 5 / SPEC-R2-ROCKER-ACCESS §5.2 — off-network viewer path
    // via the R2 relay. Only spawn when both --relay-url is set AND
    // the KeyHolder loaded; viewers need both to be useful.
    if let (Some(url), Some(handle), Some(tx)) = (args.relay_url.clone(), access_handle, relay_binary_tx) {
        let (sk, pk) = {
            let a = handle.lock().await;
            (a.tg_signing_key(), a.tg_pk_bytes())
        };
        relay::spawn_relay_session(
            url.clone(),
            sk,
            pk,
            raw_frame_tx.clone(),
            tx,
            state.clone(),
        );
        eprintln!("[relay] session spawned → {url}");
    }

    // R2-WIRE §13.5: port 21042 carries R2-WIRE events in both raw-TCP
    // (sensor side, length-prefixed) and WebSocket (browser side) form.
    // Single listener with peek-based protocol detection unifies both —
    // see the accept loop below.

    // HTTP server with WASM viewer + WebSocket + bootstrap API.
    // The legacy `/` HTML dashboard and `/ws` bidirectional channel were
    // removed once the WASM viewer at the repo's webapp/ became feature-
    // complete. The WASM viewer consumes /ws/raw + /ws/status instead.
    let mut app = Router::new()
        // Phase 5d: raw R2-WIRE frame forwarder for WASM viewers.
        .route("/ws/raw", get({
            let ws_state = state.clone();
            move |ws, connect_info| ws_raw_handler(ws, ws_state, connect_info)
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
        .route("/api/access/members", get(access_members_handler))
        .route("/api/access/revoke/{device_pk}", post(access_revoke_handler))
        // Request → approve flow (v0.1.1, the "calm-tech" enrolment
        // path): phone POSTs /request when it lands on the dashboard
        // without a cert. Operator's Link tab shows pending rows
        // with Approve/Deny. Phone polls /check until it gets the
        // signed cert bundle back.
        .route("/api/access/request",     post(access_request_handler))
        .route("/api/access/check/{device_pk}", get(access_check_handler))
        .route("/api/access/pending",     get(access_pending_handler))
        .route("/api/access/approve/{device_pk}", post(access_approve_handler))
        .route("/api/access/deny/{device_pk}",    post(access_deny_handler))
        // ACCESS v0.3 §8 — operator-only helper that returns the
        // pair of QR payloads for the "Onboard a visitor" modal.
        .route("/api/access/onboard",     get(access_onboard_handler))
        // Self-heal: a paired viewer calls this on every load with
        // its own device_pk to confirm it's still a known member.
        // 404 → stale cert → webapp wipes IndexedDB and re-prompts.
        .route("/api/access/whoami/{device_pk}", get(access_whoami_handler))
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
        .route("/api/sensor/{addr}/identify", post(identify_handler))
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
        // Operator-assigned device aliases. Persisted to
        // ~/.config/r2-rocker/device_aliases.json. Read by every
        // dashboard browser session on load + applied on top of the
        // sensor's self-reported hostname.
        .route("/api/devices/aliases",         get(device_aliases_get_handler))
        .route("/api/devices/alias",           post(device_alias_set_handler))
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

    let bind_addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .expect("valid bind address");

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║              r2-rocker dashboard                              ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  version:    {:<48}║", DASHBOARD_VERSION);
    eprintln!("║  built:      {:<48}║", env!("R2_BUILD_TIMESTAMP"));
    eprintln!("║  R2 port:    {:<48}║", format!("{} (raw R2-WIRE TCP + HTTP/WS)", bind_addr));
    eprintln!("║  dashboard:  http://{:<41}║", bind_addr.to_string());
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    let listener = tokio::net::TcpListener::bind(bind_addr).await
        .unwrap_or_else(|e| {
            eprintln!("ERROR: Cannot bind R2 port {} — {}", bind_addr, e);
            eprintln!("Is another r2-dashboard already running? Kill it first: pkill r2-dashboard");
            std::process::exit(1);
        });
    eprintln!("[r2-port] listening on {}", bind_addr);

    run_unified_listener(listener, app, state).await;
}

/// Single accept loop on the unified R2 port (R2-WIRE §13.5 — port
/// 21042 carries R2-WIRE events in both raw-TCP and WebSocket form).
/// Each accepted connection is peeked: HTTP-looking → driven via hyper
/// with the axum router; otherwise → handed to the existing sensor
/// TCP handler. Sensor frames are length-prefixed (R2-WIRE §13.4), so
/// the first byte is always the high byte of a u16 BE length — for
/// our compact frames (< 256 bytes) that's `0x00`. HTTP request lines
/// start with ASCII `[A-Z]`. The two never collide.
async fn run_unified_listener(
    listener: tokio::net::TcpListener,
    app: axum::Router<()>,
    state: Arc<AppState>,
) {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use tower::ServiceExt;

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[r2-port] accept error: {}", e);
                continue;
            }
        };
        let app_for_conn = app.clone();
        let state_for_conn = state.clone();
        tokio::spawn(async move {
            // Peek the first byte. 5 s is generous — even slow sensors
            // emit their announce within hundreds of ms of TCP connect.
            // Browsers send HTTP request lines well under that too.
            let mut first = [0u8; 1];
            let peek = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                stream.peek(&mut first),
            ).await;
            let n = match peek {
                Ok(Ok(n)) => n,
                _ => return, // timeout or read error
            };
            if n == 0 { return; }

            if first[0].is_ascii_uppercase() {
                // HTTP path — drive axum via hyper. Attach ConnectInfo
                // to every Request so /api/access/* handlers (which
                // extract `ConnectInfo<SocketAddr>` for the loopback
                // KeyHolder gate) work as they did under axum::serve.
                let svc = ServiceExt::<hyper::Request<Incoming>>::map_request(
                    app_for_conn,
                    move |mut req: hyper::Request<Incoming>| {
                        req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
                        req
                    },
                );
                let hyper_svc = TowerToHyperService::new(svc);
                let io = TokioIo::new(stream);
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, hyper_svc)
                    .with_upgrades()
                    .await
                {
                    // Don't log normal client-side closes (broken pipe
                    // / connection reset) as errors. The hyper error
                    // type doesn't expose `kind`; match on text.
                    let msg = format!("{}", e);
                    if !msg.contains("user code") && !msg.contains("closed") {
                        eprintln!("[r2-port http] {}: {}", addr, msg);
                    }
                }
            } else {
                // Raw R2-WIRE TCP — sensor connection. TCP keepalive
                // catches zombie connections within ~60 s rather than
                // waiting for the 2-hour OS default. Same shape as the
                // old run_event_listener applied.
                eprintln!("[events] sensor connected: {}", addr);
                let stream = match apply_tcp_keepalive(stream) {
                    Some(s) => s,
                    None => return,
                };
                handle_sensor_connection(stream, addr, state_for_conn).await;
            }
        });
    }
}

/// Apply 15 s/5 s TCP keepalive to a freshly-accepted sensor socket.
/// Lifted out of the former run_event_listener so the unified accept
/// loop can call it after protocol-detect.
fn apply_tcp_keepalive(stream: tokio::net::TcpStream) -> Option<tokio::net::TcpStream> {
    let std_stream = stream.into_std().ok()?;
    let sock = socket2::Socket::from(std_stream);
    sock.set_keepalive(true).ok();
    let ka = socket2::TcpKeepalive::new()
        .with_time(std::time::Duration::from_secs(15))
        .with_interval(std::time::Duration::from_secs(5));
    sock.set_tcp_keepalive(&ka).ok();
    let std_stream: std::net::TcpStream = sock.into();
    std_stream.set_nonblocking(true).ok();
    tokio::net::TcpStream::from_std(std_stream).ok()
}

/// Shared bootstrap core. Aborts any running discovery task, clears
/// the log, cycles the AP, and spawns a fresh discovery cycle.
/// Returns immediately after scheduling — discovery progress streams
/// via `r2.dash.bootstrap.progress`. Fire-and-forget by design; the
/// only synchronous failure mode is task-spawn refusal which doesn't
/// happen in practice on tokio.
async fn do_bootstrap(state: &Arc<AppState>) {
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
    emit_bootstrap_reset(state);

    let config = BootstrapConfig {
        ssid: None,
        psk: None,
        // Longer scan window than the prior 10s default: missed-on-first-pass
        // sensors (BLE advertise interval / RSSI variance / sensor reboot
        // timing right after cycle_hotspot) get caught in the same pass
        // instead of waiting another full retry cycle to be picked up.
        // Pair this with the shorter RETRY_INTERVAL_SECS in r2-bootstrap.
        scan_secs: 20,
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
    let state_for_relay = state.clone();
    let log_store = state.bootstrap_log.clone();
    let running_flag = state.bootstrap_running.clone();

    // Spawn the event relay task
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            // Build WS message — preserved for the legacy /ws/status
            // JSON path (one release for backward compat).
            let ws_msg = serde_json::json!({
                "type": "bootstrap",
                "event": &event,
            });
            let json_str = serde_json::to_string(&ws_msg).unwrap_or_default();

            // Append to log
            {
                let mut log = log_store.lock().await;
                log.push(json_str.clone());
            }

            // Two broadcasts: the legacy /ws/status JSON path carries
            // the serde-tagged structured form (kept while /ws/status
            // is still alive — to be dropped at v0.2). The R2-WIRE
            // path carries the same fields under named CBOR keys
            // per SPEC row 27 — that's our forward channel.
            let _ = state_for_relay.ws_broadcast_tx.send(json_str);
            emit_bootstrap_progress(&state_for_relay, &event);
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
}

/// `POST /api/bootstrap` — legacy HTTP entry.
async fn bootstrap_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    do_bootstrap(&state).await;
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
    emit_ota_progress(&state, "uploading", &ota_target, Some(body.len()), None);

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
            emit_ota_progress(&state, phase, &ota_target, None, Some(&msg));
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
    emit_ota_progress(state, "error", target, None, Some(msg));
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
/// Shared reset core. Returns `Ok((status_byte, message))` on a clean
/// TCP round-trip (where `status_byte == 0x00` means the sensor
/// accepted the reset), or `Err(message)` for connect / timeout /
/// network errors. Either way, `r2.dash.reset.progress` is fired at
/// each phase boundary.
async fn do_reset(state: &Arc<AppState>, addr: &str) -> Result<(u8, String), String> {
    use std::net::ToSocketAddrs;

    let ip_only: &str = addr.split(':').next().unwrap_or(addr);
    let reset_target = format!("{}:21044", ip_only);

    eprintln!("[reset] push to {}", reset_target);
    emit_reset_progress(state, "requested", &reset_target, None);

    let socket = match reset_target.to_socket_addrs() {
        Ok(mut it) => match it.next() {
            Some(a) => a,
            None    => {
                let msg = "no addr resolved".to_string();
                emit_reset_progress(state, "error", &reset_target, Some(&msg));
                return Err(msg);
            }
        },
        Err(e) => {
            let msg = format!("resolve: {e}");
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            return Err(msg);
        }
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
            emit_reset_progress(state, phase, &reset_target, Some(&msg));
            Ok((status_byte, msg))
        }
        Ok(Err(e)) => {
            let msg = format!("push: {e}");
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            Err(msg)
        }
        Err(_) => {
            let msg = "timed out after 8 s".to_string();
            emit_reset_progress(state, "error", &reset_target, Some(&msg));
            Err(msg)
        }
    }
}

/// `POST /api/sensor/{addr}/reset` — legacy HTTP entry.
async fn reset_push_handler(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
) -> impl IntoResponse {
    match do_reset(&state, &addr).await {
        Ok((status_byte, msg)) => {
            let ok = status_byte == 0x00;
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "ok": ok,
                    "status_byte": status_byte,
                    "message": msg,
                })),
            )
        }
        Err(msg) => (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"ok": false, "error": msg})),
        ),
    }
}

/// POST /api/sensor/{addr}/identify  body `{on: bool}` — toggle the
/// operator-identify overlay (solid white LED) on the named sensor.
/// Used to pick a specific board out of a busy rack for a battery
/// swap or similar. Frame goes out via the streaming-TCP peer
/// command channel (same path as set_clock_offset / sync_pulse).
/// Shared identify core. Queues a `r2.dash.identify_set` frame on
/// the named peer's streaming TCP channel. Fire-and-forget — returns
/// `Ok(())` iff the queue accepted; sensor's own ACK (LED actually
/// toggled) is not awaited.
async fn do_identify(state: &Arc<AppState>, addr: &str, on: bool) -> Result<(), String> {
    let ip_only: &str = addr.split(':').next().unwrap_or(addr);

    // peers is keyed by SocketAddr (ip:port); the path/event addr is
    // typically just the IP (or ip:port). Match on the IP portion.
    let tx = {
        let peers = state.peers.read().await;
        peers.iter()
            .find(|(sa, _)| sa.ip().to_string() == ip_only)
            .map(|(_, p)| p.tx.clone())
    };
    let Some(tx) = tx else {
        return Err("no such connected peer".to_string());
    };

    let frame = build_dash_frame(DASH_IDENTIFY_SET, 0, &encode_identify_set(on));
    if tx.send(frame).await.is_err() {
        return Err("peer queue closed".to_string());
    }
    eprintln!("[identify] {} on={}", ip_only, on);
    Ok(())
}

/// `POST /api/sensor/{addr}/identify` — legacy HTTP entry.
async fn identify_handler(
    State(state): State<Arc<AppState>>,
    Path(addr): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let on = body.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
    match do_identify(&state, &addr, on).await {
        Ok(()) => (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "on": on}))),
        Err(msg) => {
            let status = if msg == "no such connected peer" {
                axum::http::StatusCode::NOT_FOUND
            } else {
                axum::http::StatusCode::BAD_GATEWAY
            };
            (status, Json(serde_json::json!({"ok": false, "error": msg})))
        }
    }
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

// run_event_listener was a separate TCP listener on port 21042 for
// sensors, paired with axum::serve on port 8080 for browsers. Replaced
// by run_unified_listener (above) which serves both on the canonical
// R2 port 21042 with peek-based protocol detection — R2-WIRE §13.5.

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
            device_pk: None,
            last_announce: None,
            last_capture_state: None,
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

                        // SPEC-R2-ROCKER-DASHBOARD §5.2 — server-side
                        // acceleration decimation. Originally only applied
                        // to the legacy /ws/status JSON path; left /ws/raw
                        // running at the full firmware rate (100 Hz × N
                        // sensors) on the assumption the WASM hive could
                        // self-throttle. Pi5 deployment proved otherwise —
                        // the WebSocket + browser-side per-frame work
                        // saturated. Decimating at the source for both
                        // transports keeps the live wire at the spec's
                        // ~10 Hz/peer; full fidelity remains on the SD
                        // ring (+ `/api/data/*` retrieval). Task #68.
                        let is_accel = event_hash == Some(ACCELERATION);
                        let emit_live = if is_accel {
                            let due = accel_n == 0;
                            accel_n = (accel_n + 1) % ACCEL_DECIMATION;
                            due
                        } else {
                            true
                        };

                        // /ws/raw viewers — Phase 5d. Push every
                        // non-acceleration event verbatim, and one in
                        // ACCEL_DECIMATION acceleration frames.
                        if emit_live {
                            let _ = read_state.raw_frame_tx.send(RawFrame {
                                src: addr.to_string(),
                                ts_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                frame: frame.clone(),
                            });
                        }

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

                        // Cache the latest capture.state per peer so a
                        // viewer that connects (or hard-refreshes) mid-
                        // recording can have its Run-Control buttons re-sync
                        // to the actual sensor state without waiting for the
                        // next start/mark/stop transition (sensors only emit
                        // capture.state on transitions, not periodically).
                        if event_hash == Some(SENSOR_CAPTURE_STATE) {
                            let mut peers = read_state.peers.write().await;
                            if let Some(peer) = peers.get_mut(&addr) {
                                peer.last_capture_state = Some(frame.clone());
                            }
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

                            // Track A — verify the announce signature, with
                            // cert-chain check when CBOR key 8 is present.
                            // TOFU policy retained for legacy announces
                            // (log-only; don't reject yet — see
                            // SPEC-R2-ROCKER-SENSOR §3.4).
                            //
                            // Tg_pk loaded once per announce. Cheap (32-byte
                            // copy out of the Access handle); not held across
                            // any await in the verify call.
                            let tg_pk_bytes: Option<[u8; 32]> = match read_state.access.as_ref() {
                                Some(h) => Some(h.lock().await.tg_pk_bytes()),
                                None => None,
                            };
                            let sig_ok = match (&payload, &tg_pk_bytes) {
                                (Some(p), Some(tg_pk)) => verify_announce_signature(p, tg_pk),
                                (Some(_), None) => SigStatus::Malformed, // no TG loaded — treat as legacy
                                (None, _) => SigStatus::NoPayload,
                            };

                            eprintln!(
                                "[events] sensor.announce from {}: name={:?} sig={:?} payload={:?}",
                                addr, device_name, sig_ok, payload
                            );

                            // Pull device_pk out of the parsed announce payload
                            // so downstream consumers (data_merged_handler's
                            // alias lookup, the Track-A cert issuance below) don't
                            // have to re-decode the cached CBOR.
                            let device_pk_hex = payload.as_ref()
                                .and_then(|p| p.get("device_pk"))
                                .and_then(|v| v.as_str())
                                .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
                                .map(|s| s.to_string());

                            // Track A — cert issuance. When the sensor's
                            // announce passes signature verification but
                            // carries no cert (legacy TOFU mode), issue a
                            // fresh KeyHolder-signed DeviceCertificate and
                            // push it down the same TCP socket as
                            // r2.dash.enrol. The sensor persists it to NVS
                            // and the NEXT announce will carry the cert at
                            // CBOR key 8 (post-cert mode). One-shot per
                            // session — idempotent across sensor reconnects.
                            if matches!(sig_ok, SigStatus::Valid) {
                                let tx_opt = read_state.peers.read().await.get(&addr).map(|p| p.tx.clone());
                                if let (Some(pk_hex), Some(handle), Some(tx)) = (
                                    device_pk_hex.clone(),
                                    read_state.access.as_ref(),
                                    tx_opt,
                                ) {
                                    if let Ok(pk_bytes) = hex::decode(&pk_hex) {
                                        if let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) {
                                            let cert_bytes = {
                                                let access = handle.lock().await;
                                                access.issue_sensor_cert(pk_arr)
                                            };
                                            let frame = build_dash_frame(
                                                r2_fnv::fnv1a_32(b"r2.dash.enrol"),
                                                0,
                                                &cert_bytes,
                                            );
                                            if tx.send(frame).await.is_err() {
                                                eprintln!(
                                                    "[enrol] {} peer.tx closed; cert push skipped",
                                                    addr
                                                );
                                            } else {
                                                eprintln!(
                                                    "[enrol] issued + pushed cert ({} bytes) to {} (pk first 8: {})",
                                                    cert_bytes.len(),
                                                    addr,
                                                    &pk_hex[..16]
                                                );
                                            }
                                        }
                                    }
                                }
                            }

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
                                    if let Some(ref pk) = device_pk_hex {
                                        peer.device_pk = Some(pk.clone());
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

                            // Acceleration decimation already decided at the
                            // top of the frame-loop (see `emit_live`) — same
                            // gate covers /ws/raw + /ws/status so a viewer
                            // sees consistent per-peer rates regardless of
                            // transport. Per-frame logging removed long ago;
                            // frames are observable via /ws/raw (binary) or
                            // /ws/status (legacy JSON).
                            if emit_live {
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

    // Capture the peer's `device_pk` BEFORE removal so we can include
    // it in the r2.peer.disconnected event payload — the
    // DashboardViewerSentant keys by pk and needs it to drop the
    // sensor from its snapshot.
    let disconnected_pk_hex: Option<String> = {
        let peers = state.peers.read().await;
        peers.get(&addr).and_then(|p| p.device_pk.clone())
    };
    {
        let mut peers = state.peers.write().await;
        peers.remove(&addr);
    }
    eprintln!("[events] sensor disconnected: {}", addr);
    // Tracks B+C — start the migration from /ws/status JSON to R2-WIRE
    // events. The first event picked is `r2.peer.disconnected` because
    // (a) it's purely synthesised by the controller (no sensor side to
    // touch), (b) its payload is tiny, and (c) BRIDGE §3.1 already
    // pre-defines the name + shape, so a future Track E doesn't force
    // a wire break.
    //
    // The frame goes out via raw_frame_tx (same channel as the
    // sensor-originated frames on /ws/raw); the webapp's rocker hive
    // already forwards every /ws/raw event into the
    // DashboardViewerSentant, so this slot lands in the sentant
    // automatically. The legacy JSON message on /ws/status stays for
    // one release so the existing JS handler (which clears the
    // virtual LED) keeps working until UI rendering moves through
    // the hive snapshot.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let addr_str = addr.to_string();
    let payload = encode_peer_disconnected(
        &addr_str,
        now_ms,
        "tcp_close",
        disconnected_pk_hex.as_deref(),
    );
    let frame = build_dash_frame_body(PEER_DISCONNECTED, 0, &payload);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: addr_str.clone(),
        ts_ms: now_ms,
        frame,
    });
    // Legacy JSON broadcast — preserved during the transition. Drop
    // in the next release once the webapp has switched to reading
    // from the sentant snapshot.
    let msg = serde_json::json!({
        "type": "peer_disconnected",
        "addr": addr_str,
    }).to_string();
    let _ = state.ws_broadcast_tx.send(msg);
}

/// Broadcast a target-scoped progress notification to viewers — the
/// shared shape behind OTA / reset / capture status events. Tracks
/// B+C migration. Legacy JSON kept one release.
///
/// CBOR payload: `{0: target (text), 1: phase (text),
///                 2: size (uint, optional), 3: message (text, optional)}`.
fn emit_target_progress(
    state: &Arc<AppState>,
    event_hash: u32,
    legacy_json_type: &str,
    phase: &str,
    target: &str,
    size: Option<usize>,
    message: Option<&str>,
) {
    // Legacy JSON (preserved one release for old browsers).
    let mut json = serde_json::json!({
        "type": legacy_json_type,
        "phase": phase,
        "target": target,
    });
    if let Some(s) = size { json["size"] = serde_json::json!(s); }
    if let Some(m) = message { json["message"] = serde_json::json!(m); }
    let _ = state.ws_broadcast_tx.send(json.to_string());

    // R2-WIRE event on /ws/raw — picked up by the rocker viewer hive.
    let mut buf = vec![0u8; 64 + target.len() + phase.len() + message.map(|m| m.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + size.is_some() as usize + message.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(target));
    let _ = enc.kv(1, &r2_cbor::Value::Text(phase));
    if let Some(s) = size { let _ = enc.kv(2, &r2_cbor::Value::UInt(s as u64)); }
    if let Some(m) = message { let _ = enc.kv(3, &r2_cbor::Value::Text(m)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(event_hash, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: target.to_string(),
        ts_ms: now_ms,
        frame,
    });
}

fn emit_ota_progress(
    state: &Arc<AppState>,
    phase: &str,
    target: &str,
    size: Option<usize>,
    message: Option<&str>,
) {
    emit_target_progress(state, DASH_OTA_PROGRESS, "ota", phase, target, size, message);
}

fn emit_reset_progress(
    state: &Arc<AppState>,
    phase: &str,
    target: &str,
    message: Option<&str>,
) {
    emit_target_progress(state, DASH_RESET_PROGRESS, "reset", phase, target, None, message);
}

/// Capture-state progress event (fleet-scoped, not per-sensor like the
/// reset/OTA ones). CBOR payload:
///   `{0: phase (text), 1: peers (uint), 2: name (text, optional),
///     3: prefix (text, optional), 4: ts_ms (uint, optional)}`.
fn emit_capture_progress(
    state: &Arc<AppState>,
    phase: &str,
    peers: usize,
    name: Option<&str>,
    prefix: Option<&str>,
    ts_ms: Option<i64>,
) {
    // Legacy JSON.
    let mut json = serde_json::json!({
        "type": "capture",
        "phase": phase,
        "peers": peers,
    });
    if let Some(n) = name { json["name"] = serde_json::json!(n); }
    if let Some(p) = prefix { json["prefix"] = serde_json::json!(p); }
    if let Some(t) = ts_ms { json["ts_ms"] = serde_json::json!(t); }
    let _ = state.ws_broadcast_tx.send(json.to_string());

    // R2-WIRE event.
    let mut buf = vec![0u8; 64 + phase.len() + name.map(|s| s.len()).unwrap_or(0) + prefix.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + name.is_some() as usize + prefix.is_some() as usize + ts_ms.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(phase));
    let _ = enc.kv(1, &r2_cbor::Value::UInt(peers as u64));
    if let Some(n) = name   { let _ = enc.kv(2, &r2_cbor::Value::Text(n)); }
    if let Some(p) = prefix { let _ = enc.kv(3, &r2_cbor::Value::Text(p)); }
    if let Some(t) = ts_ms  { let _ = enc.kv(4, &r2_cbor::Value::UInt(t as u64)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CAPTURE_PROGRESS, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Access-event broadcast — payload mirrors the JSON shape that
/// `request_pending` / `request_approved` / `request_denied` /
/// `revoked` send today. CBOR payload:
///   `{0: subtype (text), 1: device_pk (text),
///     2: name (text, optional), 3: hint (text, optional)}`.
fn emit_access_event(
    state: &Arc<AppState>,
    subtype: &str,
    device_pk: &str,
    name: Option<&str>,
    hint: Option<&str>,
) {
    // Legacy JSON.
    let mut json = serde_json::json!({
        "type": "access",
        "event": subtype,
        "device_pk": device_pk,
    });
    if let Some(n) = name { json["name"] = serde_json::json!(n); }
    if let Some(h) = hint { json["hint"] = serde_json::json!(h); }
    let _ = state.ws_broadcast_tx.send(json.to_string());

    let mut buf = vec![0u8; 64 + subtype.len() + device_pk.len()
        + name.map(|s| s.len()).unwrap_or(0)
        + hint.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 2 + name.is_some() as usize + hint.is_some() as usize;
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(subtype));
    let _ = enc.kv(1, &r2_cbor::Value::Text(device_pk));
    if let Some(n) = name { let _ = enc.kv(2, &r2_cbor::Value::Text(n)); }
    if let Some(h) = hint { let _ = enc.kv(3, &r2_cbor::Value::Text(h)); }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_ACCESS_EVENT, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Device-alias change broadcast. CBOR payload:
///   `{0: device_pk (text), 1: name (text)}` — empty name means alias cleared.
fn emit_device_alias_changed(state: &Arc<AppState>, device_pk: &str, name: &str) {
    let _ = state.ws_broadcast_tx.send(serde_json::json!({
        "type": "device_alias",
        "device_pk": device_pk,
        "name": name,
    }).to_string());

    let mut buf = vec![0u8; 32 + device_pk.len() + name.len()];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let _ = enc.map(2);
    let _ = enc.kv(0, &r2_cbor::Value::Text(device_pk));
    let _ = enc.kv(1, &r2_cbor::Value::Text(name));
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_DEVICE_ALIAS_CHANGED, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Bootstrap progress — the shape is `{"event": ...}` with a nested
/// kind/data pair. R2-WIRE payload encodes it as
///   `{0: kind (text), 1: data (text, optional)}` where kind is one of
///   "Reset", "Log", "SensorFound", "SensorConnected", "Done", "Error".
/// Broadcast a `r2.dash.cmd.response` correlated to a viewer-issued
/// operator command (SPEC-R2-ROCKER-WIRE §2.1). Payload shape:
///   `{0: req_id (u32), 1: status (text),
///     2: message (text, optional), 3: kind (text)}`
///
/// `kind` is the command's name suffix without the `r2.dash.cmd.`
/// prefix (e.g. `"capture.start"`). Sent on `raw_frame_tx`, so every
/// connected viewer sees the reply; viewers correlate by `req_id`.
fn emit_cmd_response(
    state: &Arc<AppState>,
    req_id: u32,
    status: &str,
    message: Option<&str>,
    kind: &str,
) {
    emit_cmd_response_with_extras(state, req_id, status, message, kind, &[]);
}

/// Variant that appends kind-specific text pairs after the standard
/// four keys (SPEC §2.1 "Kind-specific response data"). Used by
/// snapshot/query responses where the payload is one or two JSON-
/// serialised strings — keeps the CBOR-translation surface small at
/// the cost of one JSON.parse on the viewer side.
fn emit_cmd_response_with_extras(
    state: &Arc<AppState>,
    req_id: u32,
    status: &str,
    message: Option<&str>,
    kind: &str,
    extras: &[(u64, &str)],
) {
    let extras_bytes: usize = extras.iter().map(|(_, v)| v.len() + 8).sum();
    let mut buf = vec![0u8; 64 + status.len() + kind.len()
                       + message.map(|m| m.len()).unwrap_or(0)
                       + extras_bytes];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = 3 + message.is_some() as usize + extras.len();
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::UInt(req_id as u64));
    let _ = enc.kv(1, &r2_cbor::Value::Text(status));
    if let Some(m) = message { let _ = enc.kv(2, &r2_cbor::Value::Text(m)); }
    let _ = enc.kv(3, &r2_cbor::Value::Text(kind));
    for (k, v) in extras {
        let _ = enc.kv(*k, &r2_cbor::Value::Text(v));
    }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_CMD_RESPONSE, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Decode an inbound operator-command frame received on /ws/raw.
/// Returns `(event_hash, req_id, payload_json)` on success.
///
/// The frame body is the bare R2-WIRE compact shape (12-byte header +
/// CBOR payload) — viewers SHOULD send the same shape that
/// `build_dash_frame_body` produces; the /ws/raw envelope does not
/// apply to viewer-emitted frames because the WebSocket layer already
/// provides message boundaries.
fn decode_cmd_frame(body: &[u8]) -> Option<(u32, u32, serde_json::Value)> {
    if body.len() < 12 {
        return None;
    }
    // Bytes 4..8 = event_hash (BE).
    let event_hash = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let payload = decode_cbor_payload(&body[12..])?;
    let req_id = payload.get("0").and_then(|v| v.as_u64())? as u32;
    Some((event_hash, req_id, payload))
}

/// Broadcast a R2-WIRE `r2.dash.bootstrap.progress` event preserving
/// the BootstrapEvent variant's full field set. Payload shape per
/// SPEC-R2-ROCKER-WIRE §2 row 27:
///   {0: kind (text),
///    1: message (text, optional — Log + Error),
///    2: addr    (text, optional — SensorFound + SensorConnected),
///    3: name    (text, optional — SensorFound + SensorConnected),
///    4: ip      (text, optional — SensorConnected),
///    5: count   (uint, optional — Done)}
fn emit_bootstrap_progress(state: &Arc<AppState>, event: &BootstrapEvent) {
    let mut buf = vec![0u8; 256];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    match event {
        BootstrapEvent::Log(s) => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Log"));
            let _ = enc.kv(1, &r2_cbor::Value::Text(s));
        }
        BootstrapEvent::SensorFound { addr, name } => {
            let _ = enc.map(3);
            let _ = enc.kv(0, &r2_cbor::Value::Text("SensorFound"));
            let _ = enc.kv(2, &r2_cbor::Value::Text(addr));
            let _ = enc.kv(3, &r2_cbor::Value::Text(name));
        }
        BootstrapEvent::SensorConnected { addr, name, ip } => {
            let _ = enc.map(4);
            let _ = enc.kv(0, &r2_cbor::Value::Text("SensorConnected"));
            let _ = enc.kv(2, &r2_cbor::Value::Text(addr));
            let _ = enc.kv(3, &r2_cbor::Value::Text(name));
            let _ = enc.kv(4, &r2_cbor::Value::Text(ip));
        }
        BootstrapEvent::Done { count } => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Done"));
            let _ = enc.kv(5, &r2_cbor::Value::UInt(*count as u64));
        }
        BootstrapEvent::Error(s) => {
            let _ = enc.map(2);
            let _ = enc.kv(0, &r2_cbor::Value::Text("Error"));
            let _ = enc.kv(1, &r2_cbor::Value::Text(s));
        }
    }
    let used = enc.len();
    buf.truncate(used);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_BOOTSTRAP_PROGRESS, 0, &buf);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Synthetic Reset event — emitted by the dashboard when the operator
/// (re)triggers bootstrap. Not a BootstrapEvent variant because it's
/// dashboard-side, not from r2_bootstrap. Payload `{0: "Reset"}`; the
/// webapp matches on kind and clears its log panel + sensor cards.
fn emit_bootstrap_reset(state: &Arc<AppState>) {
    // Legacy JSON for /ws/status — webapp's handleBootstrapEvent
    // matches `'Reset' in msg.event` for this case. Kept here as long
    // as /ws/status is alive; the R2-WIRE path is the canonical one
    // going forward (handleEvent on r2.dash.bootstrap.progress).
    let legacy = serde_json::json!({ "type": "bootstrap", "event": { "Reset": null } });
    let _ = state.ws_broadcast_tx.send(legacy.to_string());

    let mut buf = [0u8; 16];
    let used = {
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let _ = enc.map(1);
        let _ = enc.kv(0, &r2_cbor::Value::Text("Reset"));
        enc.len()
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let frame = build_dash_frame_body(DASH_BOOTSTRAP_PROGRESS, 0, &buf[..used]);
    let _ = state.raw_frame_tx.send(RawFrame {
        src: "dash".to_string(),
        ts_ms: now_ms,
        frame,
    });
}

/// Encode the `r2.peer.disconnected` payload per BRIDGE §3.1:
/// `{0: addr (text), 1: ts_ms (uint), 2: reason (text)}`, plus a
/// rocker-specific extension `{3: device_pk_hex (text)}` when the
/// disconnecting peer had identified itself via announce. The hex
/// form (64 chars) matches the announce's `device_pk` field so the
/// DashboardViewerSentant can look up + drop the sensor by pk
/// without keeping its own addr→pk map.
fn encode_peer_disconnected(addr: &str, ts_ms: u64, reason: &str, device_pk_hex: Option<&str>) -> Vec<u8> {
    let mut buf = vec![0u8; 64 + addr.len() + reason.len() + device_pk_hex.map(|s| s.len()).unwrap_or(0)];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    let n_keys = if device_pk_hex.is_some() { 4 } else { 3 };
    let _ = enc.map(n_keys);
    let _ = enc.kv(0, &r2_cbor::Value::Text(addr));
    let _ = enc.kv(1, &r2_cbor::Value::UInt(ts_ms));
    let _ = enc.kv(2, &r2_cbor::Value::Text(reason));
    if let Some(pk) = device_pk_hex {
        let _ = enc.kv(3, &r2_cbor::Value::Text(pk));
    }
    let used = enc.len();
    buf.truncate(used);
    buf
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
    /// Announce sig verifies AND a cert at CBOR key 8 verifies under the
    /// dashboard's TG_PUB_KEY, with `cert.device_public_key` matching the
    /// announce's `device_pk`. This is the post-Track-A normative mode.
    ValidWithCert,
    /// Announce sig verifies; no cert present (legacy TOFU mode).
    Valid,
    /// Signature bytes don't verify against the announced device_pk.
    /// Means either the firmware is buggy, the network is forging
    /// announces, or the canonical CBOR re-encoding doesn't match.
    BadSignature,
    /// Cert at CBOR key 8 either fails to verify under TG_PUB_KEY, or
    /// the cert's `device_public_key` doesn't match the announce's
    /// `device_pk`, or the cert is expired. The announce signature
    /// itself may still be well-formed; we reject because the
    /// cert-anchored chain is broken (per SPEC-R2-ROCKER-SENSOR §3.4
    /// post-cert mode).
    BadCert,
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
///
/// Track A — if the announce includes CBOR key 8 (`device_cert`,
/// 147 bytes), the dashboard ALSO verifies the cert chain under
/// `tg_pk` and checks that the cert's `device_public_key` matches
/// the announce's `device_pk`. Returns `ValidWithCert` on success,
/// `BadCert` on chain failure. Legacy announces (no key 8) fall
/// back to plain `Valid` (TOFU mode).
fn verify_announce_signature(payload: &serde_json::Value, tg_pk: &[u8; 32]) -> SigStatus {
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
    let hex_field_any = |key: &str| -> Option<Vec<u8>> {
        obj.get(key)?.as_str().and_then(|s| hex::decode(s).ok())
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
    if verifying_key.verify(body, &signature).is_err() {
        return SigStatus::BadSignature;
    }

    // Announce sig OK. Check for a cert at key 8 (Track A). The CBOR
    // decoder writes bytes(N) fields as hex strings into our JSON
    // intermediate; same accessor as `device_pk` / `nonce`. Length is
    // 147 (DEVICE_CERT_LEN) when present.
    let Some(cert_bytes) = hex_field_any("device_cert") else {
        // Legacy / pre-cert announce — TOFU accept per SPEC-R2-ROCKER-SENSOR §3.4.
        return SigStatus::Valid;
    };
    if cert_bytes.len() != 147 {
        return SigStatus::BadCert;
    }
    // 1. Verify the cert's trailing 64-byte signature over the leading
    //    83 bytes under the dashboard's TG_PUB_KEY.
    let signed = &cert_bytes[..83];
    let Ok(cert_sig_arr) = <[u8; 64]>::try_from(&cert_bytes[83..]) else {
        return SigStatus::BadCert;
    };
    let Ok(tg_vk) = VerifyingKey::from_bytes(tg_pk) else {
        return SigStatus::BadCert;
    };
    let cert_sig = Signature::from_bytes(&cert_sig_arr);
    if tg_vk.verify(signed, &cert_sig).is_err() {
        return SigStatus::BadCert;
    }
    // 2. Cert's device_public_key (bytes 2..34) must match announce's device_pk.
    if &cert_bytes[2..34] != device_pk.as_slice() {
        return SigStatus::BadCert;
    }
    // 3. Expiry check — cert.expires_at at bytes 75..83 (big-endian u64).
    let expires_at = u64::from_be_bytes(cert_bytes[75..83].try_into().unwrap_or([0u8; 8]));
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now_secs >= expires_at {
        return SigStatus::BadCert;
    }
    SigStatus::ValidWithCert
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
/// Dispatch a viewer-emitted operator-command frame received on
/// /ws/raw. Per SPEC-R2-ROCKER-WIRE §2.1, malformed frames and
/// unknown event hashes are dropped silently; everything else hits
/// the shared do_* core and yields a `r2.dash.cmd.response` reply
/// correlated by `req_id`.
async fn dispatch_cmd_frame(state: &Arc<AppState>, peer_addr: SocketAddr, body: &[u8]) {
    let (event_hash, req_id, payload) = match decode_cmd_frame(body) {
        Some(t) => t,
        None => {
            eprintln!("[ws/raw inbound] malformed frame (len={}) — ignoring", body.len());
            return;
        }
    };
    eprintln!("[ws/raw inbound] event_hash=0x{:08x} req_id={} from {}",
              event_hash, req_id, peer_addr);
    match event_hash {
        DASH_CMD_CAPTURE_START => {
            let _peers = do_capture_start(state).await;
            emit_cmd_response(state, req_id, "ok", None, "capture.start");
        }
        DASH_CMD_CAPTURE_MARK => {
            let name = payload.get("1").and_then(|v| v.as_str()).map(|s| s.to_string());
            let prefix = payload.get("2").and_then(|v| v.as_str()).map(|s| s.to_string());
            let name = match name {
                Some(n) => n,
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing name (key 1)"), "capture.mark");
                    return;
                }
            };
            match do_capture_mark(state, &name, prefix.as_deref()).await {
                Ok(_) => emit_cmd_response(state, req_id, "ok", None, "capture.mark"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "capture.mark"),
            }
        }
        DASH_CMD_CAPTURE_STOP => {
            let _peers = do_capture_stop(state).await;
            emit_cmd_response(state, req_id, "ok", None, "capture.stop");
        }
        DASH_CMD_RESET => {
            let addr = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing addr (key 1)"), "reset");
                    return;
                }
            };
            match do_reset(state, &addr).await {
                Ok((status_byte, msg)) if status_byte == 0x00 => {
                    emit_cmd_response(state, req_id, "ok", Some(&msg), "reset");
                }
                Ok((_status_byte, msg)) => {
                    emit_cmd_response(state, req_id, "err", Some(&msg), "reset");
                }
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "reset"),
            }
        }
        DASH_CMD_IDENTIFY => {
            let addr = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    emit_cmd_response(state, req_id, "err", Some("missing addr (key 1)"), "identify");
                    return;
                }
            };
            let on = payload.get("2").and_then(|v| v.as_bool()).unwrap_or(false);
            match do_identify(state, &addr, on).await {
                Ok(()) => emit_cmd_response(state, req_id, "ok", None, "identify"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "identify"),
            }
        }
        DASH_CMD_BOOTSTRAP => {
            eprintln!("[ws/raw cmd] bootstrap: calling do_bootstrap req_id={}", req_id);
            do_bootstrap(state).await;
            eprintln!("[ws/raw cmd] bootstrap: do_bootstrap returned, emitting response req_id={}", req_id);
            emit_cmd_response(state, req_id, "ok", Some("started"), "bootstrap");
            eprintln!("[ws/raw cmd] bootstrap: response emitted req_id={}", req_id);
        }
        DASH_CMD_DEVICE_ALIAS_SET => {
            let device_pk = payload.get("1").and_then(|v| v.as_str()).unwrap_or("");
            let name = payload.get("2").and_then(|v| v.as_str()).unwrap_or("");
            match do_device_alias_set(state, device_pk, name).await {
                Ok(_) => emit_cmd_response(state, req_id, "ok", None, "device.alias.set"),
                Err(msg) => emit_cmd_response(state, req_id, "err", Some(&msg), "device.alias.set"),
            }
        }
        // ── Access bundle ──────────────────────────────────────────
        //
        // KeyHolder-only ops (members/pending/approve/deny/revoke) use the
        // same loopback gate as the HTTP /api/access/* handlers (ACCESS
        // §11.1). request + check are open since they're how a new viewer
        // enters the system. The cert-handshake variant of this gate lands
        // with ACCESS v1.0.
        DASH_CMD_ACCESS_MEMBERS_QUERY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.members.query"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.members.query");
                return;
            }
            let rows = { handle.lock().await.members() };
            let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            emit_cmd_response_with_extras(state, req_id, "ok", None, "access.members.query", &[(4, &json)]);
        }
        DASH_CMD_ACCESS_PENDING_QUERY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.pending.query"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.pending.query");
                return;
            }
            let rows = { handle.lock().await.pending_requests() };
            let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            emit_cmd_response_with_extras(state, req_id, "ok", None, "access.pending.query", &[(4, &json)]);
        }
        DASH_CMD_ACCESS_CHECK => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.check"); return; }
            };
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.check"); return; }
            };
            let outcome = { handle.lock().await.check_request(&device_pk) };
            use access::CheckOutcome::*;
            match outcome {
                Approved(body) => {
                    let body_json = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
                    emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check",
                        &[(4, "approved"), (5, &body_json)]);
                }
                Pending => emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check", &[(4, "pending")]),
                Denied  => emit_cmd_response_with_extras(state, req_id, "ok", None, "access.check", &[(4, "denied")]),
                NotFound => emit_cmd_response(state, req_id, "err", Some("no such request"), "access.check"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.check"),
            }
        }
        DASH_CMD_ACCESS_APPROVE => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.approve"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.approve");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.approve"); return; }
            };
            let (outcome, response_body) = {
                let mut access = handle.lock().await;
                let o = access.approve_request(&device_pk);
                let body = access.peek_response(&device_pk);
                (o, body)
            };
            use access::ApproveOutcome::*;
            match outcome {
                Approved(pk) => {
                    let pk_hex = hex::encode(&pk[..]);
                    emit_access_event(state, "request_approved", &pk_hex, None, None);
                    // Push the JOIN_RESPONSE binary frame onto the relay's
                    // outbound channel so off-network viewers receive their
                    // bundle without polling — identical to the HTTP path.
                    if let (Some(tx), Some(body)) = (state.relay_binary_tx.as_ref(), response_body) {
                        use base64::Engine as _;
                        let tg_pk_hex = body.get("tg_pk_hex").and_then(|v| v.as_str());
                        let enc_b64   = body.get("encrypted_b64").and_then(|v| v.as_str());
                        if let (Some(tg_pk_hex), Some(enc_b64)) = (tg_pk_hex, enc_b64) {
                            let tg_pk_vec = hex::decode(tg_pk_hex).unwrap_or_default();
                            let encrypted = base64::engine::general_purpose::STANDARD
                                .decode(enc_b64).unwrap_or_default();
                            if tg_pk_vec.len() == 32 && !encrypted.is_empty() {
                                let mut tg_pk = [0u8; 32];
                                tg_pk.copy_from_slice(&tg_pk_vec);
                                let frame = relay::build_join_response(&pk, &tg_pk, &encrypted);
                                let _ = tx.send(frame);
                            } else {
                                eprintln!("[access] approve: malformed response body, can't build JOIN_RESPONSE");
                            }
                        }
                    }
                    emit_cmd_response(state, req_id, "ok", None, "access.approve");
                }
                NotFound        => emit_cmd_response(state, req_id, "err", Some("no such pending request"), "access.approve"),
                AlreadyApproved => emit_cmd_response(state, req_id, "err", Some("already approved"), "access.approve"),
                Denied          => emit_cmd_response(state, req_id, "err", Some("request was already denied"), "access.approve"),
                BadRequest      => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.approve"),
                Failed(e)       => emit_cmd_response(state, req_id, "err", Some(&e), "access.approve"),
            }
        }
        DASH_CMD_ACCESS_DENY => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.deny"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.deny");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.deny"); return; }
            };
            let outcome = { handle.lock().await.deny_request(&device_pk) };
            use access::DenyOutcome::*;
            match outcome {
                Denied(pk) => {
                    emit_access_event(state, "request_denied", &hex::encode(&pk[..]), None, None);
                    emit_cmd_response(state, req_id, "ok", None, "access.deny");
                }
                NotFound   => emit_cmd_response(state, req_id, "err", Some("no such pending request"), "access.deny"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.deny"),
            }
        }
        DASH_CMD_ACCESS_REVOKE => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.revoke"); return; }
            };
            if !is_keyholder(peer_addr) {
                emit_cmd_response(state, req_id, "err", Some("forbidden"), "access.revoke");
                return;
            }
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.revoke"); return; }
            };
            let outcome = { handle.lock().await.revoke(&device_pk) };
            use access::RevokeOutcome::*;
            match outcome {
                Revoked(pk) => {
                    emit_access_event(state, "revoked", &hex::encode(&pk[..]), None, None);
                    emit_cmd_response(state, req_id, "ok", None, "access.revoke");
                }
                NotFound   => emit_cmd_response(state, req_id, "err", Some("no such member (already revoked, or never paired)"), "access.revoke"),
                BadRequest => emit_cmd_response(state, req_id, "err", Some("device_pk must be 64 hex chars"), "access.revoke"),
                Other(e)   => emit_cmd_response(state, req_id, "err", Some(&e), "access.revoke"),
            }
        }
        DASH_CMD_ACCESS_REQUEST => {
            let handle = match state.access.as_ref() {
                Some(h) => h.clone(),
                None => { emit_cmd_response(state, req_id, "err", Some("access not configured"), "access.request"); return; }
            };
            let device_pk = match payload.get("1").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { emit_cmd_response(state, req_id, "err", Some("missing device_pk (key 1)"), "access.request"); return; }
            };
            let name = payload.get("2").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // Default hint to the WS peer's IP if absent — mirrors the
            // HTTP handler's behaviour, which derives hint from the
            // request socket.
            let hint = payload.get("3").and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| peer_addr.ip().to_string());
            let outcome = { handle.lock().await.submit_request(&device_pk, &name, &hint) };
            use access::RequestOutcome::*;
            match outcome {
                Submitted(pk) => {
                    let pk_hex = hex::encode(&pk[..]);
                    emit_access_event(state, "request_pending", &pk_hex, Some(&name), Some(&hint));
                    emit_cmd_response(state, req_id, "ok", None, "access.request");
                }
                BadRequest(msg) => emit_cmd_response(state, req_id, "err", Some(msg), "access.request"),
            }
        }
        _ => {
            // Unknown hash — log and drop per WIRE §2 "non-actionable".
            // No response emitted, per §2.1's failure-modes table.
            eprintln!("[ws/raw inbound] unknown event hash 0x{:08x} — ignoring", event_hash);
        }
    }
}

async fn ws_raw_handler(
    ws: WebSocketUpgrade,
    state: Arc<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_raw(socket, state, addr))
}

async fn handle_ws_raw(mut socket: WebSocket, state: Arc<AppState>, peer_addr: SocketAddr) {
    let mut rx = state.raw_frame_tx.subscribe();
    eprintln!("[ws/raw] viewer connected from {}", peer_addr);

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
            // Replay the last capture.state too so the Run-Control bar
            // reflects the actual recording state, not the IDLE default,
            // when the operator refreshes mid-session.
            if let Some(ref frame) = peer.last_capture_state {
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
            // Inbound: operator-plane commands per SPEC-R2-ROCKER-WIRE
            // §2.1. Viewer hives emit r2.dash.cmd.* events as bare
            // R2-WIRE compact bodies (no length prefix; WebSocket
            // provides message boundaries). We decode, dispatch to the
            // shared do_* core, and emit a r2.dash.cmd.response back
            // on raw_frame_tx (broadcast to all viewers; correlated by
            // req_id).
            inbound = socket.recv() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Binary(bytes))) => {
                        dispatch_cmd_frame(&state, peer_addr, &bytes).await;
                    }
                    _ => {} // text / ping / pong — ignore
                }
            }
            // Outbound: a fresh raw frame from the TCP listener.
            frame_msg = rx.recv() => {
                match frame_msg {
                    Ok(rf) => {
                        // Surface cmd.response and unknown low-volume events
                        // for the Track C migration triage. Acceleration (~10
                        // Hz × N peers) is too noisy; suppress it explicitly.
                        if rf.frame.len() >= 8 {
                            let h = u32::from_be_bytes([rf.frame[4], rf.frame[5], rf.frame[6], rf.frame[7]]);
                            if h != ACCELERATION {
                                eprintln!("[ws/raw outbound] event_hash=0x{:08x} src={} to {}",
                                          h, rf.src, peer_addr);
                            }
                        }
                        let envelope = encode_raw_frame_envelope(&rf);
                        if socket.send(Message::Binary(envelope.into())).await.is_err() {
                            eprintln!("[ws/raw outbound] socket.send FAILED for {} — viewer gone", peer_addr);
                            break;
                        }
                    }
                    // Lagged — viewer fell behind. Skip the gap; live data
                    // is preferred over backfill on the live wire (the
                    // SD ring is the durability layer).
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[ws/raw outbound] viewer {} LAGGED by {} frames", peer_addr, n);
                        continue;
                    }
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

// ── Capture core logic (shared by HTTP + /ws/raw operator events) ────
//
// Per SPEC-R2-ROCKER-WIRE §2.1, the legacy POST /api/capture/* routes
// and the new `r2.dash.cmd.capture.*` events on /ws/raw produce
// identical side-effects. Extracting the core into `do_capture_*`
// keeps both call sites in lockstep and makes the migration a pure
// wire-shape swap.

async fn do_capture_start(state: &Arc<AppState>) -> usize {
    // Fire an immediate sync_pulse round to every peer so capture
    // timestamps in the upcoming session share a tightly-refreshed
    // baseline. See SPEC-R2-ROCKER-CAPTURE §7.1.
    let dash_ts_ms = dash_wall_ms();
    {
        let peers = state.peers.read().await;
        for (_addr, peer) in peers.iter() {
            let req_id = (dash_ts_ms & 0xFFFF_FFFF) as u32;
            let payload = encode_sync_pulse(req_id, dash_ts_ms);
            let frame = build_dash_frame(
                DASH_SYNC_PULSE,
                (req_id & 0xFFFF) as u16,
                &payload,
            );
            let _ = peer.tx.send(frame).await;
        }
    }

    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_START, 0x0001, payload).await;
    emit_capture_progress(state, "start", sent, None, None, None);
    sent
}

async fn do_capture_mark(
    state: &Arc<AppState>,
    name: &str,
    prefix: Option<&str>,
) -> Result<(usize, i64), String> {
    if !is_valid_capture_name(name) {
        return Err("invalid name (use [A-Za-z0-9_-]{1,32})".to_string());
    }
    if let Some(p) = prefix {
        if !is_valid_capture_prefix(p) {
            return Err("invalid prefix (use [0-9_-]{1,32})".to_string());
        }
    }
    let ts_ms = dash_wall_ms() as i64;
    let payload = encode_capture_mark(ts_ms, name, prefix);
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_MARK, 0x0002, payload).await;
    emit_capture_progress(state, "mark", sent, Some(name), prefix, Some(ts_ms));
    Ok((sent, ts_ms))
}

async fn do_capture_stop(state: &Arc<AppState>) -> usize {
    let payload = encode_empty_map();
    let sent = fan_out_dash_frame(state, DASH_CAPTURE_STOP, 0x0003, payload).await;
    emit_capture_progress(state, "stop", sent, None, None, None);
    sent
}

/// `POST /api/capture/start` — legacy HTTP entry. Calls into
/// `do_capture_start` so behaviour is identical to the
/// `r2.dash.cmd.capture.start` event path.
async fn capture_start_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let sent = do_capture_start(&state).await;
    (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true, "peers": sent})))
}

/// `POST /api/capture/mark {name}` — legacy HTTP entry.
async fn capture_mark_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CaptureMarkBody>,
) -> impl IntoResponse {
    match do_capture_mark(&state, &body.name, body.prefix.as_deref()).await {
        Ok((sent, ts_ms)) => (axum::http::StatusCode::OK, Json(serde_json::json!({
            "ok": true,
            "ts_ms": ts_ms,
            "name": body.name,
            "prefix": body.prefix,
            "peers": sent,
        }))),
        Err(msg) => (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "ok": false,
            "error": msg,
        }))),
    }
}

/// `POST /api/capture/stop` — legacy HTTP entry.
async fn capture_stop_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let sent = do_capture_stop(&state).await;
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

/// `GET /api/data/merged?file=<basename>[&bin_ms=N]` — fetch the named
/// file from every connected peer and emit a wide-format CSV.
///
/// Without `bin_ms`: one row per unique `ts_ms` across the fleet, three
/// columns per sensor (`<ip>_x, <ip>_y, <ip>_z`). Cells are blank when
/// that sensor has no sample at that ts_ms — handy when sample
/// timestamps don't line up across the fleet (clock-sync jitter,
/// dropped samples). This is the raw merge.
///
/// With `bin_ms=N` (10 / 100 / 1000 / …): per-sensor samples are
/// bucketed into N-ms windows (`ts_ms = bucket_start_ms`), each
/// bucket's x/y/z averaged, then merged across sensors. Result: one
/// row per bucket per sensor — with N chosen above the sample period
/// (samples land at ~10 ms), every bucket has an entry for every
/// sensor and the timestamps line up across columns.
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
    let bin_ms: Option<i64> = q.get("bin_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0 && n <= 60_000);

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

    // Accumulators for the bin_ms aggregation path. One per (peer, bucket):
    //   (sum_x, sum_y, sum_z, count)
    // ; we render mean at emit time.
    let mut buckets: std::collections::BTreeMap<i64, Vec<Option<(f64, f64, f64, u32)>>> =
        std::collections::BTreeMap::new();

    for (peer_idx, (_, bytes)) in fetched.iter().enumerate() {
        for row in bytes.chunks_exact(ROW_BYTES) {
            let Some(ts) = parse_row_ts_ms(row) else { continue; };
            // Column layout offsets — adjacent fields are separated by
            // single commas, captured by the +1 shifts below.
            let x_str = trim_str(&row[26..37]);
            let y_str = trim_str(&row[38..49]);
            let z_str = trim_str(&row[50..61]);

            if let Some(bin) = bin_ms {
                // Bucket start = floor(ts / bin) * bin. Per-sensor running
                // sum + count so we emit mean(x/y/z) per bucket.
                let bucket = (ts / bin) * bin;
                let slot = buckets.entry(bucket)
                    .or_insert_with(|| vec![None; n_peers]);
                let x = x_str.parse::<f64>().unwrap_or(f64::NAN);
                let y = y_str.parse::<f64>().unwrap_or(f64::NAN);
                let z = z_str.parse::<f64>().unwrap_or(f64::NAN);
                if x.is_nan() || y.is_nan() || z.is_nan() { continue; }
                let entry = slot[peer_idx].get_or_insert((0.0, 0.0, 0.0, 0));
                entry.0 += x; entry.1 += y; entry.2 += z; entry.3 += 1;
            } else {
                by_ts.entry(ts)
                    .or_insert_with(|| vec![None; n_peers])
                    [peer_idx] = Some((x_str, y_str, z_str));
            }
        }
    }

    // Column-name pass: prefer the operator-assigned alias for each
    // sensor (looked up via device_pk per peer's last announce).
    // Falls back to the raw IP if no alias is set.
    let aliases_snapshot = {
        let g = state.device_aliases.lock().await;
        g.clone()
    };
    // Map peer IP → device_pk hex. SensorPeer.device_pk is set when
    // the announce is decoded (handle_sensor_connection); reading it
    // out beats scanning the raw CBOR for a literal string, which
    // doesn't work because the CBOR uses integer keys.
    let pk_by_ip: HashMap<String, String> = {
        let peers = state.peers.read().await;
        peers.iter().filter_map(|(sa, p)| {
            p.device_pk.as_ref().map(|pk| (sa.ip().to_string(), pk.clone()))
        }).collect()
    };
    let display_name_for = |ip: &str| -> String {
        if let Some(pk) = pk_by_ip.get(ip) {
            if let Some(alias) = aliases_snapshot.get(pk) {
                return alias.clone();
            }
        }
        ip.replace('.', "_")
    };

    let mut output = String::with_capacity(64 * 1024);
    // Header: ts_ms then three columns per sensor in sorted-IP order.
    output.push_str("ts_ms");
    for (sensor_name, _) in &fetched {
        let safe = display_name_for(sensor_name);
        output.push(','); output.push_str(&safe); output.push_str("_x");
        output.push(','); output.push_str(&safe); output.push_str("_y");
        output.push(','); output.push_str(&safe); output.push_str("_z");
    }
    output.push('\n');

    if bin_ms.is_some() {
        // Emit mean(x/y/z) per bucket; blank trio when a sensor had no
        // samples in that bucket.
        for (ts, slots) in &buckets {
            output.push_str(&ts.to_string());
            for slot in slots {
                output.push(',');
                if let Some((sx, sy, sz, n)) = slot {
                    let nf = *n as f64;
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sx / nf));
                    output.push(',');
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sy / nf));
                    output.push(',');
                    let _ = std::fmt::Write::write_fmt(&mut output,
                        format_args!("{:.6}", sz / nf));
                } else {
                    output.push(','); output.push(',');
                }
            }
            output.push('\n');
        }
    } else {
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
    }

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"merged-{}\"", name).parse().unwrap(),
    );
    (axum::http::StatusCode::OK, headers, output).into_response()
}

/// Path used by `load_device_aliases` / `save_device_aliases`. We
/// store under `$XDG_CONFIG_HOME` (falling back to `~/.config`) so
/// renames travel with the controller account; a fresh dashboard
/// install on the same machine picks them back up.
fn device_aliases_path() -> std::path::PathBuf {
    let cfg = std::env::var("XDG_CONFIG_HOME").ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.config")
        });
    std::path::PathBuf::from(cfg).join("r2-rocker").join("device_aliases.json")
}

fn load_device_aliases() -> HashMap<String, String> {
    let path = device_aliases_path();
    let Ok(bytes) = std::fs::read(&path) else { return HashMap::new(); };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        eprintln!("[aliases] {:?} not valid JSON — starting empty", path);
        return HashMap::new();
    };
    let Some(obj) = value.as_object() else { return HashMap::new(); };
    let mut out = HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    if !out.is_empty() {
        eprintln!("[aliases] loaded {} aliases from {:?}", out.len(), path);
    }
    out
}

fn save_device_aliases(map: &HashMap<String, String>) {
    let path = device_aliases_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(map).unwrap_or_else(|_| "{}".to_string());
    if let Err(e) = std::fs::write(&path, json) {
        eprintln!("[aliases] write {:?}: {e}", path);
    }
}

/// `GET /api/devices/aliases` — return the current device_pk → name map.
async fn device_aliases_get_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let g = state.device_aliases.lock().await;
    let map: serde_json::Map<String, serde_json::Value> = g.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    (axum::http::StatusCode::OK, Json(serde_json::Value::Object(map)))
}

/// `POST /api/devices/alias` `{device_pk, name}` — set / clear an
/// alias. Empty / null name clears. Broadcasts on /ws/status so
/// every connected dashboard browser picks up the change.
/// Shared device-alias set/clear core. Returns `Ok(final_name)` —
/// empty string means the alias was cleared. `Err(msg)` on validation
/// failure. Persists to disk and emits `r2.dash.device.alias.changed`
/// on success.
async fn do_device_alias_set(
    state: &Arc<AppState>,
    device_pk: &str,
    name: &str,
) -> Result<String, String> {
    if device_pk.is_empty() {
        return Err("device_pk required".to_string());
    }
    if device_pk.len() != 64 || !device_pk.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("device_pk must be 64 hex chars".to_string());
    }
    let trimmed = name.trim().to_string();
    let map_snapshot;
    {
        let mut g = state.device_aliases.lock().await;
        if trimmed.is_empty() {
            g.remove(device_pk);
        } else {
            // Cap + sanitise — surfaces in CSV filenames so no
            // path-busting characters.
            let clean: String = trimmed.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
                .take(64).collect();
            g.insert(device_pk.to_string(), clean);
        }
        map_snapshot = g.clone();
    }
    save_device_aliases(&map_snapshot);
    let final_name = map_snapshot.get(device_pk).cloned().unwrap_or_default();
    emit_device_alias_changed(state, device_pk, &final_name);
    Ok(final_name)
}

/// `POST /api/devices/alias` — legacy HTTP entry.
async fn device_alias_set_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let device_pk = body.get("device_pk").and_then(|v| v.as_str()).unwrap_or("");
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    match do_device_alias_set(&state, device_pk, name).await {
        Ok(_) => (axum::http::StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(msg) => (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg}))),
    }
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

pub(crate) fn encode_raw_frame_envelope(rf: &RawFrame) -> Vec<u8> {
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

async fn access_whoami_handler(
    State(state): State<Arc<AppState>>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let access = handle.lock().await;
    match access.lookup_member(&device_pk) {
        Some(row) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "enrolled": !row.revoked,
                "revoked":  row.revoked,
                "name":     row.name,
                "role":     row.role,
                "paired_at_ms": row.paired_at_ms,
            })),
        ).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "enrolled": false,
                "error":    "no such member",
            })),
        ).into_response(),
    }
}

async fn access_onboard_handler(
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
                "error": "only the KeyHolder (localhost) may fetch onboarding QRs",
            })),
        ).into_response();
    }
    let host_override = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let access = handle.lock().await;
    match access.onboard_info(host_override.as_deref()) {
        Ok(info) => (axum::http::StatusCode::OK, Json(serde_json::to_value(&info).unwrap_or_default())).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
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
            emit_access_event(&state, "revoked", &hex::encode(&pk[..]), None, None);
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

// ── Request → approve enrolment handlers ──────────────────────────────

#[derive(serde::Deserialize)]
struct RequestBody { device_pk: String, name: String }

async fn access_request_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    Json(body): Json<RequestBody>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let hint = format!("{}", addr.ip());
    let outcome = {
        let mut access = handle.lock().await;
        access.submit_request(&body.device_pk, &body.name, &hint)
    };
    use access::RequestOutcome::*;
    match outcome {
        Submitted(pk) => {
            // Operator-side notification — picked up by the Link tab's
            // /ws/status hook to show the pending row.
            emit_access_event(&state, "request_pending", &hex::encode(&pk[..]), Some(&body.name), Some(&hint));
            (axum::http::StatusCode::OK, Json(serde_json::json!({
                "ok": true,
                "device_pk": hex::encode(&pk[..]),
            }))).into_response()
        }
        BadRequest(msg) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        ).into_response(),
    }
}

async fn access_check_handler(
    State(state): State<Arc<AppState>>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let outcome = {
        let mut access = handle.lock().await;
        access.check_request(&device_pk)
    };
    use access::CheckOutcome::*;
    match outcome {
        Approved(body) => (axum::http::StatusCode::OK, Json(body)).into_response(),
        Pending => (
            axum::http::StatusCode::ACCEPTED,  // 202 — keep polling
            Json(serde_json::json!({"status": "pending"})),
        ).into_response(),
        Denied => (
            axum::http::StatusCode::GONE,
            Json(serde_json::json!({"status": "denied"})),
        ).into_response(),
        NotFound => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no such request"})),
        ).into_response(),
        BadRequest => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "device_pk must be 64 hex chars"})),
        ).into_response(),
    }
}

async fn access_pending_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "only the KeyHolder (localhost) may list pending requests in v0.1",
        }))).into_response();
    }
    let access = handle.lock().await;
    (axum::http::StatusCode::OK, Json(serde_json::json!({
        "pending": access.pending_requests(),
    }))).into_response()
}

async fn access_approve_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "only the KeyHolder (localhost) may approve requests in v0.1",
        }))).into_response();
    }
    let (outcome, response_body) = {
        let mut access = handle.lock().await;
        let o = access.approve_request(&device_pk);
        // Snapshot the cached response body so we can publish it via
        // the relay too — relay-side viewers don't poll /check, they
        // wait for an access.response text frame.
        let body = access.peek_response(&device_pk);
        (o, body)
    };
    use access::ApproveOutcome::*;
    match outcome {
        Approved(pk) => {
            let pk_hex = hex::encode(&pk[..]);
            // Operator-side: the row disappears from "pending". The
            // device polling /check will pick up the bundle next.
            emit_access_event(&state, "request_approved", &pk_hex, None, None);
            // Relay-side: build a notekeeper-format JOIN_RESPONSE
            // binary frame and push it onto the relay's outbound
            // channel. Off-network viewers receive it as a raw
            // binary frame over the relay (r2-relay only forwards
            // binary peer-to-peer) — no polling needed.
            //
            // Frame: [0xFF, 0x02, devicePk(32), tgPk(32), encrypted]
            if let (Some(tx), Some(body)) = (state.relay_binary_tx.as_ref(), response_body) {
                use base64::Engine as _;
                let tg_pk_hex = body.get("tg_pk_hex").and_then(|v| v.as_str());
                let enc_b64   = body.get("encrypted_b64").and_then(|v| v.as_str());
                if let (Some(tg_pk_hex), Some(enc_b64)) = (tg_pk_hex, enc_b64) {
                    let tg_pk_vec = hex::decode(tg_pk_hex).unwrap_or_default();
                    let encrypted = base64::engine::general_purpose::STANDARD
                        .decode(enc_b64).unwrap_or_default();
                    if tg_pk_vec.len() == 32 && !encrypted.is_empty() {
                        let mut tg_pk = [0u8; 32];
                        tg_pk.copy_from_slice(&tg_pk_vec);
                        let frame = relay::build_join_response(&pk, &tg_pk, &encrypted);
                        let _ = tx.send(frame);
                    } else {
                        eprintln!("[access] approve: malformed response body, can't build JOIN_RESPONSE");
                    }
                }
            }
            (axum::http::StatusCode::OK, Json(serde_json::json!({
                "ok": true,
                "device_pk": pk_hex,
            }))).into_response()
        }
        NotFound => (axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no such pending request"}))).into_response(),
        AlreadyApproved => (axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "already approved"}))).into_response(),
        Denied => (axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "request was already denied"}))).into_response(),
        BadRequest => (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "device_pk must be 64 hex chars"}))).into_response(),
        Failed(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e}))).into_response(),
    }
}

async fn access_deny_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    Path(device_pk): Path<String>,
) -> impl IntoResponse {
    let handle = match require_access(&state).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !is_keyholder(addr) {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "only the KeyHolder (localhost) may deny requests in v0.1",
        }))).into_response();
    }
    let outcome = {
        let mut access = handle.lock().await;
        access.deny_request(&device_pk)
    };
    use access::DenyOutcome::*;
    match outcome {
        Denied(pk) => {
            let pk_hex = hex::encode(&pk[..]);
            emit_access_event(&state, "request_denied", &pk_hex, None, None);
            (axum::http::StatusCode::OK, Json(serde_json::json!({
                "ok": true,
                "device_pk": pk_hex,
            }))).into_response()
        }
        NotFound => (axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no such pending request"}))).into_response(),
        BadRequest => (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "device_pk must be 64 hex chars"}))).into_response(),
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
