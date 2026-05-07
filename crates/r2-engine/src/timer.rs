//! Timer registry for delayed sends (R2-SENTANT §3.1.5).
//!
//! At most one pending timer per (sentant, event_hash) pair. Setting a new
//! delay for the same pair replaces the pending timer. Timers do not survive
//! hive restart.
//!
//! The engine has no clock — the platform layer calls [`TimerRegistry::advance`]
//! with elapsed milliseconds, and expired entries are returned for dispatch.

extern crate alloc;
use alloc::vec::Vec;

use crate::action::PayloadBuf;
use crate::event::{EventHash, Target};
use crate::sentant::SentantId;

/// A pending delayed send.
#[derive(Clone)]
struct PendingTimer {
    /// Sentant that requested the delayed send.
    source_id: SentantId,
    /// Event hash to send when the timer fires.
    event_hash: EventHash,
    /// Where to deliver.
    target: Target,
    /// Payload to deliver.
    payload: PayloadBuf,
    /// Remaining milliseconds until fire.
    remaining_ms: u32,
}

/// Fired timer ready for dispatch.
pub struct FiredTimer {
    /// Sentant that requested the delayed send.
    pub source_id: SentantId,
    /// Event hash.
    pub event_hash: EventHash,
    /// Where to deliver.
    pub target: Target,
    /// Payload.
    pub payload: PayloadBuf,
}

/// Registry of pending delayed sends.
///
/// Replacement semantics: one timer per (sentant, event_hash) pair.
pub struct TimerRegistry {
    timers: Vec<PendingTimer>,
}

impl TimerRegistry {
    /// Create an empty timer registry.
    pub fn new() -> Self {
        Self {
            timers: Vec::with_capacity(8),
        }
    }

    /// Schedule a delayed send. Replaces any existing timer for the same
    /// (source_id, event_hash) pair.
    pub fn schedule(
        &mut self,
        source_id: SentantId,
        event_hash: EventHash,
        target: Target,
        payload: &[u8],
        delay_ms: u32,
    ) {
        // Replace existing timer for same (sentant, event_hash).
        if let Some(existing) = self
            .timers
            .iter_mut()
            .find(|t| t.source_id == source_id && t.event_hash == event_hash)
        {
            existing.target = target;
            existing.payload = PayloadBuf::from_slice(payload);
            existing.remaining_ms = delay_ms;
            return;
        }
        self.timers.push(PendingTimer {
            source_id,
            event_hash,
            target,
            payload: PayloadBuf::from_slice(payload),
            remaining_ms: delay_ms,
        });
    }

    /// Advance all timers by `elapsed_ms` milliseconds. Returns any that fired.
    pub fn advance(&mut self, elapsed_ms: u32) -> Vec<FiredTimer> {
        let mut fired = Vec::new();
        self.timers.retain_mut(|t| {
            if t.remaining_ms <= elapsed_ms {
                fired.push(FiredTimer {
                    source_id: t.source_id,
                    event_hash: t.event_hash,
                    target: t.target,
                    payload: t.payload.clone(),
                });
                false // remove from registry
            } else {
                t.remaining_ms -= elapsed_ms;
                true // keep
            }
        });
        fired
    }

    /// Number of pending timers.
    pub fn len(&self) -> usize {
        self.timers.len()
    }

    /// Returns `true` if no timers are pending.
    pub fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }
}
