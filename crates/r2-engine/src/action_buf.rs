//! Fixed-capacity action buffer.
//!
//! Sentant event handlers push [`Action`]s here during `handle_event`.
//! The buffer has a compile-time maximum capacity — no heap allocation.

use crate::action::Action;

/// Maximum actions a single `handle_event` call can produce.
///
/// 16 is generous — most handlers produce 1–3 actions. The coordinator
/// sentant (START command) is the most complex at ~5 actions.
pub const MAX_ACTIONS: usize = 16;

/// Fixed-capacity buffer for actions produced by a sentant handler.
///
/// This is stack-allocated and reused between handler calls.
/// The engine clears it before each `handle_event` invocation.
pub struct ActionBuf {
    actions: [Option<Action>; MAX_ACTIONS],
    count: u8,
}

impl ActionBuf {
    /// Create an empty action buffer.
    pub const fn new() -> Self {
        // const fn can't use array init with non-Copy types in older Rust,
        // so we use a manual approach
        Self {
            actions: [
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ],
            count: 0,
        }
    }

    /// Push an action. Returns false if buffer is full.
    pub fn push(&mut self, action: Action) -> bool {
        let idx = self.count as usize;
        if idx >= MAX_ACTIONS {
            return false;
        }
        self.actions[idx] = Some(action);
        self.count += 1;
        true
    }

    /// Clear all actions (reuse the buffer).
    pub fn clear(&mut self) {
        for i in 0..self.count as usize {
            self.actions[i] = None;
        }
        self.count = 0;
    }

    /// Number of actions in the buffer.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Iterate over the actions.
    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.actions[..self.count as usize]
            .iter()
            .filter_map(|a| a.as_ref())
    }

    /// Drain actions — iterate and clear.
    pub fn drain(&mut self) -> ActionDrain<'_> {
        ActionDrain { buf: self, idx: 0 }
    }
}

/// Iterator that drains actions from the buffer.
pub struct ActionDrain<'a> {
    buf: &'a mut ActionBuf,
    idx: usize,
}

impl<'a> Iterator for ActionDrain<'a> {
    type Item = Action;

    fn next(&mut self) -> Option<Action> {
        if self.idx >= self.buf.count as usize {
            self.buf.count = 0;
            return None;
        }
        let action = self.buf.actions[self.idx].take();
        self.idx += 1;
        action
    }
}
