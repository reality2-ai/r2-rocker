//! Compiled Notekeeper sentant.
//!
//! This module is what `r2-compile` would generate from
//! `r2-notekeeper/sentant/notekeeper.yaml`. It implements the
//! [`Sentant`] trait, producing identical behaviour to any other
//! R2 runtime interpreting the same YAML definition.
//!
//! ## Events handled
//!
//! | Event | Public | Description |
//! |-------|--------|-------------|
//! | `note.create` | yes | Create a new note |
//! | `note.update` | yes | Update an existing note |
//! | `note.delete` | yes | Soft-delete a note (tombstone) |
//! | `note.list` | yes | List all notes |
//! | `note.get` | yes | Get a single note by ID |
//! | `notekeeper-sync` | no | Handle incoming sync from plugin |
//!
//! ## Plugin
//!
//! Uses `notekeeper-sync` plugin (plugin ID 0) for encrypted
//! relay sync. Plugin invocation follows R2-PLUGIN §2.3 envelope.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use r2_engine::action::Action;
use r2_engine::action_buf::ActionBuf;
use r2_engine::event::{Event, EventHash, Target};
use r2_engine::sentant::{Sentant, StateId};

// Event hashes (FNV-1a, pre-computed from R2-FNV conformance vectors)
const NOTE_CREATE: EventHash = r2_fnv::fnv1a_32(b"note.create");
const NOTE_UPDATE: EventHash = r2_fnv::fnv1a_32(b"note.update");
const NOTE_DELETE: EventHash = r2_fnv::fnv1a_32(b"note.delete");
const NOTE_LIST: EventHash = r2_fnv::fnv1a_32(b"note.list");
const NOTE_GET: EventHash = r2_fnv::fnv1a_32(b"note.get");
const NOTE_CHANGED: EventHash = r2_fnv::fnv1a_32(b"note.changed");
const NOTE_LIST_RESULT: EventHash = r2_fnv::fnv1a_32(b"note.list.result");
const NOTE_GET_RESULT: EventHash = r2_fnv::fnv1a_32(b"note.get.result");
const SYNC_RESULT: EventHash = r2_fnv::fnv1a_32(b"notekeeper-sync");

// Class hash
const CLASS_HASH: EventHash = r2_fnv::fnv1a_32(b"ai.reality2.capability.notekeeper");

// Plugin ID for notekeeper-sync
const SYNC_PLUGIN_ID: u8 = 0;

// Plugin commands
const CMD_BROADCAST_EVENT: u8 = 0;

/// A single note.
#[derive(Clone, Debug)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub deleted: bool,
}

/// Compiled Notekeeper sentant state.
///
/// This is the `data` block from the YAML, plus the state machine position.
/// For notekeeper, the FSM is stateless (all-wildcard transitions), so
/// `state` is always 0.
pub struct NotekeeperSentant {
    notes: BTreeMap<String, Note>,
    event_sequence: u64,
    state: StateId,
}

impl NotekeeperSentant {
    /// Create a new notekeeper sentant with empty state.
    pub fn new() -> Self {
        Self {
            notes: BTreeMap::new(),
            event_sequence: 0,
            state: 0,
        }
    }

    /// Access notes (for persistence / UI rendering).
    pub fn notes(&self) -> &BTreeMap<String, Note> {
        &self.notes
    }

    /// Current event sequence number.
    pub fn event_sequence(&self) -> u64 {
        self.event_sequence
    }

    // ---- Action helpers ----

    fn emit_note_changed(&self, note_id: &str, actions: &mut ActionBuf) {
        // Include full note data so JS can update its view without querying
        // CBOR: {0: id, 1: title, 2: content, 3: updated_at, 4: deleted}
        let mut buf = [0u8; 256];
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        if let Some(note) = self.notes.get(note_id) {
            if enc.map(5).is_ok() {
                let _ = enc.kv(0, &r2_cbor::Value::Text(&note.id));
                let _ = enc.kv(1, &r2_cbor::Value::Text(&note.title));
                let _ = enc.kv(2, &r2_cbor::Value::Text(&note.content));
                let _ = enc.kv(3, &r2_cbor::Value::UInt(note.updated_at));
                let _ = enc.kv(4, &r2_cbor::Value::Bool(note.deleted));
            }
        } else {
            if enc.map(1).is_ok() {
                let _ = enc.kv(0, &r2_cbor::Value::Text(note_id));
            }
        }
        let payload = enc.as_bytes();
        // Target::Sender routes to outbound, where JS can observe it
        actions.push(Action::send(Target::Sender, NOTE_CHANGED, payload));
    }

    fn invoke_sync_broadcast(&self, op: u8, note: &Note, actions: &mut ActionBuf) {
        // Plugin invocation: R2-PLUGIN §2.3 envelope as CBOR
        // {0: op, 1: note_id, 2: timestamp, 3: title, 4: content}
        let mut buf = [0u8; 256];
        let mut enc = r2_cbor::Encoder::new(&mut buf);
        let fields = if op == 2 { 3 } else { 5 };
        if enc.map(fields).is_ok() {
            let _ = enc.kv(0, &r2_cbor::Value::UInt(op as u64));
            let _ = enc.kv(1, &r2_cbor::Value::Text(&note.id));
            let _ = enc.kv(2, &r2_cbor::Value::UInt(note.updated_at));
            if op != 2 {
                let _ = enc.kv(3, &r2_cbor::Value::Text(&note.title));
                let _ = enc.kv(4, &r2_cbor::Value::Text(&note.content));
            }
        }
        let payload = enc.as_bytes();
        actions.push(Action::plugin_call(SYNC_PLUGIN_ID, CMD_BROADCAST_EVENT, payload));
    }
}

impl Sentant for NotekeeperSentant {
    fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
        match event.hash {
            NOTE_CREATE => {
                // Decode params: {id, title, content, timestamp}
                if let Some((id, title, content, ts)) = decode_note_params(event.payload) {
                    let note = Note {
                        id: id.clone(),
                        title,
                        content,
                        created_at: ts,
                        updated_at: ts,
                        deleted: false,
                    };
                    self.invoke_sync_broadcast(0, &note, actions);
                    self.notes.insert(id.clone(), note);
                    self.event_sequence += 1;
                    self.emit_note_changed(&id, actions);
                }
            }

            NOTE_UPDATE => {
                if let Some((id, title, content, ts)) = decode_note_params(event.payload) {
                    if let Some(note) = self.notes.get_mut(&id) {
                        if !note.deleted {
                            note.title = title;
                            note.content = content;
                            note.updated_at = ts;
                            self.event_sequence += 1;
                            let note_clone = note.clone();
                            self.invoke_sync_broadcast(1, &note_clone, actions);
                            self.emit_note_changed(&id, actions);
                        }
                    }
                }
            }

            NOTE_DELETE => {
                if let Some((id, _, _, ts)) = decode_note_params(event.payload) {
                    if let Some(note) = self.notes.get_mut(&id) {
                        note.deleted = true;
                        note.updated_at = ts;
                        self.event_sequence += 1;
                        let note_clone = note.clone();
                        self.invoke_sync_broadcast(2, &note_clone, actions);
                        self.emit_note_changed(&id, actions);
                    }
                }
            }

            NOTE_LIST => {
                // Send all notes as response
                // Encode as CBOR map of notes
                let mut buf = [0u8; 256];
                let active: Vec<_> = self.notes.values().filter(|n| !n.deleted).collect();
                let mut enc = r2_cbor::Encoder::new(&mut buf);
                if enc.map(active.len()).is_ok() {
                    for note in &active {
                        let _ = enc.kv(0, &r2_cbor::Value::Text(&note.id));
                    }
                }
                actions.push(Action::send(Target::Sender, NOTE_LIST_RESULT, enc.as_bytes()));
                // TODO: this needs a richer serialisation - currently only sends IDs
            }

            NOTE_GET => {
                if let Some(id) = decode_id_param(event.payload) {
                    if let Some(note) = self.notes.get(&id) {
                        let mut buf = [0u8; 256];
                        let mut enc = r2_cbor::Encoder::new(&mut buf);
                        if enc.map(5).is_ok() {
                            let _ = enc.kv(0, &r2_cbor::Value::Text(&note.id));
                            let _ = enc.kv(1, &r2_cbor::Value::Text(&note.title));
                            let _ = enc.kv(2, &r2_cbor::Value::Text(&note.content));
                            let _ = enc.kv(3, &r2_cbor::Value::UInt(note.updated_at));
                            let _ = enc.kv(4, &r2_cbor::Value::Bool(note.deleted));
                        }
                        actions.push(Action::send(Target::Sender, NOTE_GET_RESULT, enc.as_bytes()));
                    }
                }
            }

            SYNC_RESULT => {
                // Plugin result: R2-PLUGIN §2.4 envelope
                // Expecting: {status: "ok", command: "incoming", data: {op, note_id, title, content, timestamp}}
                if let Some(sync) = decode_sync_result(event.payload) {
                    // LWW merge
                    let dominated = self.notes.get(&sync.note_id)
                        .map(|n| sync.timestamp >= n.updated_at)
                        .unwrap_or(true);

                    if dominated {
                        if sync.op == 2 {
                            // Delete
                            if let Some(note) = self.notes.get_mut(&sync.note_id) {
                                note.deleted = true;
                                note.updated_at = sync.timestamp;
                            }
                        } else {
                            // Create or update
                            let note = Note {
                                id: sync.note_id.clone(),
                                title: sync.title,
                                content: sync.content,
                                created_at: sync.timestamp,
                                updated_at: sync.timestamp,
                                deleted: false,
                            };
                            self.notes.insert(sync.note_id.clone(), note);
                        }
                        self.emit_note_changed(&sync.note_id, actions);
                    }
                }
            }

            _ => {} // Unknown events silently ignored (R2-SENTANT conformance)
        }
    }

    fn state(&self) -> StateId {
        self.state
    }

    fn class_hash(&self) -> u32 {
        CLASS_HASH
    }

    fn name(&self) -> &str {
        "Notekeeper"
    }

    fn subscriptions(&self) -> &[u32] {
        &[NOTE_CREATE, NOTE_UPDATE, NOTE_DELETE, NOTE_LIST, NOTE_GET, SYNC_RESULT]
    }
}

// ---- CBOR Payload Decoders ----

/// Decode note params: {0: id, 1: title, 2: content, 3: timestamp}
fn decode_note_params(payload: &[u8]) -> Option<(String, String, String, u64)> {
    let mut dec = r2_cbor::Decoder::new(payload);
    let map_len = match dec.next().ok()? {
        r2_cbor::Item::Map(n) => n,
        _ => return None,
    };

    let mut id = String::new();
    let mut title = String::new();
    let mut content = String::new();
    let mut timestamp = 0u64;

    for _ in 0..map_len {
        let key = match dec.next().ok()? {
            r2_cbor::Item::UInt(k) => k,
            _ => return None,
        };
        match key {
            0 => {
                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                    id = String::from(core::str::from_utf8(s).ok()?);
                }
            }
            1 => {
                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                    title = String::from(core::str::from_utf8(s).ok()?);
                }
            }
            2 => {
                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                    content = String::from(core::str::from_utf8(s).ok()?);
                }
            }
            3 => {
                if let r2_cbor::Item::UInt(v) = dec.next().ok()? {
                    timestamp = v;
                }
            }
            _ => { let _ = dec.next(); } // skip unknown keys
        }
    }

    if id.is_empty() { return None; }
    Some((id, title, content, timestamp))
}

/// Decode just the id param: {0: id}
fn decode_id_param(payload: &[u8]) -> Option<String> {
    let mut dec = r2_cbor::Decoder::new(payload);
    let _ = match dec.next().ok()? {
        r2_cbor::Item::Map(_) => {},
        _ => return None,
    };
    let _ = dec.next().ok()?; // key 0
    if let r2_cbor::Item::Text(s) = dec.next().ok()? {
        return Some(String::from(core::str::from_utf8(s).ok()?));
    }
    None
}

/// Decoded sync result from plugin.
struct SyncData {
    op: u8,
    note_id: String,
    title: String,
    content: String,
    timestamp: u64,
}

/// Decode plugin result: R2-PLUGIN §2.4 envelope
/// {0: status, 1: command, 2: {op, note_id, title, content, timestamp}}
fn decode_sync_result(payload: &[u8]) -> Option<SyncData> {
    let mut dec = r2_cbor::Decoder::new(payload);
    let map_len = match dec.next().ok()? {
        r2_cbor::Item::Map(n) => n,
        _ => return None,
    };

    let mut status_ok = false;
    let mut is_incoming = false;
    let mut data: Option<SyncData> = None;

    for _ in 0..map_len {
        let key = match dec.next().ok()? {
            r2_cbor::Item::UInt(k) => k,
            _ => return None,
        };
        match key {
            0 => {
                // status
                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                    status_ok = s == b"ok";
                }
            }
            1 => {
                // command
                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                    is_incoming = s == b"incoming";
                }
            }
            2 => {
                // data map: {0: op, 1: note_id, 2: title, 3: content, 4: timestamp}
                if let r2_cbor::Item::Map(n) = dec.next().ok()? {
                    let mut op = 0u8;
                    let mut note_id = String::new();
                    let mut title = String::new();
                    let mut content = String::new();
                    let mut timestamp = 0u64;

                    for _ in 0..n {
                        let dk = match dec.next().ok()? {
                            r2_cbor::Item::UInt(k) => k,
                            _ => return None,
                        };
                        match dk {
                            0 => {
                                if let r2_cbor::Item::UInt(v) = dec.next().ok()? { op = v as u8; }
                            }
                            1 => {
                                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                                    note_id = String::from(core::str::from_utf8(s).ok()?);
                                }
                            }
                            2 => {
                                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                                    title = String::from(core::str::from_utf8(s).ok()?);
                                }
                            }
                            3 => {
                                if let r2_cbor::Item::Text(s) = dec.next().ok()? {
                                    content = String::from(core::str::from_utf8(s).ok()?);
                                }
                            }
                            4 => {
                                if let r2_cbor::Item::UInt(v) = dec.next().ok()? { timestamp = v; }
                            }
                            _ => { let _ = dec.next(); }
                        }
                    }
                    data = Some(SyncData { op, note_id, title, content, timestamp });
                }
            }
            _ => { let _ = dec.next(); }
        }
    }

    if status_ok && is_incoming { data } else { None }
}
