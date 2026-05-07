//! R2-WIRE: Alloc-based wire protocol framing.
//!
//! Thin wrapper over the `r2-wire` crate, providing owned types (`Vec<u8>` payloads,
//! `Vec<u16>` routes) for platforms with a heap. The crate handles all encoding logic.
//!
//! For `no_std` / fixed-buffer framing, depend on `r2-wire` directly.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

// Re-export crate types that are identical
pub use r2_wire::{MsgType, WireError, FrameHeader, k_split};
pub use r2_wire::{encode_byte0, encode_byte1, decode_byte0, decode_byte1};

// ── Alloc-based owned types ─────────────────────────────────────

/// Compact route stack with heap-allocated entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactRoute {
    /// Route entries (FNV hash upper 16 bits of each hive UUID).
    pub entries: Vec<u16>,
}

/// Compact message with owned payload.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactMessage {
    /// Protocol version (must be 0).
    pub version: u8,
    /// Message type.
    pub msg_type: MsgType,
    /// Route stack present.
    pub has_route: bool,
    /// HMAC tag appended.
    pub has_hmac: bool,
    /// Originated from constrained MCU.
    pub mcu_origin: bool,
    /// Time-to-live (0–15).
    pub ttl: u8,
    /// Spray-and-wait budget (0–15).
    pub k: u8,
    /// Message ID (16-bit).
    pub msg_id: u16,
    /// FNV-1a hash of event name.
    pub event_hash: u32,
    /// Target (32-bit FNV hash or special value).
    pub target: u32,
    /// Route stack (if has_route).
    pub route: Option<CompactRoute>,
    /// CBOR payload (owned).
    pub payload: Vec<u8>,
    /// Truncated HMAC tag (if has_hmac).
    pub hmac_tag: Option<[u8; 8]>,
}

/// Extended message with owned payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedMessage {
    /// Protocol version.
    pub version: u8,
    /// Message type.
    pub msg_type: MsgType,
    /// Route stack present.
    pub has_route: bool,
    /// HMAC tag appended.
    pub has_hmac: bool,
    /// MCU origin flag.
    pub mcu_origin: bool,
    /// Time-to-live.
    pub ttl: u8,
    /// Spray-and-wait budget.
    pub k: u8,
    /// Message ID (32-bit).
    pub msg_id: u32,
    /// FNV-1a hash of event name.
    pub event_hash: u32,
    /// Payload length (authoritative).
    pub payload_len: u32,
    /// Target trust group.
    pub target_group: u32,
    /// Target hive within group.
    pub target_hive: u32,
    /// Route stack (if has_route).
    pub route: Option<Vec<u32>>,
    /// CBOR payload (owned).
    pub payload: Vec<u8>,
    /// Full HMAC tag (if has_hmac).
    pub hmac_tag: Option<[u8; 32]>,
}

// ── Conversion helpers: alloc ↔ no_std crate types ──────────────

impl CompactMessage {
    /// Convert to borrowed crate type for encoding.
    fn to_crate_msg(&self) -> r2_wire::CompactMessage<'_> {
        let route_stack = self.route.as_ref().map(|r| {
            let mut rs = r2_wire::CompactRouteStack::new();
            rs.len = r.entries.len().min(8) as u8;
            for (i, &e) in r.entries.iter().take(8).enumerate() {
                rs.entries[i] = e;
            }
            rs
        });
        r2_wire::CompactMessage {
            header: r2_wire::CompactHeader {
                version: self.version,
                msg_type: self.msg_type,
                flags: r2_wire::Flags {
                    has_route: self.has_route,
                    has_hmac: self.has_hmac,
                    mcu_origin: self.mcu_origin,
                },
                ttl: self.ttl,
                k: self.k,
                msg_id: self.msg_id,
                event_hash: self.event_hash,
                target: self.target,
            },
            route: route_stack,
            payload: &self.payload,
            hmac_tag: self.hmac_tag,
        }
    }

    fn from_crate_msg(cm: &r2_wire::CompactMessage<'_>) -> Self {
        let h = &cm.header;
        CompactMessage {
            version: h.version,
            msg_type: h.msg_type,
            has_route: h.flags.has_route,
            has_hmac: h.flags.has_hmac,
            mcu_origin: h.flags.mcu_origin,
            ttl: h.ttl,
            k: h.k,
            msg_id: h.msg_id,
            event_hash: h.event_hash,
            target: h.target,
            route: cm.route.map(|r| CompactRoute {
                entries: r.entries[..r.len as usize].to_vec(),
            }),
            payload: cm.payload.to_vec(),
            hmac_tag: cm.hmac_tag,
        }
    }
}

// ── Encode/decode: delegate to crate ────────────────────────────

/// Encode a compact message to owned bytes.
pub fn encode_compact(msg: &CompactMessage) -> Vec<u8> {
    let flags = r2_wire::Flags {
        has_route: msg.has_route,
        has_hmac: msg.has_hmac,
        mcu_origin: msg.mcu_origin,
    };
    let mut route_stack = None;
    if let Some(r) = &msg.route {
        let mut rs = r2_wire::CompactRouteStack::new();
        rs.len = r.entries.len().min(8) as u8;
        for (i, &e) in r.entries.iter().take(8).enumerate() {
            rs.entries[i] = e;
        }
        route_stack = Some(rs);
    }
    let cm = r2_wire::CompactMessage {
        header: r2_wire::CompactHeader {
            version: msg.version,
            msg_type: msg.msg_type,
            flags,
            ttl: msg.ttl,
            k: msg.k,
            msg_id: msg.msg_id,
            event_hash: msg.event_hash,
            target: msg.target,
        },
        route: route_stack,
        payload: &msg.payload,
        hmac_tag: msg.hmac_tag,
    };
    // Allocate worst-case buffer
    let max_size = 12 + 1 + 8 * 2 + msg.payload.len() + 8;
    let mut buf = alloc::vec![0u8; max_size];
    let len = r2_wire::encode_compact(&cm, &mut buf).expect("buffer sized correctly");
    buf.truncate(len);
    buf
}

/// Decode a compact message from bytes (returns owned types).
pub fn decode_compact(data: &[u8]) -> Result<CompactMessage, WireError> {
    let cm = r2_wire::decode_compact(data)?;
    Ok(CompactMessage::from_crate_msg(&cm))
}

/// Encode an extended message to owned bytes.
pub fn encode_extended(msg: &ExtendedMessage) -> Vec<u8> {
    let flags = r2_wire::Flags {
        has_route: msg.has_route,
        has_hmac: msg.has_hmac,
        mcu_origin: msg.mcu_origin,
    };
    let mut route_stack = None;
    if let Some(r) = &msg.route {
        let mut rs = r2_wire::ExtendedRouteStack::new();
        rs.len = r.len().min(8) as u8;
        for (i, &e) in r.iter().take(8).enumerate() {
            rs.entries[i] = e;
        }
        route_stack = Some(rs);
    }
    let em = r2_wire::ExtendedMessage {
        header: r2_wire::ExtendedHeader {
            version: msg.version,
            msg_type: msg.msg_type,
            flags,
            ttl: msg.ttl,
            k: msg.k,
            msg_id: msg.msg_id,
            event_hash: msg.event_hash,
            payload_len: msg.payload_len,
            target_group: msg.target_group,
            target_hive: msg.target_hive,
        },
        route: route_stack,
        payload: &msg.payload,
        hmac_tag: msg.hmac_tag,
    };
    let max_size = 22 + 1 + 8 * 4 + msg.payload.len() + 32;
    let mut buf = alloc::vec![0u8; max_size];
    let len = r2_wire::encode_extended(&em, &mut buf).expect("buffer sized correctly");
    buf.truncate(len);
    buf
}

/// Decode an extended message from bytes (returns owned types).
pub fn decode_extended(data: &[u8]) -> Result<ExtendedMessage, WireError> {
    let em = r2_wire::decode_extended(data)?;
    let h = &em.header;
    Ok(ExtendedMessage {
        version: h.version,
        msg_type: h.msg_type,
        has_route: h.flags.has_route,
        has_hmac: h.flags.has_hmac,
        mcu_origin: h.flags.mcu_origin,
        ttl: h.ttl,
        k: h.k,
        msg_id: h.msg_id,
        event_hash: h.event_hash,
        payload_len: h.payload_len,
        target_group: h.target_group,
        target_hive: h.target_hive,
        route: em.route.map(|r| r.entries[..r.len as usize].to_vec()),
        payload: em.payload.to_vec(),
        hmac_tag: em.hmac_tag,
    })
}

// ── Alloc-based transcoding ─────────────────────────────────────

/// Transcode compact to extended (owned types).
pub fn compact_to_extended(compact: &CompactMessage) -> ExtendedMessage {
    let route = compact.route.as_ref().map(|r| {
        r.entries.iter().map(|&e| (e as u32) << 16).collect()
    });
    let mut hmac_tag = None;
    if let Some(tag) = &compact.hmac_tag {
        let mut full = [0u8; 32];
        full[..8].copy_from_slice(tag);
        hmac_tag = Some(full);
    }
    ExtendedMessage {
        version: compact.version,
        msg_type: compact.msg_type,
        has_route: compact.has_route,
        has_hmac: compact.has_hmac,
        mcu_origin: compact.mcu_origin,
        ttl: compact.ttl,
        k: compact.k,
        msg_id: compact.msg_id as u32,
        event_hash: compact.event_hash,
        payload_len: compact.payload.len() as u32,
        target_group: compact.target,
        target_hive: 0,
        route,
        payload: compact.payload.clone(),
        hmac_tag,
    }
}

/// Transcode extended to compact (owned types).
pub fn extended_to_compact(ext: &ExtendedMessage) -> CompactMessage {
    let route = ext.route.as_ref().map(|r| CompactRoute {
        entries: r.iter().map(|&e| (e >> 16) as u16).collect(),
    });
    let target = if ext.target_group != 0 { ext.target_group } else { ext.target_hive };
    let mut hmac_tag = None;
    if let Some(tag) = &ext.hmac_tag {
        let mut trunc = [0u8; 8];
        trunc.copy_from_slice(&tag[..8]);
        hmac_tag = Some(trunc);
    }
    CompactMessage {
        version: ext.version,
        msg_type: ext.msg_type,
        has_route: ext.has_route,
        has_hmac: ext.has_hmac,
        mcu_origin: ext.mcu_origin,
        ttl: ext.ttl,
        k: ext.k,
        msg_id: ext.msg_id as u16,
        event_hash: ext.event_hash,
        target,
        route,
        payload: ext.payload.clone(),
        hmac_tag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex: alloc::string::String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    // ---- Wire TV1: Minimal EVENT ----

    #[test]
    fn test_wire_tv1_decode() {
        let data = hex_to_bytes("0053A1B2424D3E4C1A2B3C4DA10018EA");
        let msg = decode_compact(&data).unwrap();
        assert_eq!(msg.version, 0);
        assert_eq!(msg.msg_type, MsgType::Event);
        assert!(!msg.has_route);
        assert!(!msg.has_hmac);
        assert!(!msg.mcu_origin);
        assert_eq!(msg.ttl, 5);
        assert_eq!(msg.k, 3);
        assert_eq!(msg.msg_id, 0xA1B2);
        assert_eq!(msg.event_hash, 0x424D3E4C);
        assert_eq!(msg.target, 0x1A2B3C4D);
        assert!(msg.route.is_none());
        assert_eq!(msg.payload, hex_to_bytes("A10018EA"));
        assert!(msg.hmac_tag.is_none());
    }

    #[test]
    fn test_wire_tv1_encode() {
        let msg = CompactMessage {
            version: 0,
            msg_type: MsgType::Event,
            has_route: false,
            has_hmac: false,
            mcu_origin: false,
            ttl: 5,
            k: 3,
            msg_id: 0xA1B2,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
            route: None,
            payload: hex_to_bytes("A10018EA"),
            hmac_tag: None,
        };
        let encoded = encode_compact(&msg);
        assert_eq!(encoded, hex_to_bytes("0053A1B2424D3E4C1A2B3C4DA10018EA"));
    }

    #[test]
    fn test_wire_tv1_roundtrip() {
        let data = hex_to_bytes("0053A1B2424D3E4C1A2B3C4DA10018EA");
        let msg = decode_compact(&data).unwrap();
        let re_encoded = encode_compact(&msg);
        assert_eq!(re_encoded, data);
    }

    // ---- Wire TV2: EVENT with route + HMAC ----

    #[test]
    fn test_wire_tv2_decode() {
        let data = hex_to_bytes("06420042C98950FBDEAD123401CAFEA30018FF01000218801122334455667788");
        let msg = decode_compact(&data).unwrap();
        assert_eq!(msg.version, 0);
        assert_eq!(msg.msg_type, MsgType::Event);
        assert!(msg.has_route);
        assert!(msg.has_hmac);
        assert!(!msg.mcu_origin);
        assert_eq!(msg.ttl, 4);
        assert_eq!(msg.k, 2);
        assert_eq!(msg.msg_id, 0x0042);
        assert_eq!(msg.event_hash, 0xC98950FB);
        assert_eq!(msg.target, 0xDEAD1234);
        let route = msg.route.as_ref().unwrap();
        assert_eq!(route.entries, alloc::vec![0xCAFE]);
        assert_eq!(msg.payload, hex_to_bytes("A30018FF0100021880"));
        assert_eq!(msg.hmac_tag.unwrap(), [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    }

    #[test]
    fn test_wire_tv2_roundtrip() {
        let data = hex_to_bytes("06420042C98950FBDEAD123401CAFEA30018FF01000218801122334455667788");
        let msg = decode_compact(&data).unwrap();
        let re_encoded = encode_compact(&msg);
        assert_eq!(re_encoded, data);
    }

    // ---- Wire TV3: MCU-originated EVENT ----

    #[test]
    fn test_wire_tv3_decode() {
        let data = hex_to_bytes("0121000749B4746500000000A10018BB");
        let msg = decode_compact(&data).unwrap();
        assert!(msg.mcu_origin);
        assert_eq!(msg.ttl, 2);
        assert_eq!(msg.k, 1);
        assert_eq!(msg.event_hash, 0x49B47465);
        assert_eq!(msg.target, 0x00000000);
        assert_eq!(msg.payload, hex_to_bytes("A10018BB"));
    }

    #[test]
    fn test_wire_tv3_roundtrip() {
        let data = hex_to_bytes("0121000749B4746500000000A10018BB");
        let msg = decode_compact(&data).unwrap();
        let re_encoded = encode_compact(&msg);
        assert_eq!(re_encoded, data);
    }

    // ---- Byte encoding vectors ----

    #[test]
    fn test_byte0_vectors() {
        assert_eq!(encode_byte0(0, MsgType::Event, r2_wire::Flags::default()), 0x00);
        assert_eq!(encode_byte0(0, MsgType::Event, r2_wire::Flags { has_route: true, has_hmac: true, mcu_origin: false }), 0x06);
        assert_eq!(encode_byte0(0, MsgType::Event, r2_wire::Flags { has_route: false, has_hmac: false, mcu_origin: true }), 0x01);
    }

    #[test]
    fn test_byte1_vectors() {
        assert_eq!(encode_byte1(5, 3), 0x53);
        assert_eq!(encode_byte1(4, 2), 0x42);
    }

    // ---- Version / type rejection ----

    #[test]
    fn test_version_rejection() {
        let mut data = hex_to_bytes("0053A1B2424D3E4C1A2B3C4DA10018EA");
        data[0] = 0x40;
        assert!(decode_compact(&data).is_err());
    }

    #[test]
    fn test_reserved_type_rejection() {
        let mut data = hex_to_bytes("0053A1B2424D3E4C1A2B3C4DA10018EA");
        data[0] = 0x08;
        assert!(decode_compact(&data).is_err());
    }

    #[test]
    fn test_truncated_message() {
        assert!(decode_compact(&[0x00, 0x53]).is_err());
    }

    #[test]
    fn test_k_split() {
        assert_eq!(k_split(4), (2, 2));
        assert_eq!(k_split(3), (1, 2));
        assert_eq!(k_split(1), (0, 1));
        assert_eq!(k_split(0), (0, 0));
    }

    // ---- FrameHeader ----

    #[test]
    fn test_frame_header() {
        assert_eq!(FrameHeader::Complete.encode(), 0x00);
        assert_eq!(FrameHeader::decode(0x00), FrameHeader::Complete);
        let frag = FrameHeader::Fragment { last: true, sequence: 5 };
        let byte = frag.encode();
        assert_eq!(FrameHeader::decode(byte), frag);
    }

    // ---- JSON test vectors ----

    #[test]
    fn test_json_wire_vectors() {
        let json_str = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../r2-specifications/testing/test-vectors/r2-wire-vectors.json")
        ).expect("Failed to read wire test vectors");

        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let vectors = json["vectors"].as_array().unwrap();
        for v in vectors {
            let hex = v["wire_hex"].as_str().unwrap();
            let data = hex_to_bytes(hex);
            let msg = decode_compact(&data).unwrap();
            let re_encoded = encode_compact(&msg);
            assert_eq!(re_encoded, data, "Roundtrip mismatch for {}", v["id"]);
        }
    }
}
