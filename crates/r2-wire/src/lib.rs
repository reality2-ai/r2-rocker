//! # r2-wire
//!
//! Wire protocol framing for [Reality2](https://github.com/reality2-ai),
//! implementing the R2-WIRE specification (see [`SPEC.md`](../SPEC.md) in this crate).
//!
//! Two message formats:
//! - **Compact** (12-byte header) — BLE, LoRa: 16-bit msg_id, 32-bit target
//! - **Extended** (22-byte header) — TCP, WebSocket: 32-bit msg_id, dual target
//!
//! `no_std`, zero-allocation encode/decode on `&[u8]` slices.
//!
//! ## Quick Start
//!
//! ```
//! use r2_wire::*;
//!
//! // Encode a compact EVENT message
//! let msg = CompactMessage {
//!     header: CompactHeader {
//!         version: 0,
//!         msg_type: MsgType::Event,
//!         flags: Flags::default(),
//!         ttl: 5, k: 3,
//!         msg_id: 0xA1B2,
//!         event_hash: 0x424D3E4C,  // "read_level"
//!         target: 0x1A2B3C4D,
//!     },
//!     route: None,
//!     payload: &[0xA1, 0x00, 0x18, 0xEA],  // CBOR {0: 234}
//!     hmac_tag: None,
//! };
//!
//! let mut buf = [0u8; 256];
//! let len = encode_compact(&msg, &mut buf).unwrap();
//! assert_eq!(len, 16);
//!
//! // Decode it back
//! let decoded = decode_compact(&buf[..len]).unwrap();
//! assert_eq!(decoded.header.event_hash, 0x424D3E4C);
//! assert_eq!(decoded.payload, &[0xA1, 0x00, 0x18, 0xEA]);
//! ```
//!
//! ## Transcoding
//!
//! Gateway devices transcode between compact (BLE/LoRa) and extended (TCP/WiFi):
//!
//! ```
//! use r2_wire::*;
//!
//! let compact_bytes = [
//!     0x00, 0x53, 0xA1, 0xB2, 0x42, 0x4D, 0x3E, 0x4C,
//!     0x1A, 0x2B, 0x3C, 0x4D, 0xA1, 0x00, 0x18, 0xEA,
//! ];
//! let mut ext_buf = [0u8; 256];
//! let len = transcode_compact_to_extended(&compact_bytes, &mut ext_buf).unwrap();
//! let ext = decode_extended(&ext_buf[..len]).unwrap();
//! assert_eq!(ext.header.event_hash, 0x424D3E4C);
//! ```

#![no_std]
#![deny(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

/// Compact message encode/decode (SPEC.md §3).
pub mod compact;
/// Extended message encode/decode (SPEC.md §4).
pub mod extended;
/// HMAC envelope for authenticated messaging (SPEC.md §10, R2-TRUST §6).
pub mod hmac;
/// Compact ↔ Extended transcoding (SPEC.md §5).
pub mod transcode;
/// Core types: headers, flags, route stacks, messages (SPEC.md §3–§4).
pub mod types;

pub use compact::{decode_compact, encode_compact};
pub use extended::{decode_extended, encode_extended};
pub use hmac::{
    FrameClass, HmacProvider, classify_compact, classify_extended, sign_compact, sign_extended,
    verify_compact, verify_extended, COMPACT_TAG_LEN, EXTENDED_TAG_LEN,
};
pub use transcode::{transcode_compact_to_extended, transcode_extended_to_compact};
pub use types::{
    k_split, CompactHeader, CompactMessage, CompactRouteStack, ExtendedHeader, ExtendedMessage,
    ExtendedRouteStack, Flags, FrameHeader, MsgType, WireError, decode_byte0, decode_byte1,
    encode_byte0, encode_byte1,
};

#[cfg(test)]
mod tests;
