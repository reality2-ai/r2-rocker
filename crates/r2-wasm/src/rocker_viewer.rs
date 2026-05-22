//! Dashboard-viewer sentant for r2-rocker.
//!
//! Track D of the R2-conformance roadmap (audit
//! `audits/2026-05-23-architectural-gaps.md` Finding C). The r2-rocker
//! webapp instantiates `R2RockerHive` (see `rocker_hive.rs`), which
//! registers this sentant on the EventBus. JavaScript forwards every
//! R2-WIRE event the dashboard publishes on `/ws/raw` into the hive
//! via `R2RockerHive::send_event(hash, payload)`; this sentant
//! decodes the CBOR payload and maintains a per-sensor state table.
//! The hive holds a clone of an `Rc<RefCell<Inner>>` pointing at the
//! same state so JS can read it back via `peek_state()`.
//!
//! This first slice is observation-only: JS continues to own the UI
//! and renders from its own state. The sentant runs in parallel as a
//! self-consistent record of the same data, in preparation for the
//! follow-up commits that move UI rendering through the sentant and
//! Tracks B+C which migrate the operator-plane wire shape.
//!
//! ## Thread safety
//!
//! The hive runs in a single-threaded browser environment. Rc +
//! RefCell are the natural primitives. The Sentant trait doesn't
//! require Send/Sync; `EventBus` doesn't either on wasm32. If a
//! native-target test rig ever wants to host this sentant it'll need
//! to wrap the inner state in `Arc<Mutex<_>>` instead — out of scope
//! here.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use r2_engine::action_buf::ActionBuf;
use r2_engine::event::{Event, EventHash};
use r2_engine::sentant::{Sentant, StateId};

// ── Event hashes — mirror SPEC-R2-ROCKER-WIRE §2 inventory ───────────

const EVT_SENSOR_ANNOUNCE:      EventHash = r2_fnv::fnv1a_32(b"r2.sensor.announce");
const EVT_SENSOR_ACCELERATION:  EventHash = r2_fnv::fnv1a_32(b"r2.sensor.acceleration");
const EVT_SENSOR_BATTERY:       EventHash = r2_fnv::fnv1a_32(b"r2.sensor.battery");
const EVT_SENSOR_STATUS:        EventHash = r2_fnv::fnv1a_32(b"r2.sensor.status");
const EVT_SENSOR_CAPTURE_STATE: EventHash = r2_fnv::fnv1a_32(b"r2.sensor.capture.state");

const CLASS_HASH: EventHash = r2_fnv::fnv1a_32(b"nz.ac.auckland.rocker.viewer");

const SUBSCRIPTIONS: &[u32] = &[
    EVT_SENSOR_ANNOUNCE,
    EVT_SENSOR_ACCELERATION,
    EVT_SENSOR_BATTERY,
    EVT_SENSOR_STATUS,
    EVT_SENSOR_CAPTURE_STATE,
];

/// Per-sensor record. Mirrors the JS-side state in `webapp/index.html`
/// today, in a form the rest of the sentant logic can read without
/// touching JS. Keyed by `device_pk` (32-byte Ed25519 pk, hex-encoded
/// to 64 chars).
#[derive(Default, Clone, Debug)]
pub struct SensorState {
    pub device_pk_hex: String,
    pub hostname: Option<String>,
    pub fw_ver: Option<String>,
    /// True iff the most recent announce carried CBOR key 8 (the
    /// KeyHolder-signed DeviceCertificate). Equivalent to "this sensor
    /// is a formal TG member" once Track A is fully rolled out.
    pub has_cert: bool,
    pub last_seq: u64,
    pub last_ts_ms: u64,
    pub battery_pct: Option<u8>,
    pub fsm_state: Option<u8>,
    pub capture_state: Option<u8>,
    pub capture_file: Option<String>,
    /// Accumulated count of acceleration samples seen since startup —
    /// useful as a "hive is alive" telemetry signal during Track D
    /// bring-up.
    pub sample_count: u64,
}

/// Shared sentant state — held by the sentant for writes, and by the
/// hive (via a clone of the same `Rc`) for `peek_state()` reads.
#[derive(Default)]
pub struct Inner {
    sensors: BTreeMap<String, SensorState>,
    event_count: u64,
    /// The most-recently-mentioned device_pk_hex. Used to scope
    /// per-source events (acceleration, status, capture.state) that
    /// don't themselves carry a device_pk. Set on each announce.
    /// This is a v0.1 simplification — sensor TCP frames are bound
    /// to a SocketAddr but not to a pk; a future slice could thread
    /// pk through the JS → hive boundary.
    last_pk: Option<String>,
}

impl Inner {
    /// Render the current state to a JSON string for JS consumers.
    /// Kept hand-rolled rather than going through serde-json to avoid
    /// pulling that dep into the WASM bundle.
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256 + 128 * self.sensors.len());
        out.push('{');
        out.push_str("\"event_count\":");
        push_u64(&mut out, self.event_count);
        out.push_str(",\"sensors\":[");
        for (i, (_pk, s)) in self.sensors.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push('{');
            out.push_str("\"device_pk\":\"");
            out.push_str(&s.device_pk_hex);
            out.push('"');
            json_kv_text_opt(&mut out, "hostname", s.hostname.as_deref());
            json_kv_text_opt(&mut out, "fw_ver", s.fw_ver.as_deref());
            out.push_str(",\"has_cert\":");
            out.push_str(if s.has_cert { "true" } else { "false" });
            out.push_str(",\"last_seq\":");
            push_u64(&mut out, s.last_seq);
            out.push_str(",\"last_ts_ms\":");
            push_u64(&mut out, s.last_ts_ms);
            json_kv_u64_opt(&mut out, "battery_pct", s.battery_pct.map(|v| v as u64));
            json_kv_u64_opt(&mut out, "fsm_state", s.fsm_state.map(|v| v as u64));
            json_kv_u64_opt(&mut out, "capture_state", s.capture_state.map(|v| v as u64));
            json_kv_text_opt(&mut out, "capture_file", s.capture_file.as_deref());
            out.push_str(",\"sample_count\":");
            push_u64(&mut out, s.sample_count);
            out.push('}');
        }
        out.push_str("]}");
        out
    }
}

pub struct DashboardViewerSentant {
    inner: Rc<RefCell<Inner>>,
    state: StateId,
}

impl DashboardViewerSentant {
    /// Construct the sentant and return both it (for registering on the
    /// bus) and a clone of the `Rc<RefCell<Inner>>` (for the hive to
    /// hold so it can serve `peek_state()`).
    pub fn new() -> (Box<dyn Sentant>, Rc<RefCell<Inner>>) {
        let inner = Rc::new(RefCell::new(Inner::default()));
        let s = DashboardViewerSentant { inner: inner.clone(), state: 0 };
        (Box::new(s), inner)
    }
}

impl Sentant for DashboardViewerSentant {
    fn handle_event(&mut self, event: &Event, _actions: &mut ActionBuf) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else { return };
        inner.event_count = inner.event_count.wrapping_add(1);
        match event.hash {
            EVT_SENSOR_ANNOUNCE => {
                let map = decode_top_level_map(event.payload);
                let Some(device_pk_hex) = map_get_bytes_hex(&map, 0) else { return };
                let hostname = map_get_text(&map, 1);
                let fw_ver = map_get_text(&map, 2);
                let has_cert = map_get_bytes_len(&map, 8) == Some(147);
                inner.last_pk = Some(device_pk_hex.clone());
                let s = inner.sensors.entry(device_pk_hex.clone()).or_insert_with(|| SensorState {
                    device_pk_hex: device_pk_hex.clone(),
                    ..Default::default()
                });
                if hostname.is_some() { s.hostname = hostname; }
                if fw_ver.is_some()   { s.fw_ver   = fw_ver;   }
                s.has_cert = has_cert;
            }
            EVT_SENSOR_ACCELERATION => {
                let map = decode_top_level_map(event.payload);
                let Some(seq) = map_get_u64(&map, 0) else { return };
                let ts_ms = map_get_u64(&map, 1).unwrap_or(0);
                if let Some(pk) = inner.last_pk.clone() {
                    if let Some(s) = inner.sensors.get_mut(&pk) {
                        s.last_seq = seq;
                        if ts_ms > 0 { s.last_ts_ms = ts_ms; }
                        s.sample_count = s.sample_count.wrapping_add(1);
                    }
                }
            }
            EVT_SENSOR_BATTERY => {
                let map = decode_top_level_map(event.payload);
                let pct = map_get_u64(&map, 2).map(|v| v.min(100) as u8);
                if let Some(pk) = inner.last_pk.clone() {
                    if let Some(s) = inner.sensors.get_mut(&pk) {
                        s.battery_pct = pct;
                    }
                }
            }
            EVT_SENSOR_STATUS => {
                let map = decode_top_level_map(event.payload);
                let st = map_get_u64(&map, 0).map(|v| v as u8);
                let ts_ms = map_get_u64(&map, 1).unwrap_or(0);
                if let Some(pk) = inner.last_pk.clone() {
                    if let Some(s) = inner.sensors.get_mut(&pk) {
                        s.fsm_state = st;
                        if ts_ms > 0 { s.last_ts_ms = ts_ms; }
                    }
                }
            }
            EVT_SENSOR_CAPTURE_STATE => {
                let map = decode_top_level_map(event.payload);
                let st = map_get_u64(&map, 0).map(|v| v as u8);
                let file = map_get_text(&map, 1);
                if let Some(pk) = inner.last_pk.clone() {
                    if let Some(s) = inner.sensors.get_mut(&pk) {
                        s.capture_state = st;
                        s.capture_file = file;
                    }
                }
            }
            _ => {}
        }
    }

    fn state(&self) -> StateId { self.state }
    fn class_hash(&self) -> u32 { CLASS_HASH }
    fn name(&self) -> &str { "rocker-viewer" }
    fn subscriptions(&self) -> &[u32] { SUBSCRIPTIONS }
}

// ── Tiny CBOR decode helpers ────────────────────────────────────────
//
// Top-level CBOR maps with small integer keys (≤ 23) and 1- or 2-byte
// heads. A full r2-cbor decode would pull in more code than necessary
// for what's effectively a linear scan; these helpers walk the bytes
// inline.

#[derive(Default)]
struct CborMap<'a> {
    entries: Vec<(u8, CborValue<'a>)>,
}

enum CborValue<'a> {
    UInt(u64),
    Bytes(&'a [u8]),
    Text(&'a str),
    Bool(bool),
    Other,
}

fn decode_top_level_map(buf: &[u8]) -> CborMap<'_> {
    let mut out = CborMap::default();
    let Some((n_entries, mut p)) = decode_head(buf, 0xA0) else { return out };
    for _ in 0..n_entries {
        let Some((key, np)) = decode_uint(buf, p) else { return out };
        let Some((val, np2)) = decode_value(buf, np) else { return out };
        if let Ok(k_u8) = u8::try_from(key) {
            out.entries.push((k_u8, val));
        }
        p = np2;
    }
    out
}

fn decode_head(buf: &[u8], major: u8) -> Option<(u64, usize)> {
    if buf.is_empty() { return None; }
    let b = buf[0];
    if (b & 0xE0) != major { return None; }
    let info = b & 0x1F;
    decode_size(buf, 1, info)
}

fn decode_size(buf: &[u8], mut p: usize, info: u8) -> Option<(u64, usize)> {
    let val = match info {
        n @ 0..=23 => n as u64,
        24 => { let v = *buf.get(p)? as u64; p += 1; v }
        25 => { let v = u16::from_be_bytes(buf.get(p..p+2)?.try_into().ok()?) as u64; p += 2; v }
        26 => { let v = u32::from_be_bytes(buf.get(p..p+4)?.try_into().ok()?) as u64; p += 4; v }
        27 => { let v = u64::from_be_bytes(buf.get(p..p+8)?.try_into().ok()?); p += 8; v }
        _ => return None,
    };
    Some((val, p))
}

fn decode_uint(buf: &[u8], p: usize) -> Option<(u64, usize)> {
    if p >= buf.len() { return None; }
    let b = buf[p];
    if (b & 0xE0) != 0x00 { return None; }
    decode_size(buf, p + 1, b & 0x1F)
}

fn decode_value(buf: &[u8], p: usize) -> Option<(CborValue<'_>, usize)> {
    if p >= buf.len() { return None; }
    let b = buf[p];
    let major = b & 0xE0;
    let info = b & 0x1F;
    match major {
        0x00 => {
            let (v, np) = decode_size(buf, p + 1, info)?;
            Some((CborValue::UInt(v), np))
        }
        0x40 => {
            let (n, np) = decode_size(buf, p + 1, info)?;
            let n = n as usize;
            let end = np.checked_add(n)?;
            let bytes = buf.get(np..end)?;
            Some((CborValue::Bytes(bytes), end))
        }
        0x60 => {
            let (n, np) = decode_size(buf, p + 1, info)?;
            let n = n as usize;
            let end = np.checked_add(n)?;
            let s = core::str::from_utf8(buf.get(np..end)?).ok()?;
            Some((CborValue::Text(s), end))
        }
        0xE0 => {
            match info {
                20 => Some((CborValue::Bool(false), p + 1)),
                21 => Some((CborValue::Bool(true),  p + 1)),
                _  => Some((CborValue::Other, p + 1)),
            }
        }
        _ => Some((CborValue::Other, p + 1)),
    }
}

fn map_get<'a, 'b>(map: &'b CborMap<'a>, key: u8) -> Option<&'b CborValue<'a>> {
    map.entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn map_get_u64(map: &CborMap<'_>, key: u8) -> Option<u64> {
    match map_get(map, key)? {
        CborValue::UInt(v) => Some(*v),
        _ => None,
    }
}

fn map_get_text(map: &CborMap<'_>, key: u8) -> Option<String> {
    match map_get(map, key)? {
        CborValue::Text(s) => Some(s.to_string()),
        _ => None,
    }
}

fn map_get_bytes_hex(map: &CborMap<'_>, key: u8) -> Option<String> {
    match map_get(map, key)? {
        CborValue::Bytes(b) => Some(hex_of(b)),
        _ => None,
    }
}

fn map_get_bytes_len(map: &CborMap<'_>, key: u8) -> Option<usize> {
    match map_get(map, key)? {
        CborValue::Bytes(b) => Some(b.len()),
        _ => None,
    }
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

// ── Tiny JSON helpers (avoid pulling serde into the WASM bundle) ────

fn push_u64(out: &mut String, v: u64) {
    out.push_str(&format!("{}", v));
}

fn json_kv_text_opt(out: &mut String, key: &str, val: Option<&str>) {
    if let Some(v) = val {
        out.push_str(",\"");
        out.push_str(key);
        out.push_str("\":\"");
        for c in v.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        }
        out.push('"');
    }
}

fn json_kv_u64_opt(out: &mut String, key: &str, val: Option<u64>) {
    if let Some(v) = val {
        out.push_str(",\"");
        out.push_str(key);
        out.push_str("\":");
        push_u64(out, v);
    }
}
