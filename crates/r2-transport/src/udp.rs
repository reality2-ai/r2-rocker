//! UDP datagram transport binding (R2-WIRE §13.3 / R2-WIFI §4).
//!
//! Each UDP datagram on port 21042 contains exactly one R2-WIRE message.
//! No additional framing — UDP datagrams are self-delimiting.
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │          UDP Header (8 bytes)       │
//! ├─────────────────────────────────────┤
//! │     R2-WIRE Message (variable)      │
//! └─────────────────────────────────────┘
//! ```
//!
//! This module provides validation helpers.  The actual socket I/O is
//! handled by the application (tokio, embassy, etc.).

use crate::format::WireFormat;

/// Validate a received UDP datagram as an R2-WIRE message.
///
/// Checks that the datagram is at least as large as the minimum header
/// for the expected format, and that byte 0 contains a valid version.
///
/// Returns `Ok(())` on success, `Err(reason)` on failure.
pub fn validate_udp_datagram(data: &[u8], format: WireFormat) -> Result<(), &'static str> {
    let min_size = format.header_size();
    if data.len() < min_size {
        return Err("datagram too short for R2-WIRE header");
    }

    // Check version (bits 7:6 of byte 0 must be 0b00).
    let version = (data[0] >> 6) & 0x03;
    if version != 0 {
        return Err("unsupported R2-WIRE version");
    }

    Ok(())
}

/// Extract the message type from the first byte of an R2-WIRE message.
///
/// Returns the 3-bit message type (0–7).
pub fn message_type(data: &[u8]) -> Option<u8> {
    if data.is_empty() {
        return None;
    }
    Some((data[0] >> 3) & 0x07)
}

/// R2-WIRE message type constants (R2-WIRE §3).
pub mod msg_type {
    /// Fire-and-forget event directed to a hive/trust group or broadcast.
    pub const EVENT: u8 = 0;
    /// Response routed back via @sender breadcrumb trail.
    pub const REPLY: u8 = 2;
    /// Capability advertisement.
    pub const CAPABILITY: u8 = 3;
    /// Trust group management (join/leave/cert exchange).
    pub const GROUP_MGMT: u8 = 4;
    /// Keepalive, routing table update, directory sync.
    pub const HEARTBEAT: u8 = 5;
}

/// The R2 multicast group for WiFi deployments with >10 devices (R2-WIFI §4.5).
///
/// `239.82.50.0` — "R2" = 82.50 in ASCII.
pub const R2_MULTICAST_GROUP: [u8; 4] = [239, 82, 50, 0];
