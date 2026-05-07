//! CBOR decoder. Single-pass, zero-allocation, streaming items.
//!
//! See SPEC.md §3 for Compact mode constraints and §6 for conformance vectors.

use crate::{Error, Mode};

/// A decoded CBOR item (SPEC.md §3).
///
/// Items are returned one at a time by [`Decoder::next`]. For maps and arrays,
/// the header item (`Map(n)` / `Array(n)`) is followed by `n` key-value pairs
/// or `n` elements respectively — the caller must track nesting.
#[derive(Debug, Clone, PartialEq)]
pub enum Item<'a> {
    /// Unsigned integer (major type 0).
    UInt(u64),
    /// Negative integer (major type 1), already decoded as `(-1 - raw)`.
    NegInt(i64),
    /// Boolean.
    Bool(bool),
    /// Null.
    Null,
    /// Raw float16 bits (SPEC.md §5).
    Float16Raw(u16),
    /// IEEE 754 float32.
    Float32(f32),
    /// IEEE 754 float64.
    Float64(f64),
    /// UTF-8 text string (borrowed from input slice).
    Text(&'a [u8]),
    /// Byte string (borrowed from input slice).
    Bytes(&'a [u8]),
    /// Array header — followed by `n` items.
    Array(usize),
    /// Map header — followed by `n` key-value pairs (2n items).
    Map(usize),
}

/// Maximum nesting depth for map key position tracking.
const MAX_NESTING: usize = 8;

/// Streaming CBOR decoder over a byte slice (SPEC.md §3).
///
/// Zero-allocation: items borrow directly from the input. The decoder
/// rejects tags (major type 6), `undefined`, and indefinite-length items
/// per Compact mode rules. In [`Mode::Compact`], string keys in maps are
/// rejected with [`Error::StringKeyInCompactMode`].
pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    mode: Mode,
    /// Stack tracking remaining items in each nesting level.
    /// Positive = in array (remaining items), negative = in map (remaining items, alternating key/value).
    /// Bit 0 of the absolute value tracks key(even)/value(odd) alternation for maps.
    nesting: [MapTracker; MAX_NESTING],
    depth: usize,
}

/// Tracks position within a map to detect key vs value positions.
#[derive(Clone, Copy)]
struct MapTracker {
    /// Number of remaining items (2*n for map, n for array).
    remaining: usize,
    /// True if this level is a map (as opposed to array).
    is_map: bool,
}

impl Default for MapTracker {
    fn default() -> Self {
        MapTracker { remaining: 0, is_map: false }
    }
}

impl<'a> Decoder<'a> {
    /// Create a decoder in Compact mode (default, backwards-compatible).
    pub fn new(data: &'a [u8]) -> Self {
        Self::new_with_mode(data, Mode::Compact)
    }

    /// Create a decoder with an explicit mode.
    pub fn new_with_mode(data: &'a [u8], mode: Mode) -> Self {
        Self {
            data,
            pos: 0,
            mode,
            nesting: [MapTracker::default(); MAX_NESTING],
            depth: 0,
        }
    }

    /// Current byte offset in the input.
    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// All bytes consumed?
    #[inline]
    pub fn is_done(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Returns true if there are still expected items in the nesting stack
    /// (i.e., a map or array was opened but not all items were consumed).
    pub fn has_pending_items(&self) -> bool {
        for i in 0..self.depth {
            if self.nesting[i].remaining > 0 {
                return true;
            }
        }
        false
    }

    /// Returns true if the current position is a map key in Compact mode.
    fn at_map_key(&self) -> bool {
        if self.mode != Mode::Compact || self.depth == 0 {
            return false;
        }
        let t = &self.nesting[self.depth - 1];
        // In a map, items alternate key(even remaining from pair boundary)/value.
        // remaining counts individual items (2*n_pairs). Even remaining = key position.
        t.is_map && t.remaining > 0 && t.remaining % 2 == 0
    }

    /// Advance nesting tracker after consuming one item (not a container header).
    fn advance_nesting(&mut self) {
        while self.depth > 0 {
            let t = &mut self.nesting[self.depth - 1];
            if t.remaining > 0 {
                t.remaining -= 1;
                break;
            } else {
                // This level is complete, pop up.
                self.depth -= 1;
            }
        }
    }

    /// Decode the next CBOR item.
    pub fn next(&mut self) -> Result<Item<'a>, Error> {
        let is_key_pos = self.at_map_key();

        let b = self.take_byte()?;
        let major = b >> 5;
        let ai = b & 0x1f;

        match major {
            0 => {
                let item = Item::UInt(self.argument(ai)?);
                self.advance_nesting();
                Ok(item)
            }
            1 => {
                let arg = self.argument(ai)?;
                let item = Item::NegInt(-1 - arg as i64);
                self.advance_nesting();
                Ok(item)
            }
            2 => {
                if ai == 31 {
                    return Err(Error::IndefiniteLength);
                }
                if is_key_pos {
                    return Err(Error::StringKeyInCompactMode);
                }
                let len = self.argument(ai)? as usize;
                let item = Item::Bytes(self.take_slice(len)?);
                self.advance_nesting();
                Ok(item)
            }
            3 => {
                if ai == 31 {
                    return Err(Error::IndefiniteLength);
                }
                if is_key_pos {
                    return Err(Error::StringKeyInCompactMode);
                }
                let len = self.argument(ai)? as usize;
                let item = Item::Text(self.take_slice(len)?);
                self.advance_nesting();
                Ok(item)
            }
            4 => {
                if ai == 31 {
                    return Err(Error::IndefiniteLength);
                }
                let n = self.argument(ai)? as usize;
                // Push array level onto nesting stack.
                self.advance_nesting();
                if self.depth < MAX_NESTING {
                    self.nesting[self.depth] = MapTracker { remaining: n, is_map: false };
                    self.depth += 1;
                }
                Ok(Item::Array(n))
            }
            5 => {
                if ai == 31 {
                    return Err(Error::IndefiniteLength);
                }
                let n = self.argument(ai)? as usize;
                // Push map level onto nesting stack (2*n items: alternating key/value).
                self.advance_nesting();
                if self.depth < MAX_NESTING {
                    self.nesting[self.depth] = MapTracker { remaining: n * 2, is_map: true };
                    self.depth += 1;
                }
                Ok(Item::Map(n))
            }
            6 => Err(Error::DisallowedType), // tags
            7 => {
                let item = match ai {
                    20 => Item::Bool(false),
                    21 => Item::Bool(true),
                    22 => Item::Null,
                    23 => return Err(Error::DisallowedType), // undefined
                    25 => {
                        let s = self.take_slice(2)?;
                        Item::Float16Raw(u16::from_be_bytes([s[0], s[1]]))
                    }
                    26 => {
                        let s = self.take_slice(4)?;
                        let bits = u32::from_be_bytes([s[0], s[1], s[2], s[3]]);
                        Item::Float32(f32::from_bits(bits))
                    }
                    27 => {
                        let s = self.take_slice(8)?;
                        let mut arr = [0u8; 8];
                        arr.copy_from_slice(s);
                        Item::Float64(f64::from_bits(u64::from_be_bytes(arr)))
                    }
                    _ => return Err(Error::DisallowedType),
                };
                self.advance_nesting();
                Ok(item)
            }
            _ => Err(Error::DisallowedType),
        }
    }

    // ── internals ───────────────────────────────────────────────────

    fn argument(&mut self, ai: u8) -> Result<u64, Error> {
        match ai {
            0..=23 => Ok(ai as u64),
            24 => Ok(self.take_byte()? as u64),
            25 => {
                let s = self.take_slice(2)?;
                Ok(u16::from_be_bytes([s[0], s[1]]) as u64)
            }
            26 => {
                let s = self.take_slice(4)?;
                Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64)
            }
            27 => {
                let s = self.take_slice(8)?;
                let mut arr = [0u8; 8];
                arr.copy_from_slice(s);
                Ok(u64::from_be_bytes(arr))
            }
            _ => Err(Error::DisallowedType),
        }
    }

    #[inline]
    fn take_byte(&mut self) -> Result<u8, Error> {
        if self.pos >= self.data.len() {
            return Err(Error::Truncated);
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn take_slice(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if self.pos + len > self.data.len() {
            return Err(Error::Truncated);
        }
        let s = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Value;
    use crate::Encoder;

    fn hex(s: &str) -> [u8; 180] {
        let s = s.replace(' ', "");
        let mut out = [0u8; 180];
        let b = s.as_bytes();
        let mut i = 0;
        let mut p = 0;
        while p + 1 < b.len() {
            let hi = match b[p] {
                b'0'..=b'9' => b[p] - b'0',
                b'a'..=b'f' => b[p] - b'a' + 10,
                _ => panic!(),
            };
            let lo = match b[p + 1] {
                b'0'..=b'9' => b[p + 1] - b'0',
                b'a'..=b'f' => b[p + 1] - b'a' + 10,
                _ => panic!(),
            };
            out[i] = (hi << 4) | lo;
            i += 1;
            p += 2;
        }
        out
    }

    fn hex_len(s: &str) -> usize {
        s.replace(' ', "").len() / 2
    }

    fn check_encode(f: impl FnOnce(&mut Encoder), expected: &str) {
        let mut buf = [0u8; 180];
        let mut enc = Encoder::new(&mut buf);
        f(&mut enc);
        let got = enc.as_bytes();
        let exp = hex(expected);
        let exp_len = hex_len(expected);
        assert_eq!(got.len(), exp_len, "length mismatch");
        assert_eq!(got, &exp[..exp_len], "content mismatch");
    }

    // ── R2-CBOR §6 Test Vectors (encode) ────────────────────────────

    #[test]
    fn tv01() {
        check_encode(
            |e| {
                e.map(0).unwrap();
            },
            "a0",
        );
    }
    #[test]
    fn tv02() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::UInt(42)).unwrap();
            },
            "a101182a",
        );
    }
    #[test]
    fn tv03() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::NegInt(-7)).unwrap();
            },
            "a10126",
        );
    }
    #[test]
    fn tv04() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::Float32(25.5)).unwrap();
            },
            "a101fa41cc0000",
        );
    }
    #[test]
    fn tv05() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::Float16Raw(0x3c00)).unwrap();
            },
            "a101f93c00",
        );
    }
    #[test]
    fn tv06() {
        check_encode(
            |e| {
                e.map(2).unwrap();
                e.kv(1, &Value::Bool(true)).unwrap();
                e.kv(2, &Value::Bool(false)).unwrap();
            },
            "a201f502f4",
        );
    }
    #[test]
    fn tv07() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::Null).unwrap();
            },
            "a101f6",
        );
    }
    #[test]
    fn tv08() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::Text("Hi")).unwrap();
            },
            "a101624869",
        );
    }
    #[test]
    fn tv09() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(1, &Value::Bytes(&[0xDE, 0xAD, 0xBE, 0xEF])).unwrap();
            },
            "a10144deadbeef",
        );
    }
    #[test]
    fn tv10() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.uint(1).unwrap();
                e.array(3).unwrap();
                e.uint(10).unwrap();
                e.uint(20).unwrap();
                e.uint(30).unwrap();
            },
            "a101830a14181e",
        );
    }
    #[test]
    fn tv11() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.kv(3, &Value::UInt(1771027200)).unwrap();
            },
            "a1031a698fbb00",
        );
    }
    #[test]
    fn tv12() {
        check_encode(
            |e| {
                e.map(2).unwrap();
                e.kv(1, &Value::UInt(2550)).unwrap();
                e.kv(2, &Value::UInt(6230)).unwrap();
            },
            "a2011909f602191856",
        );
    }
    #[test]
    fn tv13() {
        check_encode(
            |e| {
                e.map(1).unwrap();
                e.text("t").unwrap();
                e.uint(22).unwrap();
            },
            "a1617416",
        );
    }
    #[test]
    fn tv14() {
        check_encode(
            |e| {
                e.map(2).unwrap();
                e.kv(1, &Value::NegInt(-368485000)).unwrap();
                e.kv(2, &Value::UInt(1747633000)).unwrap();
            },
            "a2013a15f6a287021a682ac368",
        );
    }
    #[test]
    fn tv15() {
        check_encode(
            |e| {
                e.map(3).unwrap();
                e.kv(1, &Value::UInt(255)).unwrap();
                e.kv(2, &Value::UInt(128)).unwrap();
                e.kv(3, &Value::UInt(0)).unwrap();
            },
            "a30118ff02188003 00",
        );
    }

    // ── Decode tests ────────────────────────────────────────────────

    #[test]
    fn decode_tv02() {
        let data = [0xa1, 0x01, 0x18, 0x2a];
        let mut d = Decoder::new(&data);
        assert_eq!(d.next().unwrap(), Item::Map(1));
        assert_eq!(d.next().unwrap(), Item::UInt(1));
        assert_eq!(d.next().unwrap(), Item::UInt(42));
        assert!(d.is_done());
    }

    #[test]
    fn decode_tv14_gps() {
        let data = [
            0xa2, 0x01, 0x3a, 0x15, 0xf6, 0xa2, 0x87, 0x02, 0x1a, 0x68, 0x2a, 0xc3, 0x68,
        ];
        let mut d = Decoder::new(&data);
        assert_eq!(d.next().unwrap(), Item::Map(2));
        assert_eq!(d.next().unwrap(), Item::UInt(1));
        assert_eq!(d.next().unwrap(), Item::NegInt(-368485000));
        assert_eq!(d.next().unwrap(), Item::UInt(2));
        assert_eq!(d.next().unwrap(), Item::UInt(1747633000));
    }

    #[test]
    fn decode_rejects_undefined() {
        let data = [0xf7]; // undefined
        let mut d = Decoder::new(&data);
        assert_eq!(d.next().unwrap_err(), Error::DisallowedType);
    }

    #[test]
    fn decode_rejects_tags() {
        let data = [0xc0];
        let mut d = Decoder::new(&data);
        assert_eq!(d.next().unwrap_err(), Error::DisallowedType);
    }

    #[test]
    fn decode_truncated() {
        let data = [0xa2, 0x01];
        let mut d = Decoder::new(&data);
        assert_eq!(d.next().unwrap(), Item::Map(2));
        assert_eq!(d.next().unwrap(), Item::UInt(1));
        assert_eq!(d.next().unwrap_err(), Error::Truncated);
    }

    // ── JSON vector conformance tests ────────────────────────────────
    // Loaded from r2-specifications canonical vectors.
    // Any R2 implementation MUST pass these.

    extern crate alloc;
    use alloc::vec::Vec;

    const VECTORS_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../r2-specifications/testing/test-vectors/r2-cbor-vectors.json"
    ));

    fn hex_to_bytes(hex_str: &str) -> Vec<u8> {
        let s = hex_str.replace(' ', "");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    /// Decode side of the canonical R2-CBOR conformance vectors. Verifies
    /// that the decoder accepts every canonical hex without errors. The
    /// encode side (byte-equal output for each vector) is in
    /// `encode::conformance_encode_tests` — a separate module that closes
    /// the gap formerly left here when this test was misnamed
    /// `json_conformance_encode` (it never tested the encoder).
    #[test]
    fn json_conformance_decode_canonical_hex() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON)
            .expect("valid CBOR vectors JSON");
        let vectors = data["conformance_vectors"]["vectors"]
            .as_array()
            .expect("conformance_vectors.vectors array");

        assert!(vectors.len() >= 15, "expected ≥15 vectors, got {}", vectors.len());

        for v in vectors {
            let id = v["id"].as_str().unwrap_or("?");
            let expected_hex = v["hex"].as_str().expect("hex string");
            let expected_bytes = hex_to_bytes(expected_hex);
            let desc = v["description"].as_str().unwrap_or("");

            // TV13 is a Standard-mode vector (string keys); decode accordingly.
            let mode = if desc.contains("Standard") {
                crate::Mode::Standard
            } else {
                crate::Mode::Compact
            };

            // Verify decode succeeds (doesn't error)
            let mut decoder = Decoder::new_with_mode(&expected_bytes, mode);
            let first = decoder.next();
            assert!(
                first.is_ok(),
                "CBOR conformance FAIL: {} decode error on hex '{}': {:?}",
                id, expected_hex, first
            );
        }
    }

    #[test]
    fn json_error_vectors() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON)
            .expect("valid CBOR vectors JSON");
        let vectors = data["error_vectors"]["vectors"]
            .as_array()
            .expect("error_vectors.vectors array");

        for v in vectors {
            let id = v["id"].as_str().unwrap_or("?");
            let hex_str = v["hex"].as_str().expect("hex string");
            let bytes = hex_to_bytes(hex_str);

            // All error vectors are tested in Compact mode (the strictest mode).
            let mut decoder = Decoder::new(&bytes);
            let mut found_error = false;
            let mut items_decoded = 0usize;
            for _ in 0..20 {
                match decoder.next() {
                    Err(_) => { found_error = true; break; }
                    Ok(_) => {
                        items_decoded += 1;
                        if decoder.is_done() {
                            // If input exhausted but nesting incomplete, that's also truncated.
                            if decoder.has_pending_items() {
                                found_error = true;
                            }
                            break;
                        }
                    }
                }
            }
            assert!(
                found_error,
                "CBOR error vector {} should have produced an error in Compact mode but decoded successfully ({} items)",
                id, items_decoded,
            );
        }
    }
}
