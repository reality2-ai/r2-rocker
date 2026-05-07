//! Path confidence table with reinforcement learning (SPEC.md §2.2).
//!
//! Maps `(destination, next_hop)` pairs to confidence values ∈ [0, 1].
//! Confidence grows via positive/indirect reinforcement and decays exponentially.

use crate::constants::{PATH_DECAY_MU, PATH_EVICT_THRESHOLD, PATH_INDIRECT_ALPHA, PATH_POS_ALPHA};

/// A single path entry: "to reach `destination`, try `next_hop` with confidence `c`."
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathConfidenceEntry {
    /// FNV hash of the destination hive.
    pub destination: u32,
    /// FNV hash of the next-hop neighbour.
    pub next_hop: u32,
    /// Confidence ∈ [0, 1] that this path works.
    pub confidence: f32,
    /// Timestamp of last update (seconds).
    pub last_updated: u32,
    /// Number of positive observations.
    pub sample_count: u16,
}

impl PathConfidenceEntry {
    /// Sentinel empty entry.
    pub const EMPTY: Self = PathConfidenceEntry {
        destination: 0,
        next_hop: 0,
        confidence: 0.0,
        last_updated: 0,
        sample_count: 0,
    };
}

/// Fixed-capacity path table. `N` is the maximum tracked `(destination, next_hop)` pairs.
#[derive(Debug)]
pub struct PathTable<const N: usize> {
    slots: [Slot; N],
    len: usize,
}

impl<const N: usize> PathTable<N> {
    /// Create an empty table.
    pub const fn new() -> Self {
        const SLOT: Slot = Slot::new();
        PathTable {
            slots: [SLOT; N],
            len: 0,
        }
    }

    /// Iterate over active entries.
    pub fn iter(&self) -> impl Iterator<Item = &PathConfidenceEntry> {
        self.slots.iter().filter_map(|slot| slot.as_ref())
    }

    /// Record a successful delivery via `next_hop` to `destination` (SPEC.md §2.2).
    ///
    /// `c' = c + α·(1-c)`, α = 0.2.
    pub fn record_positive(&mut self, destination: u32, next_hop: u32, now: u32) {
        let entry = self.get_or_insert(destination, next_hop, now);
        entry.confidence = entry.confidence + PATH_POS_ALPHA * (1.0 - entry.confidence);
        entry.last_updated = now;
        entry.sample_count = entry.sample_count.saturating_add(1);
    }

    /// Record an indirect observation (overheard relay) — weaker reinforcement.
    ///
    /// `c' = c + α·(1-c)`, α = 0.05.
    pub fn record_indirect(&mut self, destination: u32, next_hop: u32, now: u32) {
        let entry = self.get_or_insert(destination, next_hop, now);
        entry.confidence = entry.confidence + PATH_INDIRECT_ALPHA * (1.0 - entry.confidence);
        entry.last_updated = now;
    }

    /// Seed a path entry with an explicit confidence value.
    pub fn seed(&mut self, destination: u32, next_hop: u32, now: u32, confidence: f32) {
        let entry = self.get_or_insert(destination, next_hop, now);
        entry.confidence = confidence;
        entry.last_updated = now;
        entry.sample_count = 1;
    }

    /// Apply exponential decay to all entries and evict those below threshold.
    pub fn decay(&mut self, now: u32) {
        for slot in &mut self.slots {
            if !slot.used {
                continue;
            }
            if now <= slot.entry.last_updated {
                continue;
            }
            let elapsed = now - slot.entry.last_updated;
            let decay = libm::expf(-PATH_DECAY_MU * (elapsed as f32));
            slot.entry.confidence *= decay;
            slot.entry.last_updated = now;
            if slot.entry.confidence < PATH_EVICT_THRESHOLD {
                slot.used = false;
                self.len -= 1;
            }
        }
    }

    /// Find the highest-confidence path to `destination`.
    pub fn best_for(&self, destination: u32) -> Option<PathConfidenceEntry> {
        let mut best: Option<PathConfidenceEntry> = None;
        for entry in self.iter().filter(|e| e.destination == destination) {
            match best {
                Some(current) if current.confidence >= entry.confidence => continue,
                _ => best = Some(*entry),
            }
        }
        best
    }

    fn get_or_insert(
        &mut self,
        destination: u32,
        next_hop: u32,
        now: u32,
    ) -> &mut PathConfidenceEntry {
        if let Some(idx) = self.entry_index(destination, next_hop) {
            return self.slots[idx].as_mut().unwrap();
        }

        if self.len == N {
            self.evict_oldest();
        }

        let idx = self
            .slots
            .iter()
            .position(|slot| !slot.used)
            .expect("PathTable capacity mis-tracked");
        self.slots[idx].used = true;
        self.slots[idx].entry = PathConfidenceEntry {
            destination,
            next_hop,
            confidence: 0.0,
            last_updated: now,
            sample_count: 0,
        };
        self.len += 1;
        self.slots[idx].as_mut().unwrap()
    }

    fn entry_index(&self, destination: u32, next_hop: u32) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| {
                slot.used
                    && slot.entry.destination == destination
                    && slot.entry.next_hop == next_hop
            })
            .map(|(idx, _)| idx)
    }

    fn evict_oldest(&mut self) {
        if self.len == 0 {
            return;
        }
        let mut candidate: Option<usize> = None;
        for (idx, slot) in self.slots.iter().enumerate() {
            if !slot.used {
                continue;
            }
            match candidate {
                None => candidate = Some(idx),
                Some(existing) => {
                    if slot.entry.last_updated < self.slots[existing].entry.last_updated {
                        candidate = Some(idx);
                    }
                }
            }
        }
        if let Some(idx) = candidate {
            self.slots[idx].used = false;
            self.len -= 1;
        }
    }
}

impl<const N: usize> Default for PathTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    used: bool,
    entry: PathConfidenceEntry,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            used: false,
            entry: PathConfidenceEntry::EMPTY,
        }
    }

    fn as_ref(&self) -> Option<&PathConfidenceEntry> {
        if self.used {
            Some(&self.entry)
        } else {
            None
        }
    }

    fn as_mut(&mut self) -> Option<&mut PathConfidenceEntry> {
        if self.used {
            Some(&mut self.entry)
        } else {
            None
        }
    }
}
