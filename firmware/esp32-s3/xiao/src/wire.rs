//! Inline R2-WIRE + FNV + minimal CBOR encoder.
//!
//! Hand-rolled rather than using r2-fnv / r2-wire / r2-cbor as crates, to
//! keep the firmware self-contained and the dependency tree small while
//! we iterate. Refactor up to the vendored crates once the protocol
//! shape stabilises in `r2-rocker/crates/`.
//!
//! References:
//! * `SPEC-R2-ROCKER-WIRE.md` for the event names + payload schemas.
//! * `r2-core/crates/r2-wire/src/{types.rs,compact.rs}` for the canonical
//!   encoder this is bit-for-bit equivalent to (compact frame, MsgType =
//!   Event, no route, no HMAC, version 0).

#![allow(dead_code)] // some helpers are used only by some events

// ── FNV-1a 32-bit ─────────────────────────────────────────────────────────

const FNV_OFFSET_BASIS: u32 = 0x811C_9DC5;
const FNV_PRIME: u32 = 0x0100_0193;

/// Raw FNV-1a 32-bit over a byte string. Equivalent to `r2_fnv::fnv1a_32`.
pub const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut h = FNV_OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

// Pre-computed event hashes. Each is a const fn call, so the values are
// resolved at compile time. They MUST match the hashes the dashboard uses.

pub const EVT_SENSOR_ANNOUNCE:    u32 = fnv1a_32(b"r2.sensor.announce");
pub const EVT_SENSOR_ACCELERATION: u32 = fnv1a_32(b"r2.sensor.acceleration");
pub const EVT_SENSOR_BATTERY:      u32 = fnv1a_32(b"r2.sensor.battery");
pub const EVT_SENSOR_STATUS:       u32 = fnv1a_32(b"r2.sensor.status");
pub const EVT_SENSOR_EVENT_LOG:    u32 = fnv1a_32(b"r2.sensor.event.log");

// ── R2-WIRE compact frame ────────────────────────────────────────────────

/// Compact frame builder for `MsgType=Event`, no route, no HMAC, version 0.
///
/// Layout (12-byte header + payload):
///     byte 0: (version<<6) | (msg_type<<3) | flags
///     byte 1: (ttl<<4) | k
///     bytes 2-3:   msg_id     (BE u16)
///     bytes 4-7:   event_hash (BE u32)
///     bytes 8-11:  target     (BE u32)
///     bytes 12..:  payload
///
/// `flags` bit 0 = mcu_origin = 1 here. Bits 1, 2 = 0 (no HMAC, no route).
pub fn encode_event_compact(
    out: &mut [u8],
    msg_id: u16,
    event_hash: u32,
    payload: &[u8],
) -> usize {
    const TTL: u8 = 5;
    const K: u8 = 3;
    const VERSION: u8 = 0;
    const MSG_TYPE_EVENT: u8 = 0;
    const FLAG_MCU_ORIGIN: u8 = 0b001;

    let total = 12 + payload.len();
    assert!(out.len() >= total, "encode_event_compact buffer too small");

    out[0] = (VERSION << 6) | (MSG_TYPE_EVENT << 3) | FLAG_MCU_ORIGIN;
    out[1] = (TTL << 4) | (K & 0x0F);
    out[2..4].copy_from_slice(&msg_id.to_be_bytes());
    out[4..8].copy_from_slice(&event_hash.to_be_bytes());
    out[8..12].copy_from_slice(&0u32.to_be_bytes()); // target = broadcast
    out[12..total].copy_from_slice(payload);
    total
}

/// Write a TCP-framed R2-WIRE frame (`u16 BE length` + `frame bytes`) into
/// `out`. The dashboard expects this framing on port 21042.
pub fn frame_for_tcp(
    out: &mut [u8],
    msg_id: u16,
    event_hash: u32,
    payload: &[u8],
) -> usize {
    let frame_len = 12 + payload.len();
    assert!(frame_len <= u16::MAX as usize, "frame too large for u16 prefix");
    assert!(out.len() >= 2 + frame_len, "frame_for_tcp buffer too small");
    out[0..2].copy_from_slice(&(frame_len as u16).to_be_bytes());
    encode_event_compact(&mut out[2..], msg_id, event_hash, payload);
    2 + frame_len
}

// ── Minimal CBOR encoder ─────────────────────────────────────────────────
//
// Just the subset we need: maps with integer keys, integer values
// (signed and unsigned), bytes, text, bool. Per RFC 8949 deterministic
// encoding (RFC 8949 §4.2): smallest-form integers + lexicographic key
// ordering (we use ascending integer keys which already are).

const MT_UINT:   u8 = 0x00;
const MT_NEGINT: u8 = 0x20;
const MT_BYTES:  u8 = 0x40;
const MT_TEXT:   u8 = 0x60;
const MT_ARRAY:  u8 = 0x80;
const MT_MAP:    u8 = 0xA0;
const MT_OTHER:  u8 = 0xE0;

pub struct CborWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> CborWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize { self.pos }
    pub fn as_bytes(&self) -> &[u8] { &self.buf[..self.pos] }

    fn put(&mut self, b: u8) {
        self.buf[self.pos] = b;
        self.pos += 1;
    }
    fn put_slice(&mut self, s: &[u8]) {
        self.buf[self.pos..self.pos + s.len()].copy_from_slice(s);
        self.pos += s.len();
    }

    /// Emit major-type + length using the smallest CBOR head form.
    fn head(&mut self, mt: u8, len: u64) {
        if len <= 23 {
            self.put(mt | (len as u8));
        } else if len <= u8::MAX as u64 {
            self.put(mt | 0x18);
            self.put(len as u8);
        } else if len <= u16::MAX as u64 {
            self.put(mt | 0x19);
            self.put_slice(&(len as u16).to_be_bytes());
        } else if len <= u32::MAX as u64 {
            self.put(mt | 0x1A);
            self.put_slice(&(len as u32).to_be_bytes());
        } else {
            self.put(mt | 0x1B);
            self.put_slice(&len.to_be_bytes());
        }
    }

    pub fn map(&mut self, n: usize) { self.head(MT_MAP, n as u64); }
    pub fn array(&mut self, n: usize) { self.head(MT_ARRAY, n as u64); }

    pub fn key(&mut self, k: u64) { self.head(MT_UINT, k); }

    pub fn u(&mut self, v: u64) { self.head(MT_UINT, v); }

    pub fn i(&mut self, v: i64) {
        if v >= 0 {
            self.head(MT_UINT, v as u64);
        } else {
            // CBOR negint: encodes -1-n where n is the head value.
            self.head(MT_NEGINT, (-(v + 1)) as u64);
        }
    }

    pub fn bool(&mut self, b: bool) {
        // Major type 7, simple value 20=false, 21=true.
        self.put(MT_OTHER | (if b { 21 } else { 20 }));
    }

    pub fn bytes(&mut self, s: &[u8]) {
        self.head(MT_BYTES, s.len() as u64);
        self.put_slice(s);
    }

    pub fn text(&mut self, s: &str) {
        self.head(MT_TEXT, s.len() as u64);
        self.put_slice(s.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_known() {
        // Anchor the algorithm against r2-fnv's documented test vector.
        assert_eq!(fnv1a_32(b"#ping"), 0x7CB36B0A);
    }

    #[test]
    fn cbor_smallest_form() {
        let mut buf = [0u8; 64];
        let mut w = CborWriter::new(&mut buf);
        w.map(3);
        w.key(0); w.i(-980);
        w.key(1); w.i(-456);
        w.key(2); w.u(32);
        // {0:-980, 1:-456, 2:32}: A3 00 39 03 D3  01 39 01 C7  02 18 20
        let expected = [
            0xA3,
            0x00, 0x39, 0x03, 0xD3,
            0x01, 0x39, 0x01, 0xC7,
            0x02, 0x18, 0x20,
        ];
        assert_eq!(w.as_bytes(), &expected[..]);
    }

    #[test]
    fn r2_wire_compact_event_layout() {
        let mut frame = [0u8; 16];
        let n = encode_event_compact(&mut frame, 0x1234, 0xDEAD_BEEF, &[0xAA, 0xBB]);
        assert_eq!(n, 14);
        // byte 0: version=0, msg_type=Event=0, flags=mcu_origin=001 → 0x01
        assert_eq!(frame[0], 0x01);
        // byte 1: ttl=5, k=3 → 0x53
        assert_eq!(frame[1], 0x53);
        // bytes 2-3: msg_id BE
        assert_eq!(&frame[2..4], &[0x12, 0x34]);
        // bytes 4-7: event_hash BE
        assert_eq!(&frame[4..8], &[0xDE, 0xAD, 0xBE, 0xEF]);
        // bytes 8-11: target = 0
        assert_eq!(&frame[8..12], &[0, 0, 0, 0]);
        // payload
        assert_eq!(&frame[12..14], &[0xAA, 0xBB]);
    }
}
