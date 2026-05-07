//! Tests for r2-wire: test vectors from R2-WIRE §14 + roundtrip tests.

use crate::compact::*;
use crate::extended::*;
use crate::hmac::*;
use crate::transcode::*;
use crate::types::*;

// ── Test Vector 1: Minimal EVENT, no route, no HMAC ──

const TV1: [u8; 16] = [
    0x00, 0x53, 0xA1, 0xB2, 0x42, 0x4D, 0x3E, 0x4C, 0x1A, 0x2B, 0x3C, 0x4D, 0xA1, 0x00, 0x18, 0xEA,
];

#[test]
fn tv1_decode() {
    let msg = decode_compact(&TV1).unwrap();
    assert_eq!(msg.header.version, 0);
    assert_eq!(msg.header.msg_type, MsgType::Event);
    assert_eq!(
        msg.header.flags,
        Flags {
            has_route: false,
            has_hmac: false,
            mcu_origin: false
        }
    );
    assert_eq!(msg.header.ttl, 5);
    assert_eq!(msg.header.k, 3);
    assert_eq!(msg.header.msg_id, 0xA1B2);
    assert_eq!(msg.header.event_hash, 0x424D3E4C);
    assert_eq!(msg.header.target, 0x1A2B3C4D);
    assert!(msg.route.is_none());
    assert!(msg.hmac_tag.is_none());
    assert_eq!(msg.payload, &[0xA1, 0x00, 0x18, 0xEA]);
}

#[test]
fn tv1_encode() {
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags {
                has_route: false,
                has_hmac: false,
                mcu_origin: false,
            },
            ttl: 5,
            k: 3,
            msg_id: 0xA1B2,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA1, 0x00, 0x18, 0xEA],
        hmac_tag: None,
    };
    let mut buf = [0u8; 64];
    let n = encode_compact(&msg, &mut buf).unwrap();
    assert_eq!(n, 16);
    assert_eq!(&buf[..n], &TV1);
}

// ── Test Vector 2: EVENT with route + HMAC ──

const TV2: [u8; 32] = [
    0x06, 0x42, 0x00, 0x42, 0xC9, 0x89, 0x50, 0xFB, 0xDE, 0xAD, 0x12, 0x34, 0x01, 0xCA, 0xFE, 0xA3,
    0x00, 0x18, 0xFF, 0x01, 0x00, 0x02, 0x18, 0x80, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
];

#[test]
fn tv2_decode() {
    let msg = decode_compact(&TV2).unwrap();
    assert_eq!(msg.header.version, 0);
    assert_eq!(msg.header.msg_type, MsgType::Event);
    assert_eq!(
        msg.header.flags,
        Flags {
            has_route: true,
            has_hmac: true,
            mcu_origin: false
        }
    );
    assert_eq!(msg.header.ttl, 4);
    assert_eq!(msg.header.k, 2);
    assert_eq!(msg.header.msg_id, 0x0042);
    assert_eq!(msg.header.event_hash, 0xC98950FB);
    assert_eq!(msg.header.target, 0xDEAD1234);
    let route = msg.route.unwrap();
    assert_eq!(route.len, 1);
    assert_eq!(route.entries[0], 0xCAFE);
    assert_eq!(
        msg.payload,
        &[0xA3, 0x00, 0x18, 0xFF, 0x01, 0x00, 0x02, 0x18, 0x80]
    );
    assert_eq!(
        msg.hmac_tag.unwrap(),
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
    );
}

#[test]
fn tv2_encode() {
    let mut rs = CompactRouteStack::new();
    rs.len = 1;
    rs.entries[0] = 0xCAFE;
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags {
                has_route: true,
                has_hmac: true,
                mcu_origin: false,
            },
            ttl: 4,
            k: 2,
            msg_id: 0x0042,
            event_hash: 0xC98950FB,
            target: 0xDEAD1234,
        },
        route: Some(rs),
        payload: &[0xA3, 0x00, 0x18, 0xFF, 0x01, 0x00, 0x02, 0x18, 0x80],
        hmac_tag: Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]),
    };
    let mut buf = [0u8; 64];
    let n = encode_compact(&msg, &mut buf).unwrap();
    assert_eq!(n, 32);
    assert_eq!(&buf[..n], &TV2);
}

// ── Test Vector 3: MCU-originated EVENT ──

const TV3: [u8; 16] = [
    0x01, 0x21, 0x00, 0x07, 0x49, 0xB4, 0x74, 0x65, 0x00, 0x00, 0x00, 0x00, 0xA1, 0x00, 0x18, 0xBB,
];

#[test]
fn tv3_decode() {
    let msg = decode_compact(&TV3).unwrap();
    assert_eq!(msg.header.version, 0);
    assert_eq!(msg.header.msg_type, MsgType::Event);
    assert_eq!(
        msg.header.flags,
        Flags {
            has_route: false,
            has_hmac: false,
            mcu_origin: true
        }
    );
    assert_eq!(msg.header.ttl, 2);
    assert_eq!(msg.header.k, 1);
    assert_eq!(msg.header.msg_id, 0x0007);
    assert_eq!(msg.header.event_hash, 0x49B47465);
    assert_eq!(msg.header.target, 0x00000000);
    assert_eq!(msg.payload, &[0xA1, 0x00, 0x18, 0xBB]);
}

#[test]
fn tv3_encode() {
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags {
                has_route: false,
                has_hmac: false,
                mcu_origin: true,
            },
            ttl: 2,
            k: 1,
            msg_id: 0x0007,
            event_hash: 0x49B47465,
            target: 0x00000000,
        },
        route: None,
        payload: &[0xA1, 0x00, 0x18, 0xBB],
        hmac_tag: None,
    };
    let mut buf = [0u8; 64];
    let n = encode_compact(&msg, &mut buf).unwrap();
    assert_eq!(n, 16);
    assert_eq!(&buf[..n], &TV3);
}

// ── FNV hash verification ──

#[test]
fn fnv_hash_read_level() {
    assert_eq!(r2_fnv::r2_hash("read_level").unwrap(), 0x424D3E4C);
}

#[test]
fn fnv_hash_set_color() {
    assert_eq!(r2_fnv::r2_hash("set_color").unwrap(), 0xC98950FB);
}

#[test]
fn fnv_hash_water_level() {
    assert_eq!(r2_fnv::r2_hash("water_level").unwrap(), 0x49B47465);
}

// ── Roundtrip tests ──

#[test]
fn compact_roundtrip_no_route() {
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Heartbeat,
            flags: Flags {
                has_route: false,
                has_hmac: false,
                mcu_origin: false,
            },
            ttl: 10,
            k: 0,
            msg_id: 0x1234,
            event_hash: 0x00000000,
            target: 0xABCD0000,
        },
        route: None,
        payload: &[0xA1, 0x01, 0x05],
        hmac_tag: None,
    };
    let mut buf = [0u8; 64];
    let n = encode_compact(&msg, &mut buf).unwrap();
    let dec = decode_compact(&buf[..n]).unwrap();
    assert_eq!(dec.header, msg.header);
    assert_eq!(dec.payload, msg.payload);
    assert!(dec.route.is_none());
}

#[test]
fn compact_roundtrip_with_route_hmac() {
    let mut rs = CompactRouteStack::new();
    rs.len = 2;
    rs.entries[0] = 0x1111;
    rs.entries[1] = 0x2222;
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Reply,
            flags: Flags {
                has_route: true,
                has_hmac: true,
                mcu_origin: false,
            },
            ttl: 3,
            k: 1,
            msg_id: 0xBEEF,
            event_hash: 0x12345678,
            target: 0xDEADBEEF,
        },
        route: Some(rs),
        payload: &[0xA1, 0x00, 0x01],
        hmac_tag: Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11]),
    };
    let mut buf = [0u8; 128];
    let n = encode_compact(&msg, &mut buf).unwrap();
    let dec = decode_compact(&buf[..n]).unwrap();
    assert_eq!(dec.header, msg.header);
    assert_eq!(dec.route.unwrap().len, 2);
    assert_eq!(dec.route.unwrap().entries[0], 0x1111);
    assert_eq!(dec.route.unwrap().entries[1], 0x2222);
    assert_eq!(dec.payload, msg.payload);
    assert_eq!(dec.hmac_tag.unwrap(), msg.hmac_tag.unwrap());
}

#[test]
fn extended_roundtrip() {
    let msg = ExtendedMessage {
        header: ExtendedHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags {
                has_route: false,
                has_hmac: false,
                mcu_origin: false,
            },
            ttl: 5,
            k: 3,
            msg_id: 0x0000A1B2,
            event_hash: 0x424D3E4C,
            payload_len: 4,
            target_group: 0x1A2B3C4D,
            target_hive: 0x00000000,
        },
        route: None,
        payload: &[0xA1, 0x00, 0x18, 0xEA],
        hmac_tag: None,
    };
    let mut buf = [0u8; 128];
    let n = encode_extended(&msg, &mut buf).unwrap();
    let dec = decode_extended(&buf[..n]).unwrap();
    assert_eq!(dec.header, msg.header);
    assert_eq!(dec.payload, msg.payload);
}

// ── Transcode tests ──

#[test]
fn transcode_compact_to_ext_tv1() {
    let mut buf = [0u8; 128];
    let n = transcode_compact_to_extended(&TV1, &mut buf).unwrap();
    let ext = decode_extended(&buf[..n]).unwrap();
    assert_eq!(ext.header.msg_type, MsgType::Event);
    assert_eq!(ext.header.msg_id, 0x0000A1B2);
    assert_eq!(ext.header.event_hash, 0x424D3E4C);
    assert_eq!(ext.header.target_group, 0x1A2B3C4D);
    assert_eq!(ext.header.target_hive, 0x00000000);
    assert_eq!(ext.payload, &[0xA1, 0x00, 0x18, 0xEA]);
}

#[test]
fn transcode_ext_to_compact_roundtrip() {
    // Encode TV1 as extended, then back to compact
    let mut ext_buf = [0u8; 128];
    let n1 = transcode_compact_to_extended(&TV1, &mut ext_buf).unwrap();
    let mut compact_buf = [0u8; 128];
    let n2 = transcode_extended_to_compact(&ext_buf[..n1], &mut compact_buf).unwrap();
    assert_eq!(&compact_buf[..n2], &TV1);
}

// ── Error tests ──

#[test]
fn truncated_message() {
    assert_eq!(
        decode_compact(&[0x00, 0x53]).unwrap_err(),
        WireError::TruncatedMessage
    );
}

#[test]
fn invalid_version() {
    let mut data = TV1;
    data[0] = 0xC0; // version = 3
    assert_eq!(
        decode_compact(&data).unwrap_err(),
        WireError::InvalidVersion
    );
}

#[test]
fn reserved_msg_type() {
    let mut data = TV1;
    data[0] = 0x08; // type = 1 (reserved)
    assert_eq!(
        decode_compact(&data).unwrap_err(),
        WireError::ReservedMsgType
    );
}

#[test]
fn buffer_too_small() {
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5,
            k: 3,
            msg_id: 0,
            event_hash: 0,
            target: 0,
        },
        route: None,
        payload: &[0xA1],
        hmac_tag: None,
    };
    let mut buf = [0u8; 5];
    assert_eq!(
        encode_compact(&msg, &mut buf).unwrap_err(),
        WireError::BufferTooSmall
    );
}

// ── Byte 0/1 helpers ──

#[test]
fn byte0_roundtrip() {
    let b = encode_byte0(
        0,
        MsgType::GroupMgmt,
        Flags {
            has_route: true,
            has_hmac: false,
            mcu_origin: true,
        },
    );
    let (v, t, f) = decode_byte0(b).unwrap();
    assert_eq!(v, 0);
    assert_eq!(t, MsgType::GroupMgmt);
    assert!(f.has_route);
    assert!(!f.has_hmac);
    assert!(f.mcu_origin);
}

#[test]
fn byte1_roundtrip() {
    let b = encode_byte1(15, 14);
    let (ttl, k) = decode_byte1(b);
    assert_eq!(ttl, 15);
    assert_eq!(k, 14);
}

// ── JSON vector conformance tests ────────────────────────────────
// Loaded from r2-specifications canonical vectors.
// Any R2 implementation MUST pass these.

extern crate alloc;
use alloc::vec::Vec;

const WIRE_VECTORS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../r2-specifications/testing/test-vectors/r2-wire-vectors.json"
));

fn parse_hex_u32(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .expect("valid hex u32")
}

fn parse_hex_u16(s: &str) -> u16 {
    u16::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .expect("valid hex u16")
}

fn hex_to_bytes(hex_str: &str) -> Vec<u8> {
    let s = hex_str.replace(' ', "");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn msg_type_from_str(s: &str) -> MsgType {
    match s {
        "EVENT" => MsgType::Event,
        "REPLY" => MsgType::Reply,
        "CAPABILITY" => MsgType::Capability,
        "GROUP_MGMT" => MsgType::GroupMgmt,
        "HEARTBEAT" => MsgType::Heartbeat,
        other => panic!("unknown msg_type: {}", other),
    }
}

#[test]
fn json_wire_conformance_decode() {
    let data: serde_json::Value = serde_json::from_str(WIRE_VECTORS_JSON)
        .expect("valid WIRE vectors JSON");
    let vectors = data["vectors"]
        .as_array()
        .expect("vectors array");

    assert!(vectors.len() >= 3, "expected ≥3 wire vectors, got {}", vectors.len());

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let wire_hex = v["wire_hex"].as_str().expect("wire_hex");
        let fields = &v["fields"];
        let wire_bytes = hex_to_bytes(wire_hex);

        // Decode
        let msg = decode_compact(&wire_bytes)
            .unwrap_or_else(|e| panic!("WIRE conformance {}: decode error: {:?}", id, e));

        // Verify fields
        assert_eq!(
            msg.header.version,
            fields["version"].as_u64().unwrap() as u8,
            "{}: version mismatch", id
        );
        assert_eq!(
            msg.header.msg_type,
            msg_type_from_str(fields["msg_type"].as_str().unwrap()),
            "{}: msg_type mismatch", id
        );
        assert_eq!(
            msg.header.flags.has_route,
            fields["flags"]["has_route"].as_bool().unwrap(),
            "{}: has_route mismatch", id
        );
        assert_eq!(
            msg.header.flags.has_hmac,
            fields["flags"]["has_hmac"].as_bool().unwrap(),
            "{}: has_hmac mismatch", id
        );
        assert_eq!(
            msg.header.flags.mcu_origin,
            fields["flags"]["mcu_origin"].as_bool().unwrap(),
            "{}: mcu_origin mismatch", id
        );
        assert_eq!(
            msg.header.ttl,
            fields["ttl"].as_u64().unwrap() as u8,
            "{}: ttl mismatch", id
        );
        assert_eq!(
            msg.header.k,
            fields["k"].as_u64().unwrap() as u8,
            "{}: k mismatch", id
        );
        assert_eq!(
            msg.header.msg_id,
            parse_hex_u16(fields["msg_id"].as_str().unwrap()),
            "{}: msg_id mismatch", id
        );
        assert_eq!(
            msg.header.event_hash,
            parse_hex_u32(fields["event_hash"].as_str().unwrap()),
            "{}: event_hash mismatch", id
        );
        assert_eq!(
            msg.header.target,
            parse_hex_u32(fields["target"].as_str().unwrap()),
            "{}: target mismatch", id
        );

        // Verify payload
        if let Some(payload_hex) = fields["payload_cbor_hex"].as_str() {
            let expected_payload = hex_to_bytes(payload_hex);
            assert_eq!(
                msg.payload, expected_payload.as_slice(),
                "{}: payload mismatch", id
            );
        }
    }
}

#[test]
fn json_wire_conformance_encode_roundtrip() {
    let data: serde_json::Value = serde_json::from_str(WIRE_VECTORS_JSON)
        .expect("valid WIRE vectors JSON");
    let vectors = data["vectors"]
        .as_array()
        .expect("vectors array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let wire_hex = v["wire_hex"].as_str().expect("wire_hex");
        let wire_bytes = hex_to_bytes(wire_hex);

        // Decode then re-encode — should produce identical bytes
        let msg = decode_compact(&wire_bytes)
            .unwrap_or_else(|e| panic!("WIRE roundtrip {}: decode error: {:?}", id, e));

        let mut buf = [0u8; 512];
        let len = encode_compact(&msg, &mut buf)
            .unwrap_or_else(|e| panic!("WIRE roundtrip {}: encode error: {:?}", id, e));

        assert_eq!(
            &buf[..len], wire_bytes.as_slice(),
            "WIRE roundtrip {}: re-encoded bytes differ", id
        );
    }
}

// ── HMAC envelope tests (R2-WIRE §10) ──────────────────────────────

use crate::hmac::{
    HmacProvider, FrameClass, COMPACT_TAG_LEN, EXTENDED_TAG_LEN,
    authenticated_bytes_compact, sign_compact, verify_compact, classify_compact,
};

/// Test HMAC provider with a fixed key.
struct TestHmac {
    key: [u8; 32],
}

impl TestHmac {
    fn new(key: [u8; 32]) -> Self {
        Self { key }
    }
}

impl HmacProvider for TestHmac {
    fn mac_compact(&self, data: &[u8]) -> [u8; COMPACT_TAG_LEN] {
        // Deterministic keyed hash for testing. Not cryptographic, but
        // produces different output for different inputs and keys.
        let full = self.mac_extended(data);
        let mut tag = [0u8; COMPACT_TAG_LEN];
        tag.copy_from_slice(&full[..COMPACT_TAG_LEN]);
        tag
    }

    fn mac_extended(&self, data: &[u8]) -> [u8; EXTENDED_TAG_LEN] {
        // FNV-1a–style cascading hash seeded with key bytes.
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in &self.key {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        for &b in data {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut tag = [0u8; EXTENDED_TAG_LEN];
        // Fill 32 bytes by cascading the hash state
        for chunk in tag.chunks_mut(8) {
            let bytes = h.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
            h = h.wrapping_mul(0x100000001b3);
        }
        tag
    }
}

#[test]
fn hmac_authenticated_bytes_excludes_mutable_fields() {
    let msg1 = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0xAAAA,
            event_hash: 0x424D3E4C,
            target: 0xDEADBEEF,
        },
        route: None,
        payload: &[0xA1, 0x00, 0x18, 0xEA],
        hmac_tag: None,
    };

    // Same message but different TTL, K, msg_id — mutable fields.
    let msg2 = CompactMessage {
        header: CompactHeader {
            ttl: 2, k: 0,
            msg_id: 0xBBBB,
            ..msg1.header
        },
        ..msg1
    };

    let mut buf1 = [0u8; 256];
    let mut buf2 = [0u8; 256];
    let len1 = authenticated_bytes_compact(&msg1, &mut buf1);
    let len2 = authenticated_bytes_compact(&msg2, &mut buf2);

    assert_eq!(len1, len2);
    assert_eq!(&buf1[..len1], &buf2[..len2],
        "Authenticated bytes must be identical when only mutable fields differ");
}

#[test]
fn hmac_sign_verify_roundtrip() {
    let hmac = TestHmac::new([0x42; 32]);
    let mut msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0xA1B2,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA1, 0x00, 0x18, 0xEA],
        hmac_tag: None,
    };

    // Sign
    let (flags, tag) = sign_compact(&msg, &hmac);
    msg.header.flags = flags;
    msg.hmac_tag = Some(tag);

    assert!(msg.header.flags.has_hmac);

    // Verify with same key — should pass
    assert!(verify_compact(&msg, &hmac));

    // Verify with different key — should fail
    let wrong = TestHmac::new([0xFF; 32]);
    assert!(!verify_compact(&msg, &wrong));
}

#[test]
fn hmac_survives_relay_mutation() {
    let hmac = TestHmac::new([0x42; 32]);
    let mut msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 7, k: 4,
            msg_id: 0x1234,
            event_hash: 0xC98950FB,
            target: 0xDEADBEEF,
        },
        route: None,
        payload: &[0xA2, 0x01, 0x18, 0x2A, 0x02, 0x18, 0x37],
        hmac_tag: None,
    };

    // Sign at origin
    let (flags, tag) = sign_compact(&msg, &hmac);
    msg.header.flags = flags;
    msg.hmac_tag = Some(tag);

    // Relay mutates TTL and K (mutable fields)
    msg.header.ttl = 6;
    msg.header.k = 2;
    msg.header.msg_id = 0x5678; // Relay may also change msg_id

    // Verify at destination — should still pass
    assert!(verify_compact(&msg, &hmac),
        "HMAC must survive relay mutation of TTL, K, and msg_id");
}

#[test]
fn hmac_detects_payload_tampering() {
    let hmac = TestHmac::new([0x42; 32]);
    let payload_good = [0xA1, 0x01, 0x18, 0x2A];
    let payload_bad = [0xA1, 0x01, 0x18, 0x2B]; // changed last byte

    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &payload_good,
        hmac_tag: None,
    };

    let (flags, tag) = sign_compact(&msg, &hmac);

    // Reconstruct with tampered payload but original tag
    let tampered = CompactMessage {
        header: CompactHeader { flags, ..msg.header },
        route: None,
        payload: &payload_bad,
        hmac_tag: Some(tag),
    };

    assert!(!verify_compact(&tampered, &hmac),
        "HMAC must detect payload tampering");
}

#[test]
fn classify_same_group() {
    let hmac = TestHmac::new([0x42; 32]);
    let mut msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA0],
        hmac_tag: None,
    };

    let (flags, tag) = sign_compact(&msg, &hmac);
    msg.header.flags = flags;
    msg.hmac_tag = Some(tag);

    assert_eq!(classify_compact(&msg, Some(&hmac)), Some(FrameClass::SameGroup));
}

#[test]
fn classify_unauthenticated() {
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA0],
        hmac_tag: None,
    };
    let hmac = TestHmac::new([0x42; 32]);
    assert_eq!(classify_compact(&msg, Some(&hmac)), Some(FrameClass::Unauthenticated));
}

#[test]
fn classify_relay_no_key() {
    let hmac = TestHmac::new([0x42; 32]);
    let mut msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 5, k: 3,
            msg_id: 0,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA0],
        hmac_tag: None,
    };
    let (flags, tag) = sign_compact(&msg, &hmac);
    msg.header.flags = flags;
    msg.hmac_tag = Some(tag);

    // Classify without a key — should relay
    let no_key: Option<&TestHmac> = None;
    assert_eq!(classify_compact(&msg, no_key), Some(FrameClass::Relay));
}

#[test]
fn classify_drop_invalid() {
    let hmac = TestHmac::new([0x42; 32]);
    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags { has_hmac: true, ..Flags::default() },
            ttl: 5, k: 3,
            msg_id: 0,
            event_hash: 0x424D3E4C,
            target: 0x1A2B3C4D,
        },
        route: None,
        payload: &[0xA0],
        hmac_tag: Some([0xFF; 8]), // garbage tag
    };

    // Classify with a key but invalid tag — should drop (None)
    assert_eq!(classify_compact(&msg, Some(&hmac)), None);
}

#[test]
#[cfg(feature = "alloc")]
fn hmac_extended_large_payload() {
    // Verify HMAC sign/verify works for payloads > 4KB (up to 65535).
    let hmac = TestHmac::new([0xAB; 32]);
    let large_payload = alloc::vec![0x42u8; 8192]; // 8KB payload

    let msg = ExtendedMessage {
        header: ExtendedHeader {
            version: 0,
            msg_type: MsgType::Event,
            flags: Flags::default(),
            ttl: 7, k: 0,
            msg_id: 0x1234,
            event_hash: 0xDEADBEEF,
            payload_len: large_payload.len() as u32,
            target_group: 0xAAAA0000,
            target_hive: 0xBBBB0000,
        },
        route: None,
        payload: &large_payload,
        hmac_tag: None,
    };

    // Sign
    let (flags, tag) = sign_extended(&msg, &hmac);
    assert!(flags.has_hmac);

    // Verify with correct tag
    let signed_msg = ExtendedMessage {
        header: ExtendedHeader { flags, ..msg.header },
        hmac_tag: Some(tag),
        ..msg
    };
    assert!(verify_extended(&signed_msg, &hmac));

    // Verify with wrong key fails
    let wrong = TestHmac::new([0xFF; 32]);
    assert!(!verify_extended(&signed_msg, &wrong));
}

// ── Transcoding conformance tests (R2-WIRE §5) ────────────────────

#[test]
fn json_transcode_compact_to_extended() {
    let data: serde_json::Value = serde_json::from_str(WIRE_VECTORS_JSON)
        .expect("valid WIRE vectors JSON");
    let vectors = data["transcode_vectors"]
        .as_array()
        .expect("transcode_vectors array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let source_format = v["source_format"].as_str().unwrap_or("");
        if source_format != "compact" {
            continue;
        }
        let source_hex = v["source_wire_hex"].as_str().expect("source_wire_hex");
        let source_bytes = hex_to_bytes(source_hex);

        let mut buf = [0u8; 512];
        let len = transcode_compact_to_extended(&source_bytes, &mut buf)
            .unwrap_or_else(|e| panic!("TC {}: transcode error: {:?}", id, e));

        // Verify expected wire hex if present
        if let Some(expected_hex) = v["expected_wire_hex"].as_str() {
            let expected = hex_to_bytes(expected_hex);
            assert_eq!(
                &buf[..len], expected.as_slice(),
                "TC {}: transcoded bytes differ", id
            );
        }

        // Verify expected length if present
        if let Some(expected_len) = v["expected_wire_length"].as_u64() {
            assert_eq!(
                len, expected_len as usize,
                "TC {}: transcoded length differs", id
            );
        }

        // Verify key fields if present
        if let Some(fields) = v.get("expected_fields") {
            let ext = decode_extended(&buf[..len])
                .unwrap_or_else(|e| panic!("TC {}: decode extended error: {:?}", id, e));

            if let Some(tg) = fields.get("target_group") {
                let expected_tg = u32::from_str_radix(tg.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                assert_eq!(ext.header.target_group, expected_tg, "TC {}: target_group", id);
            }
            if let Some(th) = fields.get("target_hive") {
                let expected_th = u32::from_str_radix(th.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                assert_eq!(ext.header.target_hive, expected_th, "TC {}: target_hive", id);
            }
        }
    }
}

#[test]
fn json_transcode_extended_to_compact() {
    let data: serde_json::Value = serde_json::from_str(WIRE_VECTORS_JSON)
        .expect("valid WIRE vectors JSON");
    let vectors = data["transcode_vectors"]
        .as_array()
        .expect("transcode_vectors array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let source_format = v["source_format"].as_str().unwrap_or("");
        if source_format != "extended" {
            continue;
        }
        let source_hex = v["source_wire_hex"].as_str().expect("source_wire_hex");
        let source_bytes = hex_to_bytes(source_hex);

        let mut buf = [0u8; 512];
        let len = transcode_extended_to_compact(&source_bytes, &mut buf)
            .unwrap_or_else(|e| panic!("TC {}: transcode error: {:?}", id, e));

        // Verify expected wire hex if present
        if let Some(expected_hex) = v["expected_wire_hex"].as_str() {
            let expected = hex_to_bytes(expected_hex);
            assert_eq!(
                &buf[..len], expected.as_slice(),
                "TC {}: transcoded bytes differ", id
            );
        }

        // Verify expected length if present
        if let Some(expected_len) = v["expected_wire_length"].as_u64() {
            assert_eq!(
                len, expected_len as usize,
                "TC {}: transcoded length differs", id
            );
        }

        // Verify target field for extended→compact
        if let Some(fields) = v.get("expected_fields") {
            let cm = decode_compact(&buf[..len])
                .unwrap_or_else(|e| panic!("TC {}: decode compact error: {:?}", id, e));

            if let Some(target) = fields.get("target") {
                let expected_t = u32::from_str_radix(target.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                assert_eq!(cm.header.target, expected_t, "TC {}: target", id);
            }
        }
    }
}
