//! Wire format selection by transport context.
//!
//! Per R2-WIRE §4.3.5, the wire format (compact vs extended) is determined
//! by transport context, not by inspecting the message.

/// The R2-WIRE format expected on a given transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// Compact format — used on BLE and LoRa.
    ///
    /// 12-byte header, 16-bit msg_id, truncated HMAC (8 bytes).
    Compact,

    /// Extended format — used on WiFi/IP, TCP, WebSocket.
    ///
    /// 22-byte header, 32-bit msg_id, full HMAC (32 bytes).
    Extended,
}

impl WireFormat {
    /// Minimum header size for this format.
    pub const fn header_size(self) -> usize {
        match self {
            Self::Compact => 12,
            Self::Extended => 22,
        }
    }

    /// HMAC tag size (if present) for this format.
    pub const fn hmac_tag_size(self) -> usize {
        match self {
            Self::Compact => 8,
            Self::Extended => 32,
        }
    }

    /// Select format by transport name.
    ///
    /// Returns [`Compact`](WireFormat::Compact) for `"ble"` and `"lora"`,
    /// [`Extended`](WireFormat::Extended) for everything else.
    pub fn for_transport(transport: &str) -> Self {
        match transport {
            "ble" | "lora" => Self::Compact,
            _ => Self::Extended,
        }
    }
}
