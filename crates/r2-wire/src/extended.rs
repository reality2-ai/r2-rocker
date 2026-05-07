//! Extended message encode/decode (SPEC.md §4).

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::types::*;

/// Extended fixed header size (SPEC.md §4).
const EXT_HEADER: usize = 22;
/// Extended HMAC tag size (SPEC.md §4).
const EXT_HMAC: usize = 32;

/// Encode an extended message into `buf` (SPEC.md §4).
///
/// Returns the number of bytes written.
pub fn encode_extended(msg: &ExtendedMessage<'_>, buf: &mut [u8]) -> Result<usize, WireError> {
    let route_size = match &msg.route {
        Some(r) => {
            if r.len > 8 {
                return Err(WireError::InvalidRouteLen);
            }
            1 + (r.len as usize) * 4
        }
        None => 0,
    };
    let hmac_size = if msg.hmac_tag.is_some() { EXT_HMAC } else { 0 };
    let total = EXT_HEADER + route_size + msg.payload.len() + hmac_size;
    if buf.len() < total {
        return Err(WireError::BufferTooSmall);
    }

    let h = &msg.header;
    buf[0] = encode_byte0(h.version, h.msg_type, h.flags);
    buf[1] = encode_byte1(h.ttl, h.k);
    buf[2..6].copy_from_slice(&h.msg_id.to_be_bytes());
    buf[6..10].copy_from_slice(&h.event_hash.to_be_bytes());
    buf[10..14].copy_from_slice(&h.payload_len.to_be_bytes());
    buf[14..18].copy_from_slice(&h.target_group.to_be_bytes());
    buf[18..22].copy_from_slice(&h.target_hive.to_be_bytes());

    let mut pos = EXT_HEADER;

    if let Some(r) = &msg.route {
        buf[pos] = r.len;
        pos += 1;
        for i in 0..r.len as usize {
            buf[pos..pos + 4].copy_from_slice(&r.entries[i].to_be_bytes());
            pos += 4;
        }
    }

    buf[pos..pos + msg.payload.len()].copy_from_slice(msg.payload);
    pos += msg.payload.len();

    if let Some(tag) = &msg.hmac_tag {
        buf[pos..pos + EXT_HMAC].copy_from_slice(tag);
        pos += EXT_HMAC;
    }

    Ok(pos)
}

/// Decode an extended message from bytes.
/// Decode an extended message from bytes (SPEC.md §4).
///
/// Payload is borrowed from the input slice (zero-copy).
pub fn decode_extended(data: &[u8]) -> Result<ExtendedMessage<'_>, WireError> {
    if data.len() < EXT_HEADER {
        return Err(WireError::TruncatedMessage);
    }

    let (version, msg_type, flags) = decode_byte0(data[0])?;
    let (ttl, k) = decode_byte1(data[1]);

    let msg_id = u32::from_be_bytes([data[2], data[3], data[4], data[5]]);
    let event_hash = u32::from_be_bytes([data[6], data[7], data[8], data[9]]);
    let payload_len = u32::from_be_bytes([data[10], data[11], data[12], data[13]]);
    let target_group = u32::from_be_bytes([data[14], data[15], data[16], data[17]]);
    let target_hive = u32::from_be_bytes([data[18], data[19], data[20], data[21]]);

    let header = ExtendedHeader {
        version,
        msg_type,
        flags,
        ttl,
        k,
        msg_id,
        event_hash,
        payload_len,
        target_group,
        target_hive,
    };

    let mut pos = EXT_HEADER;

    let route = if flags.has_route {
        if pos >= data.len() {
            return Err(WireError::TruncatedMessage);
        }
        let rlen = data[pos] as usize;
        pos += 1;
        if rlen == 0 || rlen > 8 {
            return Err(WireError::InvalidRouteLen);
        }
        if pos + rlen * 4 > data.len() {
            return Err(WireError::TruncatedMessage);
        }
        let mut rs = ExtendedRouteStack::new();
        rs.len = rlen as u8;
        for i in 0..rlen {
            rs.entries[i] =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
        }
        Some(rs)
    } else {
        None
    };

    let hmac_tag = if flags.has_hmac {
        if data.len() < pos + EXT_HMAC {
            return Err(WireError::TruncatedMessage);
        }
        let mut tag = [0u8; EXT_HMAC];
        tag.copy_from_slice(&data[data.len() - EXT_HMAC..]);
        Some(tag)
    } else {
        None
    };

    let payload_end = if flags.has_hmac {
        data.len() - EXT_HMAC
    } else {
        data.len()
    };
    let payload = &data[pos..payload_end];

    // R2-WIRE §4.3.2: payload_len is authoritative — reject if mismatch.
    if payload.len() as u32 != payload_len {
        return Err(WireError::PayloadLenMismatch);
    }

    Ok(ExtendedMessage {
        header,
        route,
        payload,
        hmac_tag,
    })
}

/// Mutate an extended R2-WIRE frame for forwarding by a relay hive, returning
/// a new owned buffer ready to send. Implements R2-WIRE §8.3 (TTL decrement),
/// §8.4 (K spray-and-wait split), and §9.2 (route stack append).
///
/// This function:
/// 1. Decodes the input frame.
/// 2. Decrements TTL by 1 (§8.3). Returns `WireError::TtlExhausted` if TTL=0.
/// 3. Splits K: forwarded copy gets `floor(K/2)` for normal spray (K=0–14).
///    K=15 (flood) is preserved across hops per §4.2.2.
/// 4. Appends `own_hive_id` to the route stack (§9.2). If the inbound frame
///    has no route stack (R flag clear), starts a new stack with the inbound
///    frame's transport-derived `from_hive_id` as the originator entry,
///    then appends `own_hive_id`. If R was already set, just appends.
///    Returns `WireError::InvalidRouteLen` if appending would exceed 8 hops.
/// 5. Re-encodes into a fresh buffer.
///
/// `from_hive_id` is the canonical hive_id of the immediate previous hop (the
/// transport reported source). It is only used when the inbound frame has no
/// route stack — in which case the relay synthesises the originator entry so
/// downstream relays can dedup correctly.
///
/// HMAC tag is preserved opaquely (§10.7).
#[cfg(feature = "alloc")]
pub fn prepare_relay_extended(
    frame: &[u8],
    own_hive_id: u32,
    from_hive_id: u32,
) -> Result<Vec<u8>, WireError> {
    let msg = decode_extended(frame)?;

    // Step 2: TTL check + decrement (§8.3)
    if msg.header.ttl == 0 {
        return Err(WireError::TtlExhausted);
    }
    let new_ttl = msg.header.ttl - 1;

    // Step 3: K split (§8.4). K=15 = flood mode, preserved across hops.
    let new_k = if msg.header.k == 15 {
        15
    } else {
        msg.header.k / 2
    };

    // Step 4: route stack append (§9.2)
    let mut new_route = match msg.route {
        Some(r) => r,
        None => {
            // No inbound stack — synthesise the originator entry from the
            // transport-reported source. This happens when the originator was
            // a non-conformant sender (e.g. raw test injector) that didn't set
            // the R flag. Conformant originators should always set R.
            let mut r = ExtendedRouteStack::new();
            r.len = 1;
            r.entries[0] = from_hive_id;
            r
        }
    };

    if (new_route.len as usize) >= 8 {
        return Err(WireError::InvalidRouteLen);
    }
    new_route.entries[new_route.len as usize] = own_hive_id;
    new_route.len += 1;

    // Step 5: re-encode with mutated header + route stack
    let mut new_flags = msg.header.flags;
    new_flags.has_route = true; // We now have a route stack regardless of inbound state

    let new_header = ExtendedHeader {
        ttl: new_ttl,
        k: new_k,
        flags: new_flags,
        ..msg.header
    };

    let new_msg = ExtendedMessage {
        header: new_header,
        route: Some(new_route),
        payload: msg.payload,
        hmac_tag: msg.hmac_tag,
    };

    let cap = EXT_HEADER + 1 + (new_route.len as usize) * 4 + msg.payload.len()
        + if msg.hmac_tag.is_some() { EXT_HMAC } else { 0 };
    let mut buf = alloc::vec![0u8; cap];
    let written = encode_extended(&new_msg, &mut buf)?;
    buf.truncate(written);
    Ok(buf)
}
