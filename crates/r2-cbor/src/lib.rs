//! # r2-cbor
//!
//! Constrained CBOR encoding for [Reality2](https://github.com/reality2-ai),
//! implementing the R2-CBOR specification (see [`SPEC.md`](../SPEC.md) in this crate).
//!
//! Provides `no_std` CBOR encoder and decoder for Compact mode (integer keys,
//! definite-length, ≤180 byte payloads). Zero allocation, single-pass.
//! Designed for MCUs with 2–8 KB SRAM.
//!
//! ## Encoding
//!
//! ```
//! use r2_cbor::{Encoder, Value};
//!
//! let mut buf = [0u8; 180];
//! let mut enc = Encoder::new(&mut buf);
//! enc.map(2).unwrap();
//! enc.kv(1, &Value::UInt(2550)).unwrap();   // temp: 25.50°C (integer-scaled)
//! enc.kv(2, &Value::UInt(6230)).unwrap();   // hum: 62.30%
//! let encoded = enc.as_bytes();
//! assert_eq!(encoded, &[0xA2, 0x01, 0x19, 0x09, 0xF6, 0x02, 0x19, 0x18, 0x56]);
//! ```
//!
//! ## Decoding
//!
//! ```
//! use r2_cbor::{Decoder, Item};
//!
//! let data = [0xA1, 0x01, 0x18, 0x2A]; // {1: 42}
//! let mut dec = Decoder::new(&data);
//! assert_eq!(dec.next().unwrap(), Item::Map(1));
//! assert_eq!(dec.next().unwrap(), Item::UInt(1));   // key
//! assert_eq!(dec.next().unwrap(), Item::UInt(42));   // value
//! assert!(dec.is_done());
//! ```
//!
//! ## Compact Mode Constraints (SPEC.md §3)
//!
//! - Integer keys only (no string keys)
//! - Definite length only (no indefinite-length maps/arrays/strings)
//! - No CBOR tags (major type 6)
//! - No `undefined` simple value
//! - Maximum payload: 180 bytes

#![no_std]
#![deny(missing_docs)]

mod decode;
mod encode;

pub use decode::{Decoder, Item};
pub use encode::{Encoder, Value};

/// Maximum Compact mode payload in bytes (SPEC.md §2).
pub const COMPACT_MAX: usize = 180;

/// Maximum Standard mode payload in bytes (SPEC.md §2).
pub const STANDARD_MAX: usize = 65535;

/// R2-CBOR encoding mode (SPEC.md §3).
///
/// The mode is determined by the transport — it is NOT signalled in the
/// payload itself. Compact mode is used on BLE/LoRa; Standard mode on
/// WiFi/IP transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Compact mode: integer keys only, ≤180 bytes (BLE, LoRa).
    Compact,
    /// Standard mode: string keys allowed, ≤65 535 bytes (WiFi, IP).
    Standard,
}

/// Errors during CBOR encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Output buffer is full (encoder).
    BufferFull,
    /// Input is truncated or malformed (decoder).
    Truncated,
    /// CBOR type not permitted in this mode (tags, undefined).
    DisallowedType,
    /// Indefinite-length item encountered in Compact mode.
    IndefiniteLength,
    /// String key encountered in Compact mode (R2-CBOR §3.1).
    StringKeyInCompactMode,
}
