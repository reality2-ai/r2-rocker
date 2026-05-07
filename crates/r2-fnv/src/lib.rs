//! # r2-fnv
//!
//! Event name hashing for [Reality2](https://github.com/reality2-ai), implementing
//! the R2-FNV specification (see [`SPEC.md`](../SPEC.md) in this crate).
//!
//! Maps human-readable event names to compact 32-bit identifiers using FNV-1a 32-bit.
//! Designed for `no_std` environments (MCUs with 2–8 KB SRAM). Zero allocation,
//! single-pass canonicalisation + hashing.
//!
//! ## Quick Start
//!
//! ```
//! use r2_fnv::{r2_hash, fnv1a_32};
//!
//! // Hash a platform event
//! let hash = r2_hash("#ping").unwrap();
//! assert_eq!(hash, 0x7CB36B0A);
//!
//! // Canonicalisation is automatic: case-insensitive, whitespace-stripped
//! assert_eq!(r2_hash("  Set_Temp ").unwrap(), r2_hash("set_temp").unwrap());
//!
//! // Raw FNV-1a for pre-canonicalised input
//! assert_eq!(fnv1a_32(b"#ping"), 0x7CB36B0A);
//! ```
//!
//! ## Naming Convention
//!
//! - **`#` prefix** = platform-reserved event (`#ping`, `#wifi_req`, `#ota_query`)
//! - **No prefix** = agent-defined event (`read_level`, `set_temp`, `alert`)
//!
//! The `#` is part of the hash input: `hash("#ping") ≠ hash("ping")`.
//!
//! ## Conformance
//!
//! This crate is tested against all normative vectors in SPEC.md §7.
//! Any conformant FNV-1a 32-bit implementation MUST produce identical hashes.

#![no_std]
#![deny(missing_docs)]

/// FNV-1a 32-bit offset basis (SPEC.md §2).
const FNV_OFFSET_BASIS: u32 = 0x811C_9DC5;

/// FNV-1a 32-bit prime (SPEC.md §2).
const FNV_PRIME: u32 = 0x0100_0193;

/// Reserved sentinel: broadcast target / null (SPEC.md §5).
const RESERVED_NO_EVENT: u32 = 0x0000_0000;

/// Reserved sentinel: future use (SPEC.md §5).
const RESERVED_FUTURE: u32 = 0xFFFF_FFFF;

/// Errors from R2-FNV hashing.
///
/// These are non-recoverable: the caller must choose a different event name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The event name was empty (or ASCII-whitespace-only) after canonicalisation.
    EmptyEventName,
    /// The hash collides with a reserved sentinel value (SPEC.md §5).
    /// Contains the offending hash (`0x00000000` or `0xFFFFFFFF`).
    ReservedHash(u32),
    /// The event name contains non-ASCII whitespace (e.g. U+00A0 NO-BREAK SPACE,
    /// U+2003 EM SPACE, U+3000 IDEOGRAPHIC SPACE). Per SPEC.md §2, only ASCII
    /// whitespace is stripped during canonicalisation; Unicode whitespace is
    /// not valid in event names and MUST be rejected.
    InvalidWhitespace,
}

/// Compute raw FNV-1a 32-bit over a byte slice.
///
/// No canonicalisation, no reserved-value check. Use this when the input
/// is already canonicalised (lowercase, no whitespace), or for non-event
/// hashing (e.g., device UUID → route stack entry).
///
/// ```
/// use r2_fnv::fnv1a_32;
/// assert_eq!(fnv1a_32(b""), 0x811C9DC5);  // offset basis
/// assert_eq!(fnv1a_32(b"#ping"), 0x7CB36B0A);
/// ```
#[inline]
pub const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// Hash an event name per R2-FNV SPEC.md §2–§5.
///
/// Performs canonicalisation (lowercase + strip ASCII whitespace), hashes with
/// FNV-1a 32-bit, and checks for reserved sentinel collisions. Single-pass,
/// zero-allocation.
///
/// # Errors
///
/// - [`Error::EmptyEventName`] if the name is empty after canonicalisation.
/// - [`Error::ReservedHash`] if the hash equals `0x00000000` or `0xFFFFFFFF`.
///
/// # Examples
///
/// ```
/// use r2_fnv::r2_hash;
///
/// // Platform event
/// assert_eq!(r2_hash("#ping").unwrap(), 0x7CB36B0A);
///
/// // Agent-defined event
/// assert_eq!(r2_hash("read_level").unwrap(), 0x424D3E4C);
///
/// // Canonicalisation
/// assert_eq!(r2_hash("READ_LEVEL").unwrap(), 0x424D3E4C);
/// assert_eq!(r2_hash("  read_level  ").unwrap(), 0x424D3E4C);
///
/// // Empty name
/// assert!(r2_hash("").is_err());
/// assert!(r2_hash("   ").is_err());
/// ```
pub fn r2_hash(event_name: &str) -> Result<u32, Error> {
    // Reject non-ASCII whitespace per SPEC.md §2 step 4. ASCII whitespace
    // bytes (0x09–0x0D, 0x20) are stripped below; Unicode whitespace
    // (U+00A0, U+2003, U+3000, etc.) is invalid in event names.
    for ch in event_name.chars() {
        if ch.is_whitespace() && !ch.is_ascii() {
            return Err(Error::InvalidWhitespace);
        }
    }

    let mut hash = FNV_OFFSET_BASIS;
    let mut len = 0u32;

    for &b in event_name.as_bytes() {
        // Strip ASCII whitespace (SPEC.md §3 step 2)
        if b == 0x20 || (0x09..=0x0D).contains(&b) {
            continue;
        }
        // Lowercase ASCII uppercase (SPEC.md §3 step 1)
        let b = if b.is_ascii_uppercase() { b + 32 } else { b };
        hash ^= b as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
        len += 1;
    }

    if len == 0 {
        return Err(Error::EmptyEventName);
    }
    if hash == RESERVED_NO_EVENT || hash == RESERVED_FUTURE {
        return Err(Error::ReservedHash(hash));
    }
    Ok(hash)
}

/// Hash a pre-canonicalised byte slice with reserved-value checking.
///
/// The caller MUST provide lowercase, whitespace-stripped, non-empty input.
/// This function does NOT canonicalise — it hashes the bytes as-is and checks
/// for reserved sentinels.
///
/// Use this when canonicalisation has already been done (e.g., compile-time
/// event tables).
///
/// # Errors
///
/// - [`Error::EmptyEventName`] if the slice is empty.
/// - [`Error::ReservedHash`] if the hash equals a reserved sentinel.
///
/// ```
/// use r2_fnv::r2_hash_bytes;
/// assert_eq!(r2_hash_bytes(b"#ping").unwrap(), 0x7CB36B0A);
/// ```
pub fn r2_hash_bytes(canonical: &[u8]) -> Result<u32, Error> {
    if canonical.is_empty() {
        return Err(Error::EmptyEventName);
    }
    let hash = fnv1a_32(canonical);
    if hash == RESERVED_NO_EVENT || hash == RESERVED_FUTURE {
        return Err(Error::ReservedHash(hash));
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC.md §7 — normative conformance vectors (algorithm + platform).
    #[test]
    fn spec_vectors() {
        let vectors: &[(&str, u32)] = &[
            // Algorithm vectors (agent-defined, no prefix)
            ("read_level",    0x424D3E4C),
            ("set_temp",      0x745B28B2),
            ("water_level",   0x49B47465),
            ("set_color",     0xC98950FB),
            ("get_status",    0xA8E6815A),
            ("display_image", 0x56179273),
            ("clear",         0x5C6E1222),
            ("calibrate",     0xBD2F7702),
            ("alert",         0xB7C358F9),
            ("evacuate",      0x79E8481B),
            ("message",       0x24F208E4),
            // Agent-defined variants (no prefix) — R2-FNV §6
            ("ping",          0x165DF089),
            ("pong",          0x7ADB16B7),
            ("join",          0xC922BC79),
            ("leave",         0x5473C0C0),
            ("heartbeat",     0xA79E4417),
            ("capability",    0xB4CF5A27),
            ("ack",           0x3A4A5A02),
            ("subscribe",     0xAF9E4A03),
            ("unsubscribe",   0xF9BEFE96),
            ("broadcast",     0xE2D789D4),
            // Platform-reserved events (# prefix)
            ("#ping",         0x7CB36B0A),
            ("#pong",         0x1130DCC8),
            ("#heartbeat",    0x5D6B02AE),
            ("#capability",   0xF24C0E88),
            ("#join",         0xFAD8225E),
            ("#leave",        0x2FD66589),
            ("#ack",          0xB4A426EF),
            ("#subscribe",    0xFB90EBEE),
            ("#unsubscribe",  0x25174D77),
            ("#broadcast",    0xFAECDE75),
            ("#wifi_req",     0xC85B18A8),
            ("#wifi_offer",   0x01F77656),
            ("#wifi_done",    0x89A70310),
            ("#ota_query",    0x68F41803),
            ("#ota_info",     0x610F43A5),
        ];

        for &(name, expected) in vectors {
            assert_eq!(r2_hash(name).unwrap(), expected, "failed for {name:?}");
        }
    }

    /// SPEC.md §7 — canonicalisation vectors.
    #[test]
    fn canonicalisation() {
        let expected = r2_hash("set_temp").unwrap();
        assert_eq!(r2_hash("Set_Temp").unwrap(), expected);
        assert_eq!(r2_hash("SET_TEMP").unwrap(), expected);
        assert_eq!(r2_hash("  set_temp  ").unwrap(), expected);
        assert_eq!(r2_hash("\tset_temp\n").unwrap(), expected);
        assert_eq!(r2_hash("set _temp").unwrap(), expected);
        assert_eq!(r2_hash("s e t _ t e m p").unwrap(), expected);
    }

    /// SPEC.md §3 — empty and whitespace-only names MUST error.
    #[test]
    fn empty_and_whitespace_only() {
        assert_eq!(r2_hash(""), Err(Error::EmptyEventName));
        assert_eq!(r2_hash("   "), Err(Error::EmptyEventName));
        assert_eq!(r2_hash("\t\n"), Err(Error::EmptyEventName));
    }

    /// SPEC.md §2 step 4 — Unicode whitespace MUST be rejected.
    /// ASCII whitespace is stripped (per §2.1); non-ASCII Unicode whitespace
    /// (U+00A0, U+2003, U+3000, etc.) is not a valid event-name character.
    #[test]
    fn rejects_unicode_whitespace() {
        // Trailing NO-BREAK SPACE (U+00A0)
        assert_eq!(r2_hash("set_temp\u{00A0}"), Err(Error::InvalidWhitespace));
        // Interior EM SPACE (U+2003)
        assert_eq!(r2_hash("set\u{2003}temp"), Err(Error::InvalidWhitespace));
        // Leading IDEOGRAPHIC SPACE (U+3000)
        assert_eq!(r2_hash("\u{3000}set_temp"), Err(Error::InvalidWhitespace));
        // Mixed ASCII and Unicode whitespace — Unicode wins (rejected before strip)
        assert_eq!(r2_hash("  set_temp\u{2003}  "), Err(Error::InvalidWhitespace));
        // Sanity: ASCII-only with whitespace still hashes successfully
        assert!(r2_hash("set temp").is_ok());
    }

    /// Verify no collisions among all spec vectors.
    #[test]
    fn no_collisions_among_spec_vectors() {
        let names = [
            "read_level", "set_temp", "water_level", "set_color",
            "get_status", "display_image", "clear", "calibrate",
            "alert", "evacuate", "message",
            "#ping", "#pong", "#heartbeat", "#capability",
            "#join", "#leave", "#ack", "#subscribe",
            "#unsubscribe", "#broadcast",
            "#wifi_req", "#wifi_offer", "#wifi_done",
            "#ota_query", "#ota_info",
        ];
        let mut hashes = [0u32; 26];
        for (i, name) in names.iter().enumerate() {
            hashes[i] = r2_hash(name).unwrap();
        }
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "{} vs {}", names[i], names[j]);
            }
        }
    }

    /// `r2_hash_bytes` must agree with `r2_hash` for canonical input.
    #[test]
    fn hash_bytes_consistent() {
        assert_eq!(r2_hash_bytes(b"#ping").unwrap(), r2_hash("#ping").unwrap());
        assert_eq!(r2_hash_bytes(b"set_temp").unwrap(), r2_hash("set_temp").unwrap());
    }

    /// Platform prefix survives canonicalisation.
    #[test]
    fn platform_prefix_canonicalisation() {
        let expected = r2_hash("#ping").unwrap();
        assert_eq!(r2_hash("#PING").unwrap(), expected);
        assert_eq!(r2_hash("  #PING  ").unwrap(), expected);
        assert_eq!(r2_hash("# ping").unwrap(), expected);
    }

    /// FNV offset basis is correct for empty input.
    #[test]
    fn offset_basis() {
        assert_eq!(fnv1a_32(b""), 0x811C9DC5);
    }

    // ---- JSON vector conformance tests ----
    // These load from the canonical spec vectors in r2-specifications.
    // Any R2 implementation MUST pass these same vectors.

    extern crate alloc;
    use alloc::format;

    const VECTORS_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../r2-specifications/testing/test-vectors/r2-fnv-vectors.json"
    ));

    fn parse_hex_u32(s: &str) -> u32 {
        u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
            .expect("valid hex u32")
    }

    #[test]
    fn json_conformance_vectors() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON)
            .expect("valid FNV vectors JSON");
        let vectors = data["conformance_vectors"]["vectors"]
            .as_array()
            .expect("conformance_vectors.vectors array");

        assert!(vectors.len() >= 20, "expected ≥20 conformance vectors, got {}", vectors.len());

        for v in vectors {
            let input = v["input"].as_str().expect("input string");
            let expected_hex = v["hash"].as_str().expect("hash string");
            let expected = parse_hex_u32(expected_hex);
            let actual = r2_hash(input).expect(&format!("r2_hash({:?}) should succeed", input));
            assert_eq!(
                actual, expected,
                "FNV conformance FAIL: r2_hash({:?}) = 0x{:08X}, expected {}",
                input, actual, expected_hex
            );
        }
    }

    #[test]
    fn json_canonicalisation_vectors() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON)
            .expect("valid FNV vectors JSON");
        let vectors = data["canonicalisation_vectors"]["vectors"]
            .as_array()
            .expect("canonicalisation_vectors.vectors array");

        for v in vectors {
            let input = v["input"].as_str().expect("input string");
            let expected_hex = v["hash"].as_str().expect("hash string");
            let expected = parse_hex_u32(expected_hex);
            let actual = r2_hash(input).expect(&format!("r2_hash({:?}) should succeed", input));
            assert_eq!(
                actual, expected,
                "FNV canonicalisation FAIL: r2_hash({:?}) = 0x{:08X}, expected {}",
                input, actual, expected_hex
            );
        }
    }

    #[test]
    fn json_error_vectors() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON)
            .expect("valid FNV vectors JSON");
        let vectors = data["error_vectors"]["vectors"]
            .as_array()
            .expect("error_vectors.vectors array");

        for v in vectors {
            let input = v["input"].as_str().expect("input string");
            let error_type = v["error"].as_str().expect("error string");
            let result = r2_hash(input);
            assert!(
                result.is_err(),
                "FNV error vector FAIL: r2_hash({:?}) should fail with {}, but got {:?}",
                input, error_type, result
            );
        }
    }
}
