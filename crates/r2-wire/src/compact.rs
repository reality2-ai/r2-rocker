//! Compact message encode/decode (SPEC.md §3).

use crate::types::*;

/// Compact fixed header size (SPEC.md §3).
const COMPACT_HEADER: usize = 12;
/// Compact HMAC tag size (SPEC.md §3).
const COMPACT_HMAC: usize = 8;

/// Encode a compact message into `buf` (SPEC.md §3).
///
/// Returns the number of bytes written. The caller must provide a buffer
/// large enough for `12 + route_bytes + payload.len() + hmac_bytes`.
pub fn encode_compact(msg: &CompactMessage<'_>, buf: &mut [u8]) -> Result<usize, WireError> {
    let route_size = match &msg.route {
        Some(r) => {
            if r.len > 8 {
                return Err(WireError::InvalidRouteLen);
            }
            1 + (r.len as usize) * 2
        }
        None => 0,
    };
    let hmac_size = if msg.hmac_tag.is_some() {
        COMPACT_HMAC
    } else {
        0
    };
    let total = COMPACT_HEADER + route_size + msg.payload.len() + hmac_size;
    if buf.len() < total {
        return Err(WireError::BufferTooSmall);
    }

    let h = &msg.header;
    buf[0] = encode_byte0(h.version, h.msg_type, h.flags);
    buf[1] = encode_byte1(h.ttl, h.k);
    buf[2..4].copy_from_slice(&h.msg_id.to_be_bytes());
    buf[4..8].copy_from_slice(&h.event_hash.to_be_bytes());
    buf[8..12].copy_from_slice(&h.target.to_be_bytes());

    let mut pos = COMPACT_HEADER;

    if let Some(r) = &msg.route {
        buf[pos] = r.len;
        pos += 1;
        for i in 0..r.len as usize {
            buf[pos..pos + 2].copy_from_slice(&r.entries[i].to_be_bytes());
            pos += 2;
        }
    }

    buf[pos..pos + msg.payload.len()].copy_from_slice(msg.payload);
    pos += msg.payload.len();

    if let Some(tag) = &msg.hmac_tag {
        buf[pos..pos + COMPACT_HMAC].copy_from_slice(tag);
        pos += COMPACT_HMAC;
    }

    Ok(pos)
}

/// Decode a compact message from bytes (SPEC.md §3).
///
/// The input must include the complete message (header + route + payload + HMAC).
/// Payload is borrowed from the input slice (zero-copy).
pub fn decode_compact(data: &[u8]) -> Result<CompactMessage<'_>, WireError> {
    if data.len() < COMPACT_HEADER {
        return Err(WireError::TruncatedMessage);
    }

    let (version, msg_type, flags) = decode_byte0(data[0])?;
    let (ttl, k) = decode_byte1(data[1]);

    let msg_id = u16::from_be_bytes([data[2], data[3]]);
    let event_hash = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let target = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

    let header = CompactHeader {
        version,
        msg_type,
        flags,
        ttl,
        k,
        msg_id,
        event_hash,
        target,
    };

    let mut pos = COMPACT_HEADER;

    let route = if flags.has_route {
        if pos >= data.len() {
            return Err(WireError::TruncatedMessage);
        }
        let rlen = data[pos] as usize;
        pos += 1;
        if rlen == 0 || rlen > 8 {
            return Err(WireError::InvalidRouteLen);
        }
        if pos + rlen * 2 > data.len() {
            return Err(WireError::TruncatedMessage);
        }
        let mut rs = CompactRouteStack::new();
        rs.len = rlen as u8;
        for i in 0..rlen {
            rs.entries[i] = u16::from_be_bytes([data[pos], data[pos + 1]]);
            pos += 2;
        }
        Some(rs)
    } else {
        None
    };

    let hmac_tag = if flags.has_hmac {
        if data.len() < pos + COMPACT_HMAC {
            return Err(WireError::TruncatedMessage);
        }
        let mut tag = [0u8; COMPACT_HMAC];
        tag.copy_from_slice(&data[data.len() - COMPACT_HMAC..]);
        Some(tag)
    } else {
        None
    };

    let payload_end = if flags.has_hmac {
        data.len() - COMPACT_HMAC
    } else {
        data.len()
    };
    let payload = &data[pos..payload_end];

    Ok(CompactMessage {
        header,
        route,
        payload,
        hmac_tag,
    })
}
