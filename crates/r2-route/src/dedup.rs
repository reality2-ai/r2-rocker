//! Message deduplication cache (SPEC.md §6).
//!
//! Fixed-capacity ring buffer of `(msg_id, source, timestamp)` tuples.
//! Entries expire after [`DEDUP_TTL_SECS`](crate::constants::DEDUP_TTL_SECS).

use crate::constants::DEDUP_TTL_SECS;

/// Fixed-capacity deduplication cache. `N` is the maximum number of tracked messages.
#[derive(Debug)]
pub struct DedupCache<const N: usize> {
    entries: [DedupEntry; N],
    cursor: usize,
}

impl<const N: usize> DedupCache<N> {
    /// Create an empty cache.
    pub const fn new() -> Self {
        const ENTRY: DedupEntry = DedupEntry::new();
        DedupCache {
            entries: [ENTRY; N],
            cursor: 0,
        }
    }

    /// Check and record a message. Returns `true` if already seen within the TTL window.
    pub fn is_duplicate(&mut self, now: u32, msg_id: u16, source: u16) -> bool {
        self.expire(now);
        for entry in &self.entries {
            if entry.used && entry.msg_id == msg_id && entry.source == source {
                return true;
            }
        }
        self.insert(now, msg_id, source);
        false
    }

    fn insert(&mut self, now: u32, msg_id: u16, source: u16) {
        let expires_at = now + DEDUP_TTL_SECS;
        self.entries[self.cursor] = DedupEntry {
            used: true,
            msg_id,
            source,
            expires_at,
        };
        self.cursor = (self.cursor + 1) % N;
    }

    fn expire(&mut self, now: u32) {
        for entry in &mut self.entries {
            if entry.used && entry.expires_at <= now {
                entry.used = false;
            }
        }
    }
}

impl<const N: usize> Default for DedupCache<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct DedupEntry {
    used: bool,
    msg_id: u16,
    source: u16,
    expires_at: u32,
}

impl DedupEntry {
    const fn new() -> Self {
        DedupEntry {
            used: false,
            msg_id: 0,
            source: 0,
            expires_at: 0,
        }
    }
}
