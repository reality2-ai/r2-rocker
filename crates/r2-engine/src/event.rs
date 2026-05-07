//! Event types for the R2 sentant engine.
//!
//! Events are the sole communication mechanism between sentants.
//! Each event has an FNV-1a hash identifying its type and a CBOR-encoded
//! payload. Events are transport-agnostic — the same event can arrive
//! via BLE L2CAP, WiFi TCP, or internal dispatch.

/// FNV-1a hash of an event name (e.g., `fnv::r2_hash("acceleration")`).
///
/// Using hashes instead of strings saves RAM on constrained devices and
/// matches the R2-WIRE compact format where event names are always hashed.
pub type EventHash = u32;

/// Identifies where an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSource {
    /// From a local sentant on this hive.
    Local(SentantId),
    /// From a remote hive (arrived via transport).
    /// The u32 is the first 4 bytes of the sender's RBID.
    Remote(u32),
    /// From a plugin (hardware interrupt, timer, etc.).
    Plugin(PluginId),
    /// From the platform (boot, OTA, shutdown, etc.).
    Platform,
}

/// Where an event should be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A specific local sentant.
    Sentant(SentantId),
    /// All local sentants subscribed to this event hash.
    Local,
    /// All sentants in the trust group (local + remote hives).
    TrustGroup,
    /// Reply to whoever sent the triggering event.
    Sender,
    /// Broadcast to all reachable hives (1 hop).
    Broadcast,
}

/// An event delivered to or emitted by a sentant.
///
/// The payload is borrowed — it references either a transport buffer
/// or a locally allocated CBOR encoding. For outbound events, sentants
/// build payloads into the [`ActionBuf`](crate::ActionBuf).
#[derive(Debug, Clone)]
pub struct Event<'a> {
    /// FNV-1a hash of the event name.
    pub hash: EventHash,
    /// CBOR-encoded payload (may be empty).
    pub payload: &'a [u8],
    /// Where this event came from.
    pub source: EventSource,
    /// R2-WIRE message ID (for reply correlation).
    pub msg_id: u16,
}

use super::sentant::SentantId;
use super::plugin::PluginId;
