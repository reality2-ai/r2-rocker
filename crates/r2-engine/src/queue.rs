//! Event queue — fixed-capacity ring buffer for pending events.
//!
//! Events arrive from transports, plugins, and inter-sentant sends.
//! The engine dequeues and dispatches them one at a time.
//!
//! The queue stores **owned** event data (hash + payload bytes) because
//! transport buffers may be reused before the event is processed.

use crate::event::EventHash;

/// Maximum events that can be queued.
///
/// 32 events is enough for the rocker rig's batch-at-decision-rate model.
/// At 62 batches/sec and a 20ms main loop, we'd accumulate ~1–2 events
/// per iteration. The extra headroom handles bursts.
pub const DEFAULT_QUEUE_CAPACITY: usize = 32;

/// Maximum payload bytes per queued event.
pub const MAX_QUEUED_PAYLOAD: usize = 256;

/// A queued event (owned data).
#[derive(Clone)]
pub struct QueuedEvent {
    /// FNV hash of the event name.
    pub hash: EventHash,
    /// Source sentant ID (or 0xFF for external).
    pub source_id: u8,
    /// Whether source is remote (transport) vs local.
    pub remote: bool,
    /// Remote source RBID prefix (if remote).
    pub remote_rbid: u32,
    /// R2-WIRE message ID.
    pub msg_id: u16,
    /// CBOR payload.
    payload: [u8; MAX_QUEUED_PAYLOAD],
    payload_len: u16,
}

impl QueuedEvent {
    /// Create a new queued event.
    pub fn new(hash: EventHash, source_id: u8, remote: bool, msg_id: u16, payload: &[u8]) -> Self {
        let mut buf = [0u8; MAX_QUEUED_PAYLOAD];
        let len = payload.len().min(MAX_QUEUED_PAYLOAD);
        buf[..len].copy_from_slice(&payload[..len]);
        Self {
            hash,
            source_id,
            remote,
            remote_rbid: 0,
            msg_id,
            payload: buf,
            payload_len: len as u16,
        }
    }

    /// Get the payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len as usize]
    }
}

/// Fixed-capacity ring buffer for events.
pub struct EventQueue<const N: usize = DEFAULT_QUEUE_CAPACITY> {
    events: [Option<QueuedEvent>; N],
    head: usize,
    tail: usize,
    count: usize,
}

impl<const N: usize> EventQueue<N> {
    /// Create a new empty queue.
    ///
    /// Note: due to const generics limitations, this uses a runtime init.
    /// For compile-time init, use `EventQueue::default()` or the DEFAULT_QUEUE_CAPACITY variant.
    pub fn new() -> Self {
        Self {
            events: core::array::from_fn(|_| None),
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Enqueue an event. Returns false if queue is full (event dropped).
    pub fn push(&mut self, event: QueuedEvent) -> bool {
        if self.count >= N {
            return false;
        }
        self.events[self.tail] = Some(event);
        self.tail = (self.tail + 1) % N;
        self.count += 1;
        true
    }

    /// Dequeue the next event.
    pub fn pop(&mut self) -> Option<QueuedEvent> {
        if self.count == 0 {
            return None;
        }
        let event = self.events[self.head].take();
        self.head = (self.head + 1) % N;
        self.count -= 1;
        event
    }

    /// Number of events in the queue.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether the queue is full.
    pub fn is_full(&self) -> bool {
        self.count >= N
    }

    /// Remaining capacity.
    pub fn remaining(&self) -> usize {
        N - self.count
    }

    /// Clear all events.
    pub fn clear(&mut self) {
        while self.pop().is_some() {}
    }
}
