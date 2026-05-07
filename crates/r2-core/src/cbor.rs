//! R2-CBOR: Alloc-based CBOR encoding with tree types.
//!
//! Thin wrapper over the `r2-cbor` crate, providing a `CborValue` enum tree
//! for convenient construction and pattern matching on platforms with a heap.
//! Encoding and decoding delegate to the crate's `Encoder`/`Decoder`.
//!
//! For `no_std` / fixed-buffer encoding, depend on `r2-cbor` directly.

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};

/// CBOR value types supported by R2-CBOR profile.
#[derive(Debug, Clone, PartialEq)]
pub enum CborValue {
    /// Unsigned integer (major type 0).
    UInt(u64),
    /// Negative integer (major type 1) — stores the actual value (e.g., -7).
    NegInt(i64),
    /// Byte string (major type 2).
    Bytes(Vec<u8>),
    /// UTF-8 text string (major type 3).
    Text(String),
    /// Array of values (major type 4).
    Array(Vec<CborValue>),
    /// Map of key-value pairs (major type 5).
    Map(Vec<(CborValue, CborValue)>),
    /// Boolean.
    Bool(bool),
    /// Null.
    Null,
    /// Raw IEEE 754 half-precision bits.
    Float16Raw(u16),
    /// IEEE 754 float32.
    Float32(f32),
    /// IEEE 754 float64.
    Float64(f64),
}

/// Encoding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CborMode {
    /// Integer keys only, definite length, ≤180 bytes.
    Compact,
    /// String or integer keys, ≤65535 bytes.
    Standard,
}

/// CBOR encoding/decoding errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborError {
    /// Input is truncated.
    Truncated,
    /// Invalid CBOR type byte.
    InvalidType(u8),
    /// Disallowed type for this mode.
    DisallowedType(&'static str),
    /// Indefinite-length item in compact mode.
    IndefiniteLength,
    /// String key in compact mode.
    StringKeyInCompact,
    /// CBOR tag encountered (not allowed).
    TagNotAllowed,
    /// Undefined simple value (not allowed).
    UndefinedNotAllowed,
    /// Payload exceeds maximum.
    PayloadTooLarge {
        /// Maximum allowed.
        max: usize,
        /// Actual size.
        actual: usize,
    },
    /// Invalid UTF-8 in text string.
    InvalidUtf8,
}

// ── Encoder: CborValue → bytes via r2-cbor crate ────────────────

/// Encode a `CborValue` tree to CBOR bytes.
///
/// Delegates to `r2_cbor::Encoder` for the actual byte encoding.
pub fn encode(value: &CborValue) -> Vec<u8> {
    // Calculate a generous upper bound for buffer size
    let est = estimate_size(value);
    let mut buf = alloc::vec![0u8; est];
    let mut enc = r2_cbor::Encoder::new(&mut buf);
    encode_value(&mut enc, value);
    let len = enc.len();
    buf.truncate(len);
    buf
}

fn estimate_size(value: &CborValue) -> usize {
    match value {
        CborValue::UInt(_) | CborValue::NegInt(_) => 9,
        CborValue::Bool(_) | CborValue::Null => 1,
        CborValue::Float16Raw(_) => 3,
        CborValue::Float32(_) => 5,
        CborValue::Float64(_) => 9,
        CborValue::Bytes(b) => 5 + b.len(),
        CborValue::Text(s) => 5 + s.len(),
        CborValue::Array(items) => 5 + items.iter().map(estimate_size).sum::<usize>(),
        CborValue::Map(pairs) => {
            5 + pairs
                .iter()
                .map(|(k, v)| estimate_size(k) + estimate_size(v))
                .sum::<usize>()
        }
    }
}

fn encode_value(enc: &mut r2_cbor::Encoder<'_>, value: &CborValue) {
    match value {
        CborValue::UInt(n) => { let _ = enc.value(&r2_cbor::Value::UInt(*n)); }
        CborValue::NegInt(n) => { let _ = enc.value(&r2_cbor::Value::NegInt(*n)); }
        CborValue::Bool(b) => { let _ = enc.value(&r2_cbor::Value::Bool(*b)); }
        CborValue::Null => { let _ = enc.value(&r2_cbor::Value::Null); }
        CborValue::Float16Raw(bits) => { let _ = enc.value(&r2_cbor::Value::Float16Raw(*bits)); }
        CborValue::Float32(f) => { let _ = enc.value(&r2_cbor::Value::Float32(*f)); }
        CborValue::Float64(f) => { let _ = enc.value(&r2_cbor::Value::Float64(*f)); }
        CborValue::Bytes(b) => { let _ = enc.value(&r2_cbor::Value::Bytes(b)); }
        CborValue::Text(s) => { let _ = enc.value(&r2_cbor::Value::Text(s)); }
        CborValue::Array(items) => {
            let _ = enc.array(items.len());
            for item in items {
                encode_value(enc, item);
            }
        }
        CborValue::Map(pairs) => {
            let _ = enc.map(pairs.len());
            for (k, v) in pairs {
                encode_value(enc, k);
                encode_value(enc, v);
            }
        }
    }
}

// ── Decoder: bytes → CborValue tree via r2-cbor crate ───────────

/// Decode CBOR bytes into a `CborValue` tree (Compact mode — integer keys only).
pub fn decode(data: &[u8]) -> Result<CborValue, CborError> {
    let mut dec = r2_cbor::Decoder::new(data);
    decode_item(&mut dec)
}

/// Decode CBOR bytes into a `CborValue` tree (Standard mode — string keys allowed).
pub fn decode_standard(data: &[u8]) -> Result<CborValue, CborError> {
    let mut dec = r2_cbor::Decoder::new_with_mode(data, r2_cbor::Mode::Standard);
    decode_item(&mut dec)
}

/// Decode CBOR bytes, returning the value and byte count consumed.
pub fn decode_with_pos(data: &[u8]) -> Result<(CborValue, usize), CborError> {
    let mut dec = r2_cbor::Decoder::new(data);
    let value = decode_item(&mut dec)?;
    Ok((value, dec.position()))
}

fn decode_item(dec: &mut r2_cbor::Decoder<'_>) -> Result<CborValue, CborError> {
    let item = dec.next().map_err(map_decode_error)?;
    match item {
        r2_cbor::Item::UInt(n) => Ok(CborValue::UInt(n)),
        r2_cbor::Item::NegInt(n) => Ok(CborValue::NegInt(n)),
        r2_cbor::Item::Bool(b) => Ok(CborValue::Bool(b)),
        r2_cbor::Item::Null => Ok(CborValue::Null),
        r2_cbor::Item::Float16Raw(bits) => Ok(CborValue::Float16Raw(bits)),
        r2_cbor::Item::Float32(f) => Ok(CborValue::Float32(f)),
        r2_cbor::Item::Float64(f) => Ok(CborValue::Float64(f)),
        r2_cbor::Item::Text(bytes) => {
            let s = core::str::from_utf8(bytes).map_err(|_| CborError::InvalidUtf8)?;
            Ok(CborValue::Text(s.into()))
        }
        r2_cbor::Item::Bytes(bytes) => Ok(CborValue::Bytes(bytes.to_vec())),
        r2_cbor::Item::Array(count) => {
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(decode_item(dec)?);
            }
            Ok(CborValue::Array(items))
        }
        r2_cbor::Item::Map(count) => {
            let mut pairs = Vec::with_capacity(count);
            for _ in 0..count {
                let k = decode_item(dec)?;
                let v = decode_item(dec)?;
                pairs.push((k, v));
            }
            Ok(CborValue::Map(pairs))
        }
    }
}

fn map_decode_error(e: r2_cbor::Error) -> CborError {
    match e {
        r2_cbor::Error::Truncated => CborError::Truncated,
        r2_cbor::Error::DisallowedType => CborError::UndefinedNotAllowed,
        r2_cbor::Error::IndefiniteLength => CborError::IndefiniteLength,
        r2_cbor::Error::BufferFull => CborError::Truncated, // shouldn't happen in decode
        r2_cbor::Error::StringKeyInCompactMode => CborError::StringKeyInCompact,
    }
}

// ── Compact mode validation ─────────────────────────────────────

/// Validate CBOR payload against R2-CBOR compact mode rules.
pub fn validate_compact(data: &[u8]) -> Result<(), CborError> {
    if data.len() > r2_cbor::COMPACT_MAX {
        return Err(CborError::PayloadTooLarge {
            max: r2_cbor::COMPACT_MAX,
            actual: data.len(),
        });
    }
    validate_compact_item(data, &mut 0)
}

fn validate_compact_item(data: &[u8], pos: &mut usize) -> Result<(), CborError> {
    if *pos >= data.len() {
        return Err(CborError::Truncated);
    }

    let initial = data[*pos];
    let major = initial >> 5;
    let ai = initial & 0x1F;
    *pos += 1;

    match major {
        0 | 1 => {
            skip_argument(data, pos, ai)?;
        }
        2 | 3 => {
            if ai == 31 {
                return Err(CborError::IndefiniteLength);
            }
            let len = decode_argument(data, pos, ai)? as usize;
            if *pos + len > data.len() {
                return Err(CborError::Truncated);
            }
            *pos += len;
        }
        4 => {
            if ai == 31 {
                return Err(CborError::IndefiniteLength);
            }
            let count = decode_argument(data, pos, ai)? as usize;
            for _ in 0..count {
                validate_compact_item(data, pos)?;
            }
        }
        5 => {
            if ai == 31 {
                return Err(CborError::IndefiniteLength);
            }
            let count = decode_argument(data, pos, ai)? as usize;
            for _ in 0..count {
                if *pos >= data.len() {
                    return Err(CborError::Truncated);
                }
                let key_major = data[*pos] >> 5;
                if key_major == 3 {
                    return Err(CborError::StringKeyInCompact);
                }
                validate_compact_item(data, pos)?;
                validate_compact_item(data, pos)?;
            }
        }
        6 => return Err(CborError::TagNotAllowed),
        7 => match ai {
            20 | 21 | 22 => {}
            23 => return Err(CborError::UndefinedNotAllowed),
            25 => {
                if *pos + 2 > data.len() {
                    return Err(CborError::Truncated);
                }
                *pos += 2;
            }
            26 => {
                if *pos + 4 > data.len() {
                    return Err(CborError::Truncated);
                }
                *pos += 4;
            }
            27 => {
                if *pos + 8 > data.len() {
                    return Err(CborError::Truncated);
                }
                *pos += 8;
            }
            31 => return Err(CborError::IndefiniteLength),
            _ => return Err(CborError::InvalidType(initial)),
        },
        _ => return Err(CborError::InvalidType(initial)),
    }
    Ok(())
}

fn skip_argument(data: &[u8], pos: &mut usize, ai: u8) -> Result<(), CborError> {
    decode_argument(data, pos, ai)?;
    Ok(())
}

fn decode_argument(data: &[u8], pos: &mut usize, ai: u8) -> Result<u64, CborError> {
    match ai {
        0..=23 => Ok(ai as u64),
        24 => {
            if *pos >= data.len() {
                return Err(CborError::Truncated);
            }
            let val = data[*pos] as u64;
            *pos += 1;
            Ok(val)
        }
        25 => {
            if *pos + 2 > data.len() {
                return Err(CborError::Truncated);
            }
            let val = u16::from_be_bytes([data[*pos], data[*pos + 1]]) as u64;
            *pos += 2;
            Ok(val)
        }
        26 => {
            if *pos + 4 > data.len() {
                return Err(CborError::Truncated);
            }
            let val = u32::from_be_bytes([
                data[*pos],
                data[*pos + 1],
                data[*pos + 2],
                data[*pos + 3],
            ]) as u64;
            *pos += 4;
            Ok(val)
        }
        27 => {
            if *pos + 8 > data.len() {
                return Err(CborError::Truncated);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&data[*pos..*pos + 8]);
            *pos += 8;
            Ok(u64::from_be_bytes(bytes))
        }
        _ => Err(CborError::InvalidType(ai)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn test_cbor_tv1_empty_map() {
        let bytes = hex_to_bytes("a0");
        let val = decode(&bytes).unwrap();
        assert_eq!(val, CborValue::Map(alloc::vec![]));
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv2_single_uint() {
        let bytes = hex_to_bytes("a101182a");
        let val = decode(&bytes).unwrap();
        match &val {
            CborValue::Map(pairs) => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, CborValue::UInt(1));
                assert_eq!(pairs[0].1, CborValue::UInt(42));
            }
            _ => panic!("Expected map"),
        }
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv3_negative_int() {
        let bytes = hex_to_bytes("a10126");
        let val = decode(&bytes).unwrap();
        match &val {
            CborValue::Map(pairs) => {
                assert_eq!(pairs[0].1, CborValue::NegInt(-7));
            }
            _ => panic!("Expected map"),
        }
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv4_float32() {
        let bytes = hex_to_bytes("a101fa41cc0000");
        let val = decode(&bytes).unwrap();
        match &val {
            CborValue::Map(pairs) => {
                assert_eq!(pairs[0].1, CborValue::Float32(25.5));
            }
            _ => panic!("Expected map"),
        }
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv5_float16() {
        let bytes = hex_to_bytes("a101f93c00");
        let val = decode(&bytes).unwrap();
        match &val {
            CborValue::Map(pairs) => {
                assert_eq!(pairs[0].1, CborValue::Float16Raw(0x3C00));
            }
            _ => panic!("Expected map"),
        }
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv6_booleans() {
        let bytes = hex_to_bytes("a201f502f4");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv7_null() {
        let bytes = hex_to_bytes("a101f6");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv8_short_text() {
        let bytes = hex_to_bytes("a101624869");
        let val = decode(&bytes).unwrap();
        match &val {
            CborValue::Map(pairs) => {
                assert_eq!(pairs[0].1, CborValue::Text("Hi".into()));
            }
            _ => panic!("Expected map"),
        }
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv9_byte_string() {
        let bytes = hex_to_bytes("a10144deadbeef");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv10_nested_array() {
        let bytes = hex_to_bytes("a101830a14181e");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv11_large_uint() {
        let bytes = hex_to_bytes("a1031a698fbb00");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv12_temp_hum() {
        let bytes = hex_to_bytes("a2011909f602191856");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv13_string_key() {
        // TV13 is a Standard-mode vector (string keys allowed).
        // In Compact mode, string keys are rejected.
        let bytes = hex_to_bytes("a1617416");
        let val = decode_standard(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);

        // Verify Compact mode rejects string keys.
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn test_cbor_tv14_gps_compact() {
        let bytes = hex_to_bytes("a2013a15f6a287021a682ac368");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    #[test]
    fn test_cbor_tv15_rgb_compact() {
        let bytes = hex_to_bytes("a30118ff0218800300");
        let val = decode(&bytes).unwrap();
        assert_eq!(encode(&val), bytes);
    }

    // ---- Error vectors ----

    #[test]
    fn test_cbor_err1_string_key_compact() {
        let bytes = hex_to_bytes("a1617416");
        assert_eq!(validate_compact(&bytes), Err(CborError::StringKeyInCompact));
    }

    #[test]
    fn test_cbor_err2_indefinite_length() {
        let bytes = hex_to_bytes("bf01182aff");
        assert_eq!(validate_compact(&bytes), Err(CborError::IndefiniteLength));
    }

    #[test]
    fn test_cbor_err3_tag_in_compact() {
        let bytes = hex_to_bytes("a101c0763230323630323135");
        assert_eq!(validate_compact(&bytes), Err(CborError::TagNotAllowed));
    }

    #[test]
    fn test_cbor_err4_truncated() {
        let bytes = hex_to_bytes("a201");
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn test_cbor_err5_undefined() {
        let bytes = hex_to_bytes("a101f7");
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn test_roundtrip_uint() {
        let val = CborValue::UInt(42);
        assert_eq!(decode(&encode(&val)).unwrap(), val);
    }

    #[test]
    fn test_roundtrip_negint() {
        let val = CborValue::NegInt(-7);
        assert_eq!(decode(&encode(&val)).unwrap(), val);
    }

    #[test]
    fn test_roundtrip_float32() {
        let val = CborValue::Float32(25.5);
        assert_eq!(decode(&encode(&val)).unwrap(), val);
    }

    #[test]
    fn test_roundtrip_float64() {
        let val = CborValue::Float64(core::f64::consts::PI);
        let encoded = encode(&val);
        // CBOR float64: 0xFB + 8 bytes big-endian IEEE 754
        assert_eq!(encoded[0], 0xFB);
        assert_eq!(encoded.len(), 9);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, val);
    }

    #[test]
    fn test_float64_special_values() {
        // Zero
        let val = CborValue::Float64(0.0);
        assert_eq!(decode(&encode(&val)).unwrap(), val);

        // Negative
        let val = CborValue::Float64(-273.15);
        assert_eq!(decode(&encode(&val)).unwrap(), val);

        // Very large
        let val = CborValue::Float64(1.7976931348623157e308);
        assert_eq!(decode(&encode(&val)).unwrap(), val);

        // Very small
        let val = CborValue::Float64(5e-324);
        assert_eq!(decode(&encode(&val)).unwrap(), val);
    }

    #[test]
    fn test_canonical_integer_encoding() {
        let encoded = encode(&CborValue::UInt(23));
        assert_eq!(encoded, &[0x17]);
        let encoded = encode(&CborValue::UInt(24));
        assert_eq!(encoded, &[0x18, 0x18]);
    }

    #[test]
    fn test_json_cbor_vectors() {
        let json_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../r2-specifications/testing/test-vectors/r2-cbor-vectors.json"
        ))
        .expect("Failed to read CBOR test vectors");

        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let vectors = json["conformance_vectors"]["vectors"].as_array().unwrap();
        for v in vectors {
            let hex_str = v["hex"].as_str().unwrap();
            let bytes = hex_to_bytes(hex_str);
            let desc = v["description"].as_str().unwrap_or("");

            // Use Standard mode for vectors that contain string keys.
            let result = if desc.contains("Standard") {
                decode_standard(&bytes)
            } else {
                decode(&bytes)
            };
            assert!(
                result.is_ok(),
                "Failed to decode {}: {:?}",
                v["id"],
                result.err()
            );
            let re_encoded = encode(&result.unwrap());
            assert_eq!(re_encoded, bytes, "Re-encode mismatch for {}", v["id"]);
        }
    }
}
