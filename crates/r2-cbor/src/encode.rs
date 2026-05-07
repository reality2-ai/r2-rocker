//! CBOR Compact mode encoder. Zero-allocation, single-pass.
//!
//! See SPEC.md §3–§5 for encoding rules.

use crate::Error;

/// A CBOR value for encoding (SPEC.md §3–§5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    /// Unsigned integer (major type 0).
    UInt(u64),
    /// Negative integer (major type 1). Value must be < 0.
    NegInt(i64),
    /// Boolean (simple values true/false).
    Bool(bool),
    /// Null (simple value 22).
    Null,
    /// IEEE 754 float32 (SPEC.md §5).
    Float32(f32),
    /// IEEE 754 float64 (SPEC.md §5).
    Float64(f64),
    /// Raw float16 bits — caller is responsible for IEEE 754 encoding.
    Float16Raw(u16),
    /// UTF-8 text string (major type 3).
    Text(&'a str),
    /// Byte string (major type 2).
    Bytes(&'a [u8]),
}

#[cfg(test)]
mod conformance_encode_tests {
    //! Encoder round-trip tests against the canonical R2-CBOR conformance
    //! vectors. Closes the gap identified in
    //! r2-specifications/audits/2026-05-01/compliance.md F20: encode.rs
    //! previously had zero #[test] functions and the misnamed
    //! json_conformance_encode test in decode.rs only decoded.
    //!
    //! For each canonical vector that is expressible through the encoder API,
    //! this constructs the value and asserts byte-equality against the
    //! `hex` field. This verifies the encoder produces canonical bytes —
    //! specifically: §4.5 shortest integer encoding, §4.2 definite-length
    //! only, §10 no duplicate keys (by construction), and the byte layout
    //! of every primitive Value variant.
    //!
    //! Standard-mode vectors (TV13, string keys) are skipped — the current
    //! encoder API targets Compact mode only.

    extern crate alloc;
    use alloc::vec::Vec;
    use super::{Encoder, Value};

    /// Encode `body` into a fresh buffer, return the written bytes.
    fn encode<F>(body: F) -> Vec<u8>
    where
        F: FnOnce(&mut Encoder) -> Result<(), crate::Error>,
    {
        let mut buf = [0u8; 64];
        let n = {
            let mut enc = Encoder::new(&mut buf);
            body(&mut enc).expect("encode succeeds");
            enc.len()
        };
        buf[..n].to_vec()
    }

    /// Hex string → byte vector (canonical R2 vectors are lowercase hex).
    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn tv1_empty_map() {
        // {} → 0xa0
        let actual = encode(|e| e.map(0));
        assert_eq!(actual, hex_to_bytes("a0"), "TV1: empty map");
    }

    #[test]
    fn tv2_single_uint() {
        // {1: 42} → 0xa101182a
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::UInt(42))
        });
        assert_eq!(actual, hex_to_bytes("a101182a"), "TV2: {{1: 42}}");
    }

    #[test]
    fn tv3_single_negint() {
        // {1: -7} → 0xa10126
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::NegInt(-7))
        });
        assert_eq!(actual, hex_to_bytes("a10126"), "TV3: {{1: -7}}");
    }

    #[test]
    fn tv4_single_float32() {
        // {1: 25.5_f32} → 0xa101fa41cc0000
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::Float32(25.5))
        });
        assert_eq!(actual, hex_to_bytes("a101fa41cc0000"), "TV4: float32 25.5");
    }

    #[test]
    fn tv5_single_float16() {
        // {1: 1.0_f16} → 0xa101f93c00
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::Float16Raw(0x3c00))
        });
        assert_eq!(actual, hex_to_bytes("a101f93c00"), "TV5: float16 1.0");
    }

    #[test]
    fn tv6_boolean_pair() {
        // {1: true, 2: false} → 0xa201f502f4
        let actual = encode(|e| {
            e.map(2)?;
            e.kv(1, &Value::Bool(true))?;
            e.kv(2, &Value::Bool(false))
        });
        assert_eq!(actual, hex_to_bytes("a201f502f4"), "TV6: boolean pair");
    }

    #[test]
    fn tv7_null_value() {
        // {1: null} → 0xa101f6
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::Null)
        });
        assert_eq!(actual, hex_to_bytes("a101f6"), "TV7: null");
    }

    #[test]
    fn tv8_short_text() {
        // {1: "Hi"} → 0xa101624869
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::Text("Hi"))
        });
        assert_eq!(actual, hex_to_bytes("a101624869"), "TV8: text \"Hi\"");
    }

    #[test]
    fn tv9_byte_string() {
        // {1: h'DEADBEEF'} → 0xa10144deadbeef
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(1, &Value::Bytes(&[0xde, 0xad, 0xbe, 0xef]))
        });
        assert_eq!(actual, hex_to_bytes("a10144deadbeef"), "TV9: byte string");
    }

    #[test]
    fn tv10_nested_array() {
        // {1: [10, 20, 30]} → 0xa101830a14181e
        let actual = encode(|e| {
            e.map(1)?;
            e.uint(1)?; // key
            e.array(3)?;
            e.uint(10)?;
            e.uint(20)?;
            e.uint(30)
        });
        assert_eq!(actual, hex_to_bytes("a101830a14181e"), "TV10: nested array");
    }

    #[test]
    fn tv11_large_uint_timestamp() {
        // {3: 1771027200} → 0xa1031a698fbb00
        let actual = encode(|e| {
            e.map(1)?;
            e.kv(3, &Value::UInt(1771027200))
        });
        assert_eq!(actual, hex_to_bytes("a1031a698fbb00"), "TV11: timestamp uint");
    }

    #[test]
    fn tv12_temp_hum_compact() {
        // {1: 2550, 2: 6230} → 0xa2011909f602191856
        let actual = encode(|e| {
            e.map(2)?;
            e.kv(1, &Value::UInt(2550))?;
            e.kv(2, &Value::UInt(6230))
        });
        assert_eq!(actual, hex_to_bytes("a2011909f602191856"), "TV12: temp+hum");
    }

    #[test]
    fn tv14_gps_compact() {
        // {1: -368485000, 2: 1747633000} → 0xa2013a15f6a287021a682ac368
        let actual = encode(|e| {
            e.map(2)?;
            e.kv(1, &Value::NegInt(-368485000))?;
            e.kv(2, &Value::UInt(1747633000))
        });
        assert_eq!(
            actual,
            hex_to_bytes("a2013a15f6a287021a682ac368"),
            "TV14: GPS"
        );
    }

    #[test]
    fn tv15_rgb_compact() {
        // {1: 255, 2: 128, 3: 0} → 0xa30118ff0218800300
        let actual = encode(|e| {
            e.map(3)?;
            e.kv(1, &Value::UInt(255))?;
            e.kv(2, &Value::UInt(128))?;
            e.kv(3, &Value::UInt(0))
        });
        assert_eq!(actual, hex_to_bytes("a30118ff0218800300"), "TV15: RGB");
    }

    /// Cross-check: shortest integer encoding (§4.5). The encoder MUST use
    /// the smallest representation. {1: 0}, {1: 1}, {1: 23} all fit in one
    /// byte after the major type; 24..=255 use one extra byte; etc.
    #[test]
    fn shortest_integer_encoding() {
        // 0 → 0x00 (one byte)
        assert_eq!(encode(|e| e.uint(0)), hex_to_bytes("00"));
        // 23 → 0x17 (still one byte — argument fits in MT lower 5 bits)
        assert_eq!(encode(|e| e.uint(23)), hex_to_bytes("17"));
        // 24 → 0x1818 (one byte argument)
        assert_eq!(encode(|e| e.uint(24)), hex_to_bytes("1818"));
        // 255 → 0x18ff
        assert_eq!(encode(|e| e.uint(255)), hex_to_bytes("18ff"));
        // 256 → 0x190100 (two byte argument)
        assert_eq!(encode(|e| e.uint(256)), hex_to_bytes("190100"));
        // 65535 → 0x19ffff
        assert_eq!(encode(|e| e.uint(65535)), hex_to_bytes("19ffff"));
        // 65536 → 0x1a00010000 (four byte argument)
        assert_eq!(encode(|e| e.uint(65536)), hex_to_bytes("1a00010000"));
    }

    /// §4.6: encoder MUST signal an error when payload exceeds buffer.
    /// The Encoder enforces this via `Error::BufferFull`.
    #[test]
    fn encoder_rejects_buffer_overflow() {
        let mut tiny = [0u8; 2];
        let mut enc = Encoder::new(&mut tiny);
        // map header fits (1 byte), key fits (1 byte), value won't fit
        enc.map(1).expect("map header fits");
        enc.uint(1).expect("key fits");
        let result = enc.value(&Value::UInt(42));
        assert!(
            matches!(result, Err(crate::Error::BufferFull)),
            "expected BufferFull, got {:?}",
            result
        );
    }
}

/// Fixed-buffer CBOR encoder (SPEC.md §3–§4).
///
/// Writes CBOR into a caller-provided `&mut [u8]` buffer. No heap allocation.
/// Returns [`Error::BufferFull`] if the buffer is exhausted.
pub struct Encoder<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Encoder<'a> {
    /// Create an encoder over a mutable buffer.
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Write a map header (n key-value pairs).
    #[inline]
    pub fn map(&mut self, n: usize) -> Result<(), Error> {
        self.head(5, n as u64)
    }

    /// Write an array header (n items).
    #[inline]
    pub fn array(&mut self, n: usize) -> Result<(), Error> {
        self.head(4, n as u64)
    }

    /// Write a uint key followed by a value (Compact mode convenience).
    #[inline]
    pub fn kv(&mut self, key: u64, val: &Value) -> Result<(), Error> {
        self.uint(key)?;
        self.value(val)
    }

    /// Write a single value.
    pub fn value(&mut self, val: &Value) -> Result<(), Error> {
        match *val {
            Value::UInt(v) => self.uint(v),
            Value::NegInt(v) => self.negint(v),
            Value::Bool(true) => self.byte(0xf5),
            Value::Bool(false) => self.byte(0xf4),
            Value::Null => self.byte(0xf6),
            Value::Float32(v) => {
                self.byte(0xfa)?;
                self.raw(&v.to_bits().to_be_bytes())
            }
            Value::Float64(v) => {
                self.byte(0xfb)?;
                self.raw(&v.to_bits().to_be_bytes())
            }
            Value::Float16Raw(bits) => {
                self.byte(0xf9)?;
                self.raw(&bits.to_be_bytes())
            }
            Value::Text(s) => {
                let b = s.as_bytes();
                self.head(3, b.len() as u64)?;
                self.raw(b)
            }
            Value::Bytes(b) => {
                self.head(2, b.len() as u64)?;
                self.raw(b)
            }
        }
    }

    /// Write an unsigned integer.
    #[inline]
    pub fn uint(&mut self, val: u64) -> Result<(), Error> {
        self.head(0, val)
    }

    /// Write a negative integer (val must be < 0).
    #[inline]
    pub fn negint(&mut self, val: i64) -> Result<(), Error> {
        self.head(1, (-1 - val) as u64)
    }

    /// Write a text string.
    pub fn text(&mut self, s: &str) -> Result<(), Error> {
        let b = s.as_bytes();
        self.head(3, b.len() as u64)?;
        self.raw(b)
    }

    /// Return the encoded bytes so far.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.pos]
    }

    /// Bytes written.
    #[inline]
    pub fn len(&self) -> usize {
        self.pos
    }

    /// Returns `true` if no bytes have been written.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    // ── internals ───────────────────────────────────────────────────

    fn head(&mut self, major: u8, val: u64) -> Result<(), Error> {
        let mt = major << 5;
        if val <= 23 {
            self.byte(mt | val as u8)
        } else if val <= 0xFF {
            self.byte(mt | 24)?;
            self.byte(val as u8)
        } else if val <= 0xFFFF {
            self.byte(mt | 25)?;
            self.raw(&(val as u16).to_be_bytes())
        } else if val <= 0xFFFF_FFFF {
            self.byte(mt | 26)?;
            self.raw(&(val as u32).to_be_bytes())
        } else {
            self.byte(mt | 27)?;
            self.raw(&val.to_be_bytes())
        }
    }

    #[inline]
    fn byte(&mut self, b: u8) -> Result<(), Error> {
        if self.pos >= self.buf.len() {
            return Err(Error::BufferFull);
        }
        self.buf[self.pos] = b;
        self.pos += 1;
        Ok(())
    }

    #[inline]
    fn raw(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.pos + data.len() > self.buf.len() {
            return Err(Error::BufferFull);
        }
        self.buf[self.pos..self.pos + data.len()].copy_from_slice(data);
        self.pos += data.len();
        Ok(())
    }
}
