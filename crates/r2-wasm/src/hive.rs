//! R2 Hive - the browser-side sentant runtime.
//!
//! Wraps the r2-engine EventBus with the Notekeeper sentant and
//! sync plugin, exposing it to JavaScript via wasm-bindgen.
//!
//! JavaScript sends events in, the sentant processes them, and
//! JavaScript reads the results out. The sentant manages all state;
//! JavaScript is pure UI rendering.

use alloc::vec::Vec;

use wasm_bindgen::prelude::*;

use r2_engine::bus::EventBus;
use r2_engine::queue::QueuedEvent;

use crate::notekeeper::NotekeeperSentant;
use crate::sync_plugin::NotekeeperSyncPlugin;

/// The R2 Hive running in the browser.
///
/// Contains the EventBus with the Notekeeper sentant and sync plugin.
/// JavaScript interacts with it by sending events and polling for
/// outbound actions.
#[wasm_bindgen]
pub struct R2Hive {
    bus: EventBus,
}

#[wasm_bindgen]
impl R2Hive {
    /// Create a new hive with the Notekeeper sentant and sync plugin.
    #[wasm_bindgen(constructor)]
    pub fn new() -> R2Hive {
        let mut bus = EventBus::new();

        // Register plugin first (ID 0)
        let plugin = NotekeeperSyncPlugin::new(0);
        bus.register_plugin(Box::new(plugin));

        // Register sentant (ID 0)
        let sentant = NotekeeperSentant::new();
        bus.register_sentant(Box::new(sentant));

        // Initialise
        bus.init_all();

        R2Hive { bus }
    }

    /// Send an event to the sentant.
    ///
    /// `event_hash` is the FNV-1a hash of the event name.
    /// `payload` is CBOR-encoded event parameters.
    ///
    /// After calling this, check `drain_outbound()` for events the
    /// sentant wants to send (sync, notifications, etc).
    pub fn send_event(&mut self, event_hash: u32, payload: &[u8]) {
        let event = QueuedEvent::new(event_hash, 0xFF, false, 0, payload);
        self.bus.enqueue(event);
        self.bus.tick();
    }

    /// Push incoming sync data from another device (already decrypted).
    ///
    /// `payload` is the CBOR-encoded R2-PLUGIN result envelope.
    pub fn push_sync_inbound(&mut self, payload: &[u8]) {
        let sync_hash = r2_fnv::fnv1a_32(b"notekeeper-sync");
        let event = QueuedEvent::new(sync_hash, 0xFE, false, 0, payload);
        self.bus.enqueue(event);
        self.bus.tick();
    }

    /// Process one tick of the engine.
    pub fn tick(&mut self) {
        self.bus.poll_plugins();
        self.bus.tick();
    }

    /// Drain all outbound events (events the sentant wants to send externally).
    ///
    /// Returns a JSON array of events: [{"hash":N,"payload":"hex"}, ...]
    /// JavaScript processes these (encrypt + relay, UI update, etc).
    pub fn drain_outbound(&mut self) -> String {
        let events = self.bus.drain_outbound();
        if events.is_empty() {
            return alloc::string::String::from("[]");
        }

        let mut json = alloc::string::String::from("[");
        for (i, evt) in events.iter().enumerate() {
            if i > 0 { json.push(','); }
            json.push_str(&alloc::format!(
                "{{\"hash\":{},\"payload\":\"{}\"}}",
                evt.hash,
                hex_encode(evt.payload())
            ));
        }
        json.push(']');
        json
    }
}

fn hex_encode(bytes: &[u8]) -> alloc::string::String {
    bytes.iter().map(|b| alloc::format!("{:02x}", b)).collect()
}
