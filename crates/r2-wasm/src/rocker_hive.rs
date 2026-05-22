//! `R2RockerHive` — wasm-bindgen entry point for the rocker webapp.
//! Sibling of `R2Hive` (`hive.rs`); both share the same r2-engine
//! EventBus shape but register different sentants.
//!
//! Track D of the R2-conformance roadmap (see
//! `audits/2026-05-23-architectural-gaps.md` Finding C). The rocker
//! webapp constructs a `R2RockerHive` at boot, forwards every R2-WIRE
//! event the dashboard publishes on `/ws/raw` into it via
//! `send_event(hash, payload)`, and reads the resulting per-sensor
//! state via `peek_state()`. JS continues to own UI rendering for
//! v0.1 (this slice is observation-only); a future slice transitions
//! the UI to render from `peek_state()` directly, then Tracks B+C
//! migrate the wire shape itself.
//!
//! ## Why both `R2Hive` and `R2RockerHive` in one crate?
//!
//! Each browser deployment instantiates exactly one hive (a tab is a
//! single TG-member device). The crate ships both wasm-bindgen
//! constructors so the rocker webapp picks `R2RockerHive::new()` and
//! the notekeeper webapp picks `R2Hive::new()`. Sharing the WASM
//! bundle is fine — the cost is negligible (a few kB of sentant code
//! per variant) and avoids parallel build/deploy plumbing.

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;

use wasm_bindgen::prelude::*;

use r2_engine::bus::EventBus;
use r2_engine::queue::QueuedEvent;

use crate::rocker_viewer::{DashboardViewerSentant, Inner};

#[wasm_bindgen]
pub struct R2RockerHive {
    bus: EventBus,
    /// Clone of the `Rc<RefCell<Inner>>` the sentant updates on each
    /// event. `peek_state()` borrows it for JSON serialisation.
    sentant_state: Rc<RefCell<Inner>>,
}

#[wasm_bindgen]
impl R2RockerHive {
    /// Construct the rocker hive with the DashboardViewerSentant
    /// registered on the EventBus. Called once from `bootstrapHive`
    /// in `webapp/index.html`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> R2RockerHive {
        let (sentant, sentant_state) = DashboardViewerSentant::new();
        let mut bus = EventBus::new();
        bus.register_sentant(sentant);
        bus.init_all();
        R2RockerHive { bus, sentant_state }
    }

    /// Forward an R2-WIRE event into the hive. JavaScript pulls the
    /// event hash + CBOR payload out of the binary `/ws/raw` frame
    /// (via `decode_compact_frame`) and calls this. Same shape as
    /// `R2Hive::send_event`.
    pub fn send_event(&mut self, event_hash: u32, payload: &[u8]) {
        let event = QueuedEvent::new(event_hash, 0xFF, false, 0, payload);
        self.bus.enqueue(event);
        self.bus.tick();
    }

    /// Drive one tick of the engine. Intended to be called from
    /// `requestAnimationFrame` in the webapp once the engine grows
    /// timer-driven behaviour. For Track D's first slice the sentant
    /// is purely event-reactive — calling tick is a no-op but the API
    /// is there for symmetry with `R2Hive`.
    pub fn tick(&mut self) {
        self.bus.poll_plugins();
        self.bus.tick();
    }

    /// Snapshot the per-sensor state table as a JSON string. UI code
    /// can `JSON.parse(hive.peek_state())` to consume.
    ///
    /// Shape:
    ///   {
    ///     "event_count": N,
    ///     "sensors": [
    ///       {
    ///         "device_pk": "<64 hex>",
    ///         "hostname": "...",        // optional
    ///         "fw_ver": "...",           // optional
    ///         "has_cert": true|false,
    ///         "last_seq": N,
    ///         "last_ts_ms": N,
    ///         "battery_pct": 0..100,    // optional
    ///         "fsm_state": 0..9,        // optional
    ///         "capture_state": 0|1|2,   // optional
    ///         "capture_file": "...",    // optional
    ///         "sample_count": N
    ///       },
    ///       ...
    ///     ]
    ///   }
    pub fn peek_state(&self) -> String {
        match self.sentant_state.try_borrow() {
            Ok(inner) => inner.to_json(),
            Err(_) => String::from("{\"error\":\"sentant state borrow conflict\"}"),
        }
    }
}
