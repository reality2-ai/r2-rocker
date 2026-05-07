//! Compact ↔ Extended transcoding (SPEC.md §5).
//!
//! Gateway devices transcode between compact (BLE/LoRa) and extended (TCP/WiFi).
//! Route entries are zero-extended or truncated. HMAC transcoding is lossy.

use crate::compact::{decode_compact, encode_compact};
use crate::extended::{decode_extended, encode_extended};
use crate::types::*;

/// Transcode compact bytes → extended bytes in `buf` (SPEC.md §5).
///
/// Route entries are zero-extended (`entry << 16`). HMAC tags are zero-padded
/// to 32 bytes (lossy — will not verify on the extended side).
pub fn transcode_compact_to_extended(compact: &[u8], buf: &mut [u8]) -> Result<usize, WireError> {
    let cm = decode_compact(compact)?;
    let h = &cm.header;

    let ext_route = cm.route.map(|r| {
        let mut er = ExtendedRouteStack::new();
        er.len = r.len;
        for i in 0..r.len as usize {
            // zero-extend 2-byte to 4-byte (upper 16 bits)
            er.entries[i] = (r.entries[i] as u32) << 16;
        }
        er
    });

    let ext_flags = Flags {
        has_route: h.flags.has_route,
        has_hmac: h.flags.has_hmac,
        mcu_origin: h.flags.mcu_origin,
    };

    let ext_hmac = cm.hmac_tag.map(|t| {
        let mut full = [0u8; 32];
        full[..8].copy_from_slice(&t);
        full
    });

    let ext = ExtendedMessage {
        header: ExtendedHeader {
            version: h.version,
            msg_type: h.msg_type,
            flags: ext_flags,
            ttl: h.ttl,
            k: h.k,
            msg_id: h.msg_id as u32,
            event_hash: h.event_hash,
            payload_len: cm.payload.len() as u32,
            target_group: h.target,
            target_hive: 0x00000000,
        },
        route: ext_route,
        payload: cm.payload,
        hmac_tag: ext_hmac,
    };

    encode_extended(&ext, buf)
}

/// Transcode extended bytes → compact bytes in `buf` (SPEC.md §5).
///
/// Route entries are truncated to upper 16 bits. Target uses `target_group`
/// if nonzero, else `target_hive`. HMAC tags are truncated to first 8 bytes.
pub fn transcode_extended_to_compact(extended: &[u8], buf: &mut [u8]) -> Result<usize, WireError> {
    let em = decode_extended(extended)?;
    let h = &em.header;

    let compact_route = em.route.map(|r| {
        let mut cr = CompactRouteStack::new();
        cr.len = r.len;
        for i in 0..r.len as usize {
            // truncate to upper 16 bits
            cr.entries[i] = (r.entries[i] >> 16) as u16;
        }
        cr
    });

    let target = if h.target_group != 0 {
        h.target_group
    } else {
        h.target_hive
    };

    let compact_hmac = em.hmac_tag.map(|t| {
        let mut trunc = [0u8; 8];
        trunc.copy_from_slice(&t[..8]);
        trunc
    });

    let cm = CompactMessage {
        header: CompactHeader {
            version: h.version,
            msg_type: h.msg_type,
            flags: Flags {
                has_route: h.flags.has_route,
                has_hmac: h.flags.has_hmac,
                mcu_origin: h.flags.mcu_origin,
            },
            ttl: h.ttl,
            k: h.k,
            msg_id: h.msg_id as u16,
            event_hash: h.event_hash,
            target,
        },
        route: compact_route,
        payload: em.payload,
        hmac_tag: compact_hmac,
    };

    encode_compact(&cm, buf)
}
