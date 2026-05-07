//! Notekeeper sync plugin - bridges sentant actions to relay transport.
//!
//! This plugin implements the R2-PLUGIN `Plugin` trait for the browser
//! environment. It handles:
//!
//! - `broadcast_event` command: encodes sync data, signals JS to encrypt and send
//! - `poll`: returns incoming sync data that JS has received and decrypted
//!
//! The actual WebSocket communication and DEK encryption happen in JavaScript.
//! This plugin acts as the R2-PLUGIN conformant bridge between the sentant
//! engine and the browser transport layer.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use r2_engine::plugin::{Plugin, PluginCommand, PluginError, PluginId, PluginResponse, PluginResult};

/// Plugin commands (must match notekeeper-sync-plugin.yaml)
pub const CMD_BROADCAST_EVENT: u8 = 0;

/// The notekeeper sync plugin.
///
/// Outbound: sentant pushes `PluginCall` -> plugin queues the data -> JS polls and sends
/// Inbound: JS receives from relay -> pushes into plugin -> sentant receives via `poll`
pub struct NotekeeperSyncPlugin {
    id: PluginId,
    /// Queue of outbound sync payloads (sentant -> JS -> relay)
    outbound: VecDeque<Vec<u8>>,
    /// Queue of inbound sync payloads (relay -> JS -> sentant)
    /// Each entry is a CBOR-encoded R2-PLUGIN result envelope
    inbound: VecDeque<(u32, Vec<u8>)>,
    /// Plugin health: is the relay connected?
    relay_connected: bool,
}

impl NotekeeperSyncPlugin {
    pub fn new(id: PluginId) -> Self {
        Self {
            id,
            outbound: VecDeque::new(),
            inbound: VecDeque::new(),
            relay_connected: false,
        }
    }

    /// JS calls this to get the next outbound sync payload to encrypt and send.
    pub fn drain_outbound(&mut self) -> Option<Vec<u8>> {
        self.outbound.pop_front()
    }

    /// JS calls this to push a received+decrypted sync payload from relay.
    /// The payload should be CBOR-encoded plugin result envelope:
    /// {0: "ok", 1: "incoming", 2: {op, note_id, title, content, timestamp}}
    pub fn push_inbound(&mut self, event_hash: u32, payload: Vec<u8>) {
        self.inbound.push_back((event_hash, payload));
    }

    /// JS reports relay connection state.
    pub fn set_relay_connected(&mut self, connected: bool) {
        self.relay_connected = connected;
    }

    /// Check if there are outbound items waiting.
    pub fn has_outbound(&self) -> bool {
        !self.outbound.is_empty()
    }
}

impl Plugin for NotekeeperSyncPlugin {
    fn execute(&mut self, command: PluginCommand, data: &[u8]) -> PluginResult {
        match command {
            CMD_BROADCAST_EVENT => {
                if !self.relay_connected {
                    return PluginResult::Error(PluginError::new(1, "relay_disconnected"));
                }
                // Queue the sync data for JS to pick up, encrypt, and send
                self.outbound.push_back(data.to_vec());
                PluginResult::Ok(PluginResponse::empty())
            }
            _ => PluginResult::Error(PluginError::new(255, "unknown_command")),
        }
    }

    fn name(&self) -> &str {
        "notekeeper-sync"
    }

    fn id(&self) -> PluginId {
        self.id
    }

    fn init(&mut self) -> PluginResult {
        // Plugin started - JS will connect to relay
        PluginResult::Ok(PluginResponse::empty())
    }

    fn poll(&mut self) -> Option<(u32, &[u8])> {
        // Return next inbound sync event for the sentant
        if let Some((hash, ref payload)) = self.inbound.front() {
            Some((*hash, payload))
        } else {
            None
        }
    }
}

/// After the engine processes the poll result, call this to advance the queue.
impl NotekeeperSyncPlugin {
    pub fn advance_inbound(&mut self) {
        self.inbound.pop_front();
    }
}
