//! The core transport abstraction — medium-agnostic interface.
//!
//! Every transport in R2 (BLE, WiFi/UDP, LoRa, TCP/IP) implements the
//! [`Transport`] trait.  The R2 logical mesh (R2-ROUTE §1.4.2) interacts
//! with transports exclusively through this interface.
//!
//! ## Scope
//!
//! Transports carry **R2-WIRE frames only** — events, heartbeats,
//! capabilities, GROUP_MGMT.  These are small, fire-and-forget messages.
//!
//! Bulk data (firmware, files, chat) is carried by **plugins** using
//! their own protocols over the connectivity that transports provide.
//! Plugins are not part of this trait.
//!
//! ## Information Flow (R2-ROUTE §1.4.4)
//!
//! **Upward (transport → logical mesh):**
//! - Reachability: "hive X reachable" / "hive Y gone"
//! - Link quality: RSSI, SNR, latency
//! - Transport state: available / degraded / unavailable
//!
//! **Downward (logical mesh → transport):**
//! - Send: "deliver this frame to hive X"
//! - Priority: relay-forwarded (critical) vs heartbeat (background)

use crate::format::WireFormat;

// ---------------------------------------------------------------------------
// Transport identity
// ---------------------------------------------------------------------------

/// Identifies a transport type.
///
/// Used as an index into the neighbour table's per-transport arrays
/// (R2-ROUTE §2.2) and for transport selection scoring (R2-ROUTE §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TransportId {
    /// Bluetooth Low Energy (R2-BLE).
    Ble = 0,
    /// WiFi — UDP events over SoftAP or infrastructure (R2-WIFI).
    Wifi = 1,
    /// LoRa radio (R2-LORA).
    Lora = 2,
    /// Internet — TCP/IP to remote hives beyond radio range.
    Internet = 3,
}

impl TransportId {
    /// Bitmask for the neighbour table `transports` bitfield (R2-ROUTE §2.2).
    pub const fn bitmask(self) -> u8 {
        1 << (self as u8)
    }

    /// The R2-WIRE format expected on this transport (R2-WIRE §4.3.5).
    ///
    /// BLE and LoRa use compact format (12-byte header).
    /// WiFi and Internet use extended format (22-byte header).
    pub const fn wire_format(self) -> WireFormat {
        match self {
            Self::Ble | Self::Lora => WireFormat::Compact,
            Self::Wifi | Self::Internet => WireFormat::Extended,
        }
    }

    /// Default power cost (relative units, R2-ROUTE §5.2).
    pub const fn default_power_cost(self) -> u8 {
        match self {
            Self::Ble => 1,
            Self::Lora => 5,
            Self::Internet => 8,
            Self::Wifi => 10,
        }
    }

    /// Maximum R2-WIRE payload size for this transport.
    ///
    /// This is the worst-case (smallest) MTU.  Actual MTU may vary by
    /// conditions (e.g. LoRa SF12 limits payload to 51 bytes).
    pub const fn max_payload(self) -> usize {
        match self {
            Self::Ble => 200,        // BLE extended advertising / L2CAP CoC
            Self::Lora => 222,       // LoRa SF7/BW125 max
            Self::Wifi => 65535,     // UDP datagram
            Self::Internet => 65535, // TCP length-prefixed
        }
    }
}

// ---------------------------------------------------------------------------
// Transport state
// ---------------------------------------------------------------------------

/// Current state of a transport, reported upward to the routing layer
/// (R2-ROUTE §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// Transport is available for new and existing peers.
    Available,
    /// Available for existing peers only.
    ///
    /// Example: BLE pool full (R2-BLESCHED §3.3) — can still send to
    /// pooled peers but cannot connect to new ones.
    ExistingOnly,
    /// Temporarily unavailable.
    ///
    /// Example: LoRa duty cycle exhausted, WiFi not associated.
    Unavailable,
    /// Transport has failed and requires recovery.
    Failed,
}

// ---------------------------------------------------------------------------
// Link quality
// ---------------------------------------------------------------------------

/// Per-link quality metrics reported by a transport.
///
/// Maps to the `link_quality` and `rssi` fields in the neighbour table
/// (R2-ROUTE §2.2).
#[derive(Debug, Clone, Copy, Default)]
pub struct LinkQuality {
    /// General quality score \[0.0, 1.0\].
    pub quality: f32,
    /// RSSI in dBm (BLE, WiFi).  0 = unknown.
    pub rssi: i8,
    /// Signal-to-noise ratio in dB (LoRa).  0 = unknown.
    pub snr: i8,
    /// Round-trip latency in milliseconds (TCP/IP).  0 = unknown.
    pub latency_ms: u16,
}

// ---------------------------------------------------------------------------
// Send error
// ---------------------------------------------------------------------------

/// Error from a transport send operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Frame exceeds the transport's current MTU.
    PayloadTooLarge,
    /// Transport is currently unavailable (duty cycle, pool full, etc.).
    Unavailable,
    /// Target hive is not reachable on this transport.
    Unreachable,
    /// Transport-level I/O error.
    IoError,
}

// ---------------------------------------------------------------------------
// The Transport trait
// ---------------------------------------------------------------------------

/// A transport binding — the medium-agnostic interface between R2-WIRE
/// framing and a physical/logical medium.
///
/// ## What transports carry
///
/// R2-WIRE frames only: events, heartbeats, capabilities, GROUP_MGMT.
/// These are small (16–222 bytes), fire-and-forget.
///
/// ## What transports do NOT carry
///
/// Bulk data (firmware, files, chat messages, AI responses).  That is a
/// plugin concern — plugins use the connectivity that transports provide
/// but have their own protocols (R2-WIRE §1.1.1, R2-DEPLOY §4.7).
///
/// ## Implementor's Guide
///
/// - **BLE**: `r2-ble` crate (L2CAP CoC or GATT, hardware-dependent).
/// - **LoRa**: `r2-lora` crate (radio driver, hardware-dependent).
/// - **WiFi/UDP**: compose [`udp`](crate::udp) helpers with a socket.
/// - **TCP/IP**: compose [`tcp`](crate::tcp) helpers with a socket.
///
/// The trait is deliberately simple.  Connection management, pool
/// scheduling, duty cycle tracking, and radio configuration are internal
/// to each implementation.  The routing layer only asks "can you send
/// this?" and "what can you see?"
pub trait Transport {
    /// Which transport this is.
    fn id(&self) -> TransportId;

    /// The R2-WIRE format carried on this transport.
    fn wire_format(&self) -> WireFormat {
        self.id().wire_format()
    }

    /// Current transport state (R2-ROUTE §5.6).
    fn state(&self) -> TransportState;

    /// Maximum R2-WIRE frame size this transport can carry right now.
    ///
    /// May be less than [`TransportId::max_payload`] due to current
    /// conditions (e.g. LoRa SF12 → 51 bytes).
    fn current_mtu(&self) -> usize {
        self.id().max_payload()
    }

    /// Send an R2-WIRE frame to a target hive.
    ///
    /// `target` is the FNV-1a 32-bit hash of the destination hive's
    /// device ID.  `0x00000000` = broadcast.
    ///
    /// `frame` is a complete R2-WIRE message (compact or extended,
    /// matching [`wire_format()`](Transport::wire_format)).  The
    /// transport wraps it for the medium (length prefix, datagram
    /// boundary, radio frame) but MUST NOT alter the R2-WIRE bytes.
    fn send(&self, target: u32, frame: &[u8]) -> Result<(), SendError>;

    /// Link quality to a specific neighbour on this transport.
    ///
    /// Returns `None` if the hive is not reachable via this transport.
    fn link_quality(&self, hive_id: u32) -> Option<LinkQuality>;
}
