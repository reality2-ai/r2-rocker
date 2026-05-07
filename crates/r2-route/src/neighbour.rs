//! Neighbour table with exponential confidence decay (SPEC.md §2.1).
//!
//! Each observed neighbour gets an entry tracking confidence, link quality per
//! transport, mobility class, and last-seen time. Confidence decays exponentially;
//! entries are evicted when confidence drops below threshold or hard timeout expires.

use crate::constants::{
    INFRA_LAMBDA, LINK_QUALITY_ALPHA, MOBILE_LAMBDA, NEIGHBOUR_EVICT_THRESHOLD,
    NEIGHBOUR_HARD_TIMEOUT, NEIGHBOUR_INIT_CONF, NEIGHBOUR_POS_ALPHA,
};
use crate::transport::{clamp01, QualitySample, Transport, TransportSet};

/// Device mobility classification (SPEC.md §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobilityClass {
    /// Moving device (phone, wearable). Half-life ~5 min.
    Mobile,
    /// Fixed device (sensor, gateway). Half-life ~2.5 hours.
    Infrastructure,
}

impl MobilityClass {
    /// Exponential decay rate λ for this class.
    pub fn lambda(self) -> f32 {
        match self {
            MobilityClass::Mobile => MOBILE_LAMBDA,
            MobilityClass::Infrastructure => INFRA_LAMBDA,
        }
    }

    /// Approximate half-life in seconds.
    pub fn half_life_seconds(self) -> u32 {
        match self {
            MobilityClass::Mobile => 5 * 60,
            MobilityClass::Infrastructure => 150 * 60,
        }
    }
}

/// A single neighbour observation from a received message or scan.
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    /// FNV-1a hash of the neighbour's device UUID.
    pub hive_id: u32,
    /// Transport the observation arrived on.
    pub transport: Transport,
    /// Monotonic timestamp (seconds).
    pub timestamp: u32,
    /// Quality sample (RSSI, SNR, or direct value).
    pub quality: QualitySample,
    /// Raw RSSI if available.
    pub rssi: Option<i8>,
    /// Whether the neighbour is an MCU-only device.
    pub mcu_origin: bool,
    /// Mobility classification.
    pub mobility: MobilityClass,
}

impl Observation {
    /// Convert the quality sample to a [0, 1] value.
    pub fn measured_quality(&self) -> f32 {
        clamp01(self.quality.to_quality())
    }
}

/// A tracked neighbour with confidence and per-transport link quality.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeighbourEntry {
    /// FNV-1a hash of the neighbour's device UUID.
    pub hive_id: u32,
    /// Set of transports observed for this neighbour.
    pub transports: TransportSet,
    /// EWMA link quality per transport [0, 1].
    pub link_quality: [f32; Transport::COUNT],
    /// Last observation timestamp (seconds).
    pub last_seen: u32,
    /// Transport of most recent observation.
    pub last_seen_transport: Transport,
    /// Reachability confidence ∈ [0, 1] (SPEC.md §2.1).
    pub confidence: f32,
    /// Mobility classification.
    pub mobility: MobilityClass,
    /// True if only MCU-originated messages have been seen (no relay capability).
    pub mcu_only: bool,
    /// Last RSSI per transport (dBm).
    pub rssi: [i8; Transport::COUNT],
    /// Total observations ingested.
    pub sample_count: u16,
}

impl NeighbourEntry {
    /// Sentinel empty entry.
    pub const EMPTY: Self = NeighbourEntry {
        hive_id: 0,
        transports: TransportSet::empty(),
        link_quality: [0.0; Transport::COUNT],
        last_seen: 0,
        last_seen_transport: Transport::Ble,
        confidence: 0.0,
        mobility: MobilityClass::Mobile,
        mcu_only: false,
        rssi: [0; Transport::COUNT],
        sample_count: 0,
    };

    /// Create from a first observation.
    pub fn new(obs: &Observation) -> Self {
        let mut link_quality = [0.0; Transport::COUNT];
        let idx = obs.transport.index();
        link_quality[idx] = obs.measured_quality();
        let mut transports = TransportSet::empty();
        transports.insert(obs.transport);
        let mut rssi = [0i8; Transport::COUNT];
        if let Some(rssi_val) = obs.rssi {
            rssi[idx] = rssi_val;
        }
        NeighbourEntry {
            hive_id: obs.hive_id,
            transports,
            link_quality,
            last_seen: obs.timestamp,
            last_seen_transport: obs.transport,
            confidence: NEIGHBOUR_INIT_CONF,
            mobility: obs.mobility,
            mcu_only: obs.mcu_origin,
            rssi,
            sample_count: 1,
        }
    }

    /// Update with a new observation (EWMA quality, positive confidence reinforcement).
    pub fn update(&mut self, obs: &Observation) {
        let idx = obs.transport.index();
        let measured = obs.measured_quality();
        self.transports.insert(obs.transport);
        self.link_quality[idx] =
            LINK_QUALITY_ALPHA * measured + (1.0 - LINK_QUALITY_ALPHA) * self.link_quality[idx];
        if let Some(rssi_val) = obs.rssi {
            self.rssi[idx] = rssi_val;
        }
        self.last_seen = obs.timestamp;
        self.last_seen_transport = obs.transport;
        self.mobility = obs.mobility;
        if !obs.mcu_origin {
            self.mcu_only = false;
        } else if self.sample_count == 0 {
            self.mcu_only = true;
        }
        self.confidence = clamp01(self.confidence + NEIGHBOUR_POS_ALPHA * (1.0 - self.confidence));
        self.sample_count = self.sample_count.saturating_add(1);
    }

    /// Apply exponential decay to confidence based on elapsed time (SPEC.md §2.1).
    pub fn decay(&mut self, now: u32) {
        if now <= self.last_seen {
            return;
        }
        let elapsed = now - self.last_seen;
        let decay = libm::expf(-self.mobility.lambda() * (elapsed as f32));
        self.confidence = clamp01(self.confidence * decay);
    }

    /// Check if this neighbour is viable for routing (above confidence threshold, not MCU-only).
    pub fn is_viable(&self, min_confidence: f32) -> bool {
        self.confidence >= min_confidence && !self.mcu_only
    }
}

/// Fixed-capacity neighbour table. `N` is the maximum number of tracked neighbours.
#[derive(Debug)]
pub struct NeighbourTable<const N: usize> {
    slots: [Slot; N],
    len: usize,
}

impl<const N: usize> NeighbourTable<N> {
    /// Create an empty table.
    pub const fn new() -> Self {
        const SLOT: Slot = Slot::new();
        NeighbourTable {
            slots: [SLOT; N],
            len: 0,
        }
    }

    /// Number of active entries.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Iterate over active neighbour entries.
    pub fn iter(&self) -> impl Iterator<Item = &NeighbourEntry> {
        self.slots.iter().filter_map(|slot| slot.as_ref())
    }

    /// Mutable iteration over active entries.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut NeighbourEntry> {
        self.slots.iter_mut().filter_map(|slot| slot.as_mut())
    }

    /// Look up a neighbour by hive ID.
    pub fn get(&self, hive_id: u32) -> Option<&NeighbourEntry> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref())
            .find(|entry| entry.hive_id == hive_id)
    }

    /// Mutable lookup by hive ID.
    pub fn get_mut(&mut self, hive_id: u32) -> Option<&mut NeighbourEntry> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.as_mut())
            .find(|entry| entry.hive_id == hive_id)
    }

    /// Check if a neighbour is tracked and viable for routing.
    pub fn has_viable(&self, hive_id: u32, min_confidence: f32) -> bool {
        self.get(hive_id)
            .map(|entry| entry.is_viable(min_confidence))
            .unwrap_or(false)
    }

    /// Insert or update a neighbour from an observation. Evicts lowest-confidence if full.
    pub fn upsert(&mut self, obs: Observation) -> &mut NeighbourEntry {
        if let Some(idx) = self.index_of(obs.hive_id) {
            let entry = self.slots[idx].as_mut().unwrap();
            entry.update(&obs);
            return entry;
        }

        if self.len == N {
            self.evict_lowest();
        }

        let idx = self
            .slots
            .iter()
            .position(|slot| !slot.used)
            .expect("NeighbourTable capacity mis-tracked");
        self.slots[idx].entry = NeighbourEntry::new(&obs);
        self.slots[idx].used = true;
        self.len += 1;
        self.slots[idx].as_mut().unwrap()
    }

    fn index_of(&self, hive_id: u32) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.used && slot.entry.hive_id == hive_id)
            .map(|(idx, _)| idx)
    }

    fn lowest_index(&self) -> Option<usize> {
        let mut idx = None;
        let mut lowest = f32::MAX;
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.used {
                continue;
            }
            if slot.entry.confidence < lowest {
                lowest = slot.entry.confidence;
                idx = Some(i);
            } else if float_abs(slot.entry.confidence - lowest) < f32::EPSILON {
                if let Some(existing) = idx {
                    if slot.entry.last_seen < self.slots[existing].entry.last_seen {
                        idx = Some(i);
                    }
                }
            }
        }
        idx
    }

    fn evict_lowest(&mut self) {
        if let Some(i) = self.lowest_index() {
            self.slots[i].used = false;
            self.len -= 1;
        }
    }

    /// Decay all entries and evict those below threshold or past hard timeout (SPEC.md §2.1).
    pub fn decay(&mut self, now: u32) {
        for slot in &mut self.slots {
            if !slot.used {
                continue;
            }
            slot.entry.decay(now);
            let elapsed = now.saturating_sub(slot.entry.last_seen);
            if slot.entry.confidence < NEIGHBOUR_EVICT_THRESHOLD || elapsed > NEIGHBOUR_HARD_TIMEOUT
            {
                slot.used = false;
                self.len -= 1;
            }
        }
    }
}

impl<const N: usize> Default for NeighbourTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    used: bool,
    entry: NeighbourEntry,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            used: false,
            entry: NeighbourEntry::EMPTY,
        }
    }

    fn as_ref(&self) -> Option<&NeighbourEntry> {
        if self.used {
            Some(&self.entry)
        } else {
            None
        }
    }

    fn as_mut(&mut self) -> Option<&mut NeighbourEntry> {
        if self.used {
            Some(&mut self.entry)
        } else {
            None
        }
    }
}

#[inline(always)]
fn float_abs(v: f32) -> f32 {
    if v < 0.0 {
        -v
    } else {
        v
    }
}
