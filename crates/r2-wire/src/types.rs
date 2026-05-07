//! Core types for R2-WIRE protocol framing (SPEC.md §3–§4).

/// Wire-level errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// Output buffer too small for the encoded message.
    BufferTooSmall,
    /// Unsupported protocol version (only v0 is defined).
    InvalidVersion,
    /// Message type value out of range.
    InvalidMsgType,
    /// Route stack length is 0 or >8.
    InvalidRouteLen,
    /// Input data is shorter than expected.
    TruncatedMessage,
    /// Reserved message type (1, 6, 7).
    ReservedMsgType,
    /// Payload exceeds transport maximum.
    PayloadTooLarge,
    /// Extended format payload_len does not match actual remaining data (R2-WIRE §4.3.2).
    PayloadLenMismatch,
    /// TTL was already 0 — message MUST NOT be relayed (R2-WIRE §8.3).
    TtlExhausted,
}

/// Message type — 3 bits in byte 0 (SPEC.md §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    /// Standard event delivery (type 0).
    Event = 0,
    /// Reply routed via route stack (type 2).
    Reply = 2,
    /// Capability advertisement (type 3).
    Capability = 3,
    /// Trust group management (type 4).
    GroupMgmt = 4,
    /// Mesh heartbeat / keepalive (type 5).
    Heartbeat = 5,
}

impl MsgType {
    /// Parse a 3-bit message type value.
    pub fn from_u8(v: u8) -> Result<Self, WireError> {
        match v {
            0 => Ok(MsgType::Event),
            1 => Err(WireError::ReservedMsgType),
            2 => Ok(MsgType::Reply),
            3 => Ok(MsgType::Capability),
            4 => Ok(MsgType::GroupMgmt),
            5 => Ok(MsgType::Heartbeat),
            6 | 7 => Err(WireError::ReservedMsgType),
            _ => Err(WireError::InvalidMsgType),
        }
    }
}

/// Header flags — 3 bits in byte 0 (SPEC.md §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    /// Route stack is present (bit 2).
    pub has_route: bool,
    /// HMAC tag is appended (bit 1).
    pub has_hmac: bool,
    /// Message originated from a constrained MCU (bit 0).
    pub mcu_origin: bool,
}

impl Flags {
    /// Pack flags into 3 bits: `[R][H][M]`.
    pub fn to_bits(self) -> u8 {
        ((self.has_route as u8) << 2) | ((self.has_hmac as u8) << 1) | (self.mcu_origin as u8)
    }

    /// Unpack 3 bits into flags.
    pub fn from_bits(v: u8) -> Self {
        Flags {
            has_route: v & 0x04 != 0,
            has_hmac: v & 0x02 != 0,
            mcu_origin: v & 0x01 != 0,
        }
    }
}

/// Encode byte 0: `(version << 6) | (msg_type << 3) | flags` (SPEC.md §3.1).
pub fn encode_byte0(ver: u8, msg_type: MsgType, flags: Flags) -> u8 {
    (ver << 6) | ((msg_type as u8) << 3) | flags.to_bits()
}

/// Decode byte 0 into `(version, msg_type, flags)` (SPEC.md §3.1).
pub fn decode_byte0(b: u8) -> Result<(u8, MsgType, Flags), WireError> {
    let ver = b >> 6;
    if ver != 0 {
        return Err(WireError::InvalidVersion);
    }
    let typ = MsgType::from_u8((b >> 3) & 0x07)?;
    let flags = Flags::from_bits(b & 0x07);
    Ok((ver, typ, flags))
}

/// Encode byte 1: `(TTL << 4) | K` (SPEC.md §3.2).
pub fn encode_byte1(ttl: u8, k: u8) -> u8 {
    (ttl << 4) | (k & 0x0F)
}

/// Decode byte 1 into `(TTL, K)` (SPEC.md §3.2).
pub fn decode_byte1(b: u8) -> (u8, u8) {
    (b >> 4, b & 0x0F)
}

// ── Compact types (SPEC.md §3) ──────────────────────────────────

/// Compact message header — 12 bytes fixed (SPEC.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactHeader {
    /// Protocol version (must be 0).
    pub version: u8,
    /// Message type.
    pub msg_type: MsgType,
    /// Header flags.
    pub flags: Flags,
    /// Time-to-live (0–15, decremented at each hop).
    pub ttl: u8,
    /// Spray-and-wait budget (0–15).
    pub k: u8,
    /// Message ID for deduplication (16-bit).
    pub msg_id: u16,
    /// FNV-1a 32-bit hash of the event name.
    pub event_hash: u32,
    /// Target hive/group (32-bit FNV hash or special value).
    pub target: u32,
}

/// Compact route stack — 2-byte entries, max 8 (SPEC.md §3.3).
///
/// Each entry is bits [31:16] of FNV-1a 32-bit of the hive's device UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactRouteStack {
    /// Number of entries (1–8).
    pub len: u8,
    /// Route entries (only `len` are valid).
    pub entries: [u16; 8],
}

impl CompactRouteStack {
    /// Create an empty route stack.
    pub fn new() -> Self {
        CompactRouteStack {
            len: 0,
            entries: [0; 8],
        }
    }
}

/// A parsed compact message, borrowing payload from the input (SPEC.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactMessage<'a> {
    /// Decoded header fields.
    pub header: CompactHeader,
    /// Route stack (if R flag set).
    pub route: Option<CompactRouteStack>,
    /// CBOR payload (borrowed from input slice).
    pub payload: &'a [u8],
    /// Truncated HMAC-SHA256 tag — 8 bytes (if H flag set).
    pub hmac_tag: Option<[u8; 8]>,
}

// ── Extended types (SPEC.md §4) ─────────────────────────────────

/// Extended message header — 22 bytes fixed (SPEC.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedHeader {
    /// Protocol version (must be 0).
    pub version: u8,
    /// Message type.
    pub msg_type: MsgType,
    /// Header flags.
    pub flags: Flags,
    /// Time-to-live (0–15).
    pub ttl: u8,
    /// Spray-and-wait budget (0–15).
    pub k: u8,
    /// Message ID for deduplication (32-bit).
    pub msg_id: u32,
    /// FNV-1a 32-bit hash of the event name.
    pub event_hash: u32,
    /// Payload length in bytes (authoritative).
    pub payload_len: u32,
    /// Target trust group (32-bit FNV hash).
    pub target_group: u32,
    /// Target hive within group (32-bit FNV hash).
    pub target_hive: u32,
}

/// Extended route stack — 4-byte entries, max 8 (SPEC.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedRouteStack {
    /// Number of entries (1–8).
    pub len: u8,
    /// Route entries (only `len` are valid).
    pub entries: [u32; 8],
}

impl ExtendedRouteStack {
    /// Create an empty route stack.
    pub fn new() -> Self {
        ExtendedRouteStack {
            len: 0,
            entries: [0; 8],
        }
    }
}

/// A parsed extended message, borrowing payload from the input (SPEC.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedMessage<'a> {
    /// Decoded header fields.
    pub header: ExtendedHeader,
    /// Route stack (if R flag set).
    pub route: Option<ExtendedRouteStack>,
    /// CBOR payload (borrowed from input slice).
    pub payload: &'a [u8],
    /// Full HMAC-SHA256 tag — 32 bytes (if H flag set).
    pub hmac_tag: Option<[u8; 32]>,
}

// ── L2CAP CoC framing ───────────────────────────────────────────

/// Frame header byte for L2CAP CoC message framing.
///
/// Single byte prefix on each L2CAP SDU:
/// - `0x00` = complete message (no fragmentation)
/// - `0x80+` = fragment: bit 6 = last flag, bits [5:0] = sequence number
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHeader {
    /// Complete message — no fragmentation needed.
    Complete,
    /// Fragment with sequence number (0–63) and last-fragment flag.
    Fragment {
        /// True if this is the last fragment.
        last: bool,
        /// Fragment sequence number (0–63).
        sequence: u8,
    },
}

impl FrameHeader {
    /// Encode to a single byte.
    pub fn encode(&self) -> u8 {
        match self {
            FrameHeader::Complete => 0x00,
            FrameHeader::Fragment { last, sequence } => {
                (1 << 7) | ((*last as u8) << 6) | (sequence & 0x3F)
            }
        }
    }

    /// Decode from a single byte.
    pub fn decode(byte: u8) -> Self {
        if byte & 0x80 == 0 {
            FrameHeader::Complete
        } else {
            FrameHeader::Fragment {
                last: byte & 0x40 != 0,
                sequence: byte & 0x3F,
            }
        }
    }
}

// ── Spray-and-wait routing ──────────────────────────────────────

/// Split spray-and-wait budget K for relay: returns `(forwarded, retained)`.
///
/// Half goes to the relay copy, the remainder stays with the original.
/// K=0 means direct delivery only (no further spraying).
/// K=15 is the flood sentinel — both copies retain 15 (R2-WIRE §3.2).
pub fn k_split(k: u8) -> (u8, u8) {
    if k == 0 {
        return (0, 0);
    }
    if k == 15 {
        return (15, 15);
    }
    let fwd = k / 2;
    let retain = k - fwd;
    (fwd, retain)
}
