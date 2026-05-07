//! HMAC envelope for authenticated messaging (R2-WIRE §10, R2-TRUST §6).
//!
//! The wire protocol authenticates only **immutable** fields — TTL, K,
//! msg_id, and route_stack are mutable (relay nodes change them) and
//! explicitly excluded from the HMAC input.
//!
//! ## Authenticated bytes
//!
//! **Compact:** `type(1) || event_hash(4) || target(4) || payload(N)`
//!
//! **Extended:** `type(1) || event_hash(4) || target_group(4) || target_hive(4) || payload(N)`
//!
//! ## Tag sizes
//!
//! - Compact: 8 bytes (truncated HMAC-SHA256)
//! - Extended: 32 bytes (full HMAC-SHA256)
//!
//! ## Usage
//!
//! The [`HmacProvider`] trait is crypto-agnostic — `r2-wire` defines *what*
//! to authenticate, the caller supplies *how*. `r2-trust` provides the
//! concrete implementation using HKDF-derived keys.

use crate::types::{CompactMessage, ExtendedMessage, Flags};

/// Compact HMAC tag size — truncated to first 8 bytes of HMAC-SHA256.
pub const COMPACT_TAG_LEN: usize = 8;
/// Extended HMAC tag size — full 32-byte HMAC-SHA256 output.
pub const EXTENDED_TAG_LEN: usize = 32;

/// Maximum authenticated-bytes buffer for compact messages.
///
/// 1 (type) + 4 (event_hash) + 4 (target) + 180 (max CBOR compact) = 189.
const COMPACT_AUTH_MAX: usize = 1 + 4 + 4 + 180;

/// Crypto-agnostic HMAC provider (R2-WIRE §10.3).
///
/// Implementors compute HMAC-SHA256 over the authenticated bytes and return
/// the tag. The trait has no dependencies on any crypto crate — `r2-wire`
/// only defines the interface.
///
/// # Constant-time requirement
///
/// [`verify_compact`] and [`verify_extended`] use the provider's output
/// and perform constant-time comparison. Implementations SHOULD also use
/// constant-time MAC finalization internally.
pub trait HmacProvider {
    /// Compute truncated 8-byte HMAC tag for compact frames.
    fn mac_compact(&self, authenticated_bytes: &[u8]) -> [u8; COMPACT_TAG_LEN];

    /// Compute full 32-byte HMAC tag for extended frames.
    fn mac_extended(&self, authenticated_bytes: &[u8]) -> [u8; EXTENDED_TAG_LEN];
}

// ---------------------------------------------------------------------------
// Authenticated bytes extraction
// ---------------------------------------------------------------------------

/// Build the authenticated byte sequence for a compact message (R2-WIRE §10.2).
///
/// Returns the number of bytes written into `buf`.
///
/// Layout: `type(1) || event_hash(4) || target(4) || payload(N)`
pub fn authenticated_bytes_compact(msg: &CompactMessage<'_>, buf: &mut [u8]) -> usize {
    let payload_len = msg.payload.len();
    let total = 1 + 4 + 4 + payload_len;
    debug_assert!(buf.len() >= total);

    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target.to_be_bytes());
    buf[9..9 + payload_len].copy_from_slice(msg.payload);
    total
}

/// Build the authenticated byte sequence for an extended message (R2-WIRE §10.2).
///
/// Returns the number of bytes written into `buf`.
///
/// Layout: `type(1) || event_hash(4) || target_group(4) || target_hive(4) || payload(N)`
pub fn authenticated_bytes_extended(msg: &ExtendedMessage<'_>, buf: &mut [u8]) -> usize {
    let payload_len = msg.payload.len();
    let total = 1 + 4 + 4 + 4 + payload_len;
    debug_assert!(buf.len() >= total);

    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[13..13 + payload_len].copy_from_slice(msg.payload);
    total
}

// ---------------------------------------------------------------------------
// Sign (apply HMAC tag to a message)
// ---------------------------------------------------------------------------

/// Compute and attach the HMAC tag to a compact message.
///
/// Returns a new `Flags` with `has_hmac = true` and the 8-byte tag.
/// The caller should set `msg.header.flags = flags` and `msg.hmac_tag = Some(tag)`
/// before encoding, or use the returned pair directly.
pub fn sign_compact(
    msg: &CompactMessage<'_>,
    hmac: &impl HmacProvider,
) -> (Flags, [u8; COMPACT_TAG_LEN]) {
    let mut auth_buf = [0u8; COMPACT_AUTH_MAX];
    let len = authenticated_bytes_compact(msg, &mut auth_buf);
    let tag = hmac.mac_compact(&auth_buf[..len]);
    let flags = Flags {
        has_hmac: true,
        ..msg.header.flags
    };
    (flags, tag)
}

/// Compute and attach the HMAC tag to an extended message.
///
/// Returns a new `Flags` with `has_hmac = true` and the 32-byte tag.
///
/// With the `alloc` feature, supports payloads up to 65,535 bytes (full
/// R2-WIRE spec range). Without `alloc`, limited to 4KB payloads via a
/// stack buffer (sufficient for no_std environments that only use compact
/// frames for large transfers).
pub fn sign_extended(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
) -> (Flags, [u8; EXTENDED_TAG_LEN]) {
    let payload_len = msg.payload.len();
    let total = 13 + payload_len;

    let tag = sign_extended_inner(msg, hmac, total);
    let flags = Flags {
        has_hmac: true,
        ..msg.header.flags
    };
    (flags, tag)
}

#[cfg(feature = "alloc")]
fn sign_extended_inner(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
    total: usize,
) -> [u8; EXTENDED_TAG_LEN] {
    let mut buf = alloc::vec![0u8; total];
    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[13..total].copy_from_slice(msg.payload);
    hmac.mac_extended(&buf[..total])
}

#[cfg(not(feature = "alloc"))]
fn sign_extended_inner(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
    total: usize,
) -> [u8; EXTENDED_TAG_LEN] {
    // no_std fallback: stack-allocate up to 4KB.
    const EXT_AUTH_MAX: usize = 13 + 4096;
    debug_assert!(total <= EXT_AUTH_MAX, "extended payload too large for stack HMAC; enable alloc feature");
    let mut buf = [0u8; EXT_AUTH_MAX];
    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[13..total].copy_from_slice(msg.payload);
    hmac.mac_extended(&buf[..total])
}

// ---------------------------------------------------------------------------
// Verify (check HMAC tag on a received message)
// ---------------------------------------------------------------------------

/// Verify the HMAC tag on a compact message.
///
/// Returns `true` if the tag matches (constant-time comparison).
/// Returns `false` if no tag is present or the tag doesn't match.
pub fn verify_compact(msg: &CompactMessage<'_>, hmac: &impl HmacProvider) -> bool {
    let received_tag = match msg.hmac_tag {
        Some(tag) => tag,
        None => return false,
    };

    let mut auth_buf = [0u8; COMPACT_AUTH_MAX];
    let len = authenticated_bytes_compact(msg, &mut auth_buf);
    let expected = hmac.mac_compact(&auth_buf[..len]);

    constant_time_eq(&received_tag, &expected)
}

/// Verify the HMAC tag on an extended message.
///
/// Returns `true` if the tag matches (constant-time comparison).
/// Returns `false` if no tag is present or the tag doesn't match.
pub fn verify_extended(msg: &ExtendedMessage<'_>, hmac: &impl HmacProvider) -> bool {
    let received_tag = match msg.hmac_tag {
        Some(tag) => tag,
        None => return false,
    };

    let payload_len = msg.payload.len();
    let total = 13 + payload_len;
    let expected = verify_extended_inner(msg, hmac, total);

    match expected {
        Some(tag) => constant_time_eq(&received_tag, &tag),
        None => false,
    }
}

#[cfg(feature = "alloc")]
fn verify_extended_inner(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
    total: usize,
) -> Option<[u8; EXTENDED_TAG_LEN]> {
    let mut buf = alloc::vec![0u8; total];
    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[13..total].copy_from_slice(msg.payload);
    Some(hmac.mac_extended(&buf[..total]))
}

#[cfg(not(feature = "alloc"))]
fn verify_extended_inner(
    msg: &ExtendedMessage<'_>,
    hmac: &impl HmacProvider,
    total: usize,
) -> Option<[u8; EXTENDED_TAG_LEN]> {
    const EXT_AUTH_MAX: usize = 13 + 4096;
    if total > EXT_AUTH_MAX {
        return None; // Too large for stack verification
    }
    let mut buf = [0u8; EXT_AUTH_MAX];
    buf[0] = msg.header.msg_type as u8;
    buf[1..5].copy_from_slice(&msg.header.event_hash.to_be_bytes());
    buf[5..9].copy_from_slice(&msg.header.target_group.to_be_bytes());
    buf[9..13].copy_from_slice(&msg.header.target_hive.to_be_bytes());
    buf[13..total].copy_from_slice(msg.payload);
    Some(hmac.mac_extended(&buf[..total]))
}

// ---------------------------------------------------------------------------
// Frame classification (R2-TRUST §6.3)
// ---------------------------------------------------------------------------

/// Inbound frame classification (R2-TRUST §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// HMAC verified with trust group key — same trust group.
    SameGroup,
    /// HMAC present but no matching key — relay opaquely.
    Relay,
    /// No HMAC tag (H flag = 0) — unauthenticated.
    Unauthenticated,
}

/// Classify an inbound compact frame (R2-TRUST §6.3).
///
/// - `group_hmac`: the trust group's HMAC provider (if this device is a member).
///
/// Returns `None` if the HMAC is present but **invalid** (frame MUST be dropped).
pub fn classify_compact(
    msg: &CompactMessage<'_>,
    group_hmac: Option<&impl HmacProvider>,
) -> Option<FrameClass> {
    if msg.hmac_tag.is_none() {
        return Some(FrameClass::Unauthenticated);
    }

    // HMAC is present. Try to verify.
    match group_hmac {
        Some(hmac) => {
            if verify_compact(msg, hmac) {
                Some(FrameClass::SameGroup)
            } else {
                None // Invalid HMAC — drop frame
            }
        }
        None => {
            // We have no key for this group — forward opaquely.
            Some(FrameClass::Relay)
        }
    }
}

/// Classify an inbound extended frame (R2-TRUST §6.3).
///
/// Same semantics as [`classify_compact`].
pub fn classify_extended(
    msg: &ExtendedMessage<'_>,
    group_hmac: Option<&impl HmacProvider>,
) -> Option<FrameClass> {
    if msg.hmac_tag.is_none() {
        return Some(FrameClass::Unauthenticated);
    }

    match group_hmac {
        Some(hmac) => {
            if verify_extended(msg, hmac) {
                Some(FrameClass::SameGroup)
            } else {
                None
            }
        }
        None => Some(FrameClass::Relay),
    }
}

// ---------------------------------------------------------------------------
// Constant-time comparison (R2-WIRE §10.6 step 3)
// ---------------------------------------------------------------------------

/// Constant-time byte slice equality (no early exit on mismatch).
#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
