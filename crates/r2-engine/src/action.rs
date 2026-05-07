//! Actions — what sentants produce in response to events.
//!
//! Sentants don't perform I/O directly. Instead, they push [`Action`]s
//! into an [`ActionBuf`](crate::ActionBuf), and the engine executes them.
//! This keeps sentant logic pure, testable, and deterministic.

use crate::event::{EventHash, Target};
use crate::plugin::PluginId;
use crate::sentant::StateId;

/// Maximum payload size for an action (bytes).
///
/// This bounds the CBOR payload that a sentant can attach to a Send action.
/// 256 bytes covers most R2 events. Larger payloads (e.g., firmware chunks)
/// bypass the sentant engine entirely (plugin-to-transport direct path).
pub const MAX_ACTION_PAYLOAD: usize = 256;

/// An action produced by a sentant's event handler.
///
/// Actions are value types with inline payload storage (no heap allocation).
/// The engine processes them in order after `handle_event` returns.
#[derive(Debug, Clone)]
pub enum Action {
    /// Send an event to a target.
    Send {
        /// Where to deliver.
        target: Target,
        /// FNV hash of the event name.
        event_hash: EventHash,
        /// CBOR-encoded payload (inline, up to [`MAX_ACTION_PAYLOAD`] bytes).
        payload: PayloadBuf,
    },

    /// Transition to a new state.
    ///
    /// Applied immediately before the next action is processed.
    /// Multiple transitions in one handler: last one wins.
    Transition(StateId),

    /// Call a plugin function.
    PluginCall {
        /// Which plugin to invoke.
        plugin_id: PluginId,
        /// Plugin-specific command byte.
        command: u8,
        /// Command data (inline).
        data: PayloadBuf,
    },

    /// Schedule an event to be sent after a delay.
    ///
    /// One timer per (sentant, event_hash) pair — setting a new delayed
    /// send for the same event_hash replaces the pending one (R2-SENTANT
    /// replacement semantics).
    DelayedSend {
        /// Delay in milliseconds.
        delay_ms: u32,
        /// Where to deliver.
        target: Target,
        /// FNV hash of the event name.
        event_hash: EventHash,
        /// CBOR-encoded payload (inline).
        payload: PayloadBuf,
    },

    /// Log a message (compiled to UART/RTT on MCU, stdout on Linux).
    Log {
        /// Log level (0=error, 1=warn, 2=info, 3=debug).
        level: u8,
        /// Message bytes (UTF-8, inline).
        message: PayloadBuf,
    },
}

/// Fixed-capacity inline payload buffer.
///
/// Avoids heap allocation for event payloads. On constrained targets,
/// this is the only option. On `alloc` targets, large payloads can
/// use the `Vec`-backed variant.
#[derive(Clone)]
pub struct PayloadBuf {
    buf: [u8; MAX_ACTION_PAYLOAD],
    len: u16,
}

impl PayloadBuf {
    /// Create an empty payload.
    pub const fn empty() -> Self {
        Self {
            buf: [0u8; MAX_ACTION_PAYLOAD],
            len: 0,
        }
    }

    /// Create a payload from a byte slice. Truncates if too long.
    pub fn from_slice(data: &[u8]) -> Self {
        let mut buf = [0u8; MAX_ACTION_PAYLOAD];
        let len = data.len().min(MAX_ACTION_PAYLOAD);
        buf[..len].copy_from_slice(&data[..len]);
        Self {
            buf,
            len: len as u16,
        }
    }

    /// Get the payload bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    /// Payload length in bytes.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for PayloadBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PayloadBuf({} bytes)", self.len)
    }
}

// Convenience constructors for Action
impl Action {
    /// Create a Send action with no payload.
    pub fn send_empty(target: Target, event_hash: EventHash) -> Self {
        Self::Send {
            target,
            event_hash,
            payload: PayloadBuf::empty(),
        }
    }

    /// Create a Send action with a CBOR payload.
    pub fn send(target: Target, event_hash: EventHash, payload: &[u8]) -> Self {
        Self::Send {
            target,
            event_hash,
            payload: PayloadBuf::from_slice(payload),
        }
    }

    /// Create a state transition.
    pub fn transition(state: StateId) -> Self {
        Self::Transition(state)
    }

    /// Create a plugin call.
    pub fn plugin_call(plugin_id: PluginId, command: u8, data: &[u8]) -> Self {
        Self::PluginCall {
            plugin_id,
            command,
            data: PayloadBuf::from_slice(data),
        }
    }

    /// Create a delayed send.
    pub fn delayed_send(
        delay_ms: u32,
        target: Target,
        event_hash: EventHash,
        payload: &[u8],
    ) -> Self {
        Self::DelayedSend {
            delay_ms,
            target,
            event_hash,
            payload: PayloadBuf::from_slice(payload),
        }
    }
}
