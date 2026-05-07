//! Forwarding engine — the main routing decision maker (SPEC.md §3).
//!
//! [`RouteEngine`] combines neighbour tracking, path learning, deduplication,
//! and strategy to produce [`ForwardAdvice`] for each incoming message.

use core::cmp;

use heapless::Vec;
use r2_wire::types::MsgType;

use crate::constants::FORWARDING_CONFIDENCE_FLOOR;
use crate::dedup::DedupCache;
use crate::hop::{self, DropReason, HopBudget};
use crate::neighbour::{NeighbourEntry, NeighbourTable, Observation};
use crate::path::PathTable;
use crate::strategy::StrategyVector;
use crate::transport::Transport;

/// Message destination — broadcast or a specific hive address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Broadcast to all reachable hives (`@all`, target = 0x00000000).
    Broadcast,
    /// Specific hive (FNV-1a hash of device UUID).
    Address(u32),
}

impl Target {
    /// Raw target ID (0 = broadcast).
    pub fn id(&self) -> u32 {
        match self {
            Target::Broadcast => 0,
            Target::Address(id) => *id,
        }
    }

    /// Returns true if broadcast (including target=0).
    pub fn is_broadcast(&self) -> bool {
        matches!(self, Target::Broadcast) || self.id() == 0
    }
}

impl From<u32> for Target {
    fn from(value: u32) -> Self {
        if value == 0 {
            Target::Broadcast
        } else {
            Target::Address(value)
        }
    }
}

/// Input to the forwarding engine (SPEC.md §3).
#[derive(Debug, Clone)]
pub struct ForwardRequest {
    /// Current monotonic timestamp (seconds).
    pub now: u32,
    /// Message ID for dedup.
    pub msg_id: u16,
    /// Source hop (compressed hive ID of the immediate sender).
    pub source_hop: u16,
    /// Current TTL.
    pub ttl: u8,
    /// Current spray-and-wait K budget.
    pub k: u8,
    /// Intended destination.
    pub destination: Target,
    /// Message type (affects GROUP_MGMT exemptions).
    pub msg_type: MsgType,
    /// Payload size for transport selection.
    pub payload_len: usize,
    /// Whether this device relays messages.
    pub relay_enabled: bool,
    /// Whether the local mesh is congested.
    pub congested: bool,
    /// Random [0, 1) for probabilistic relay decision.
    pub dice_roll: f32,
}

/// A specific next-hop selected by the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectedHop {
    /// Neighbour hive ID.
    pub neighbour: u32,
    /// Best transport to reach this neighbour.
    pub transport: Transport,
    /// Path or neighbour confidence.
    pub confidence: f32,
}

/// Forwarding decision (SPEC.md §3.2).
#[derive(Debug, Clone)]
pub enum ForwardAction<const N: usize> {
    /// Message should be dropped.
    Drop(DropReason),
    /// Deliver locally only (TTL=0 or destination is self).
    DeliverOnly,
    /// Route via a specific next-hop.
    Directed(DirectedHop),
    /// Forward to multiple neighbours (flood or spray).
    Flood(Vec<DirectedHop, N>),
}

/// Complete forwarding advice including budget allocation.
#[derive(Debug, Clone)]
pub struct ForwardAdvice<const N: usize> {
    /// The routing action to take.
    pub action: ForwardAction<N>,
    /// TTL for the forwarded copy.
    pub ttl: u8,
    /// K budget for the forwarded copy.
    pub forwarded_k: u8,
    /// K budget retained by this node.
    pub retained_k: u8,
}

impl<const N: usize> ForwardAdvice<N> {
    fn drop(reason: DropReason) -> Self {
        ForwardAdvice {
            action: ForwardAction::Drop(reason),
            ttl: 0,
            forwarded_k: 0,
            retained_k: 0,
        }
    }

    fn directed(budget: HopBudget, hop: DirectedHop) -> Self {
        ForwardAdvice {
            action: ForwardAction::Directed(hop),
            ttl: budget.ttl,
            forwarded_k: budget.forwarded_k,
            retained_k: budget.retained_k,
        }
    }

    fn flood(budget: HopBudget, hops: Vec<DirectedHop, N>) -> Self {
        ForwardAdvice {
            action: ForwardAction::Flood(hops),
            ttl: budget.ttl,
            forwarded_k: budget.forwarded_k,
            retained_k: budget.retained_k,
        }
    }
}

/// The routing engine — combines neighbours, paths, dedup, and strategy (SPEC.md §3).
///
/// Generic over table capacities for different device classes:
/// - Constrained MCU: `RouteEngine<16, 16, 32>`
/// - Gateway: `RouteEngine<64, 64, 64>` (default)
pub struct RouteEngine<
    const NEIGHBOURS: usize = 64,
    const PATHS: usize = 64,
    const DEDUP: usize = 64,
> {
    neighbours: NeighbourTable<NEIGHBOURS>,
    paths: PathTable<PATHS>,
    dedup: DedupCache<DEDUP>,
    strategy: StrategyVector,
}

impl<const NEIGHBOURS: usize, const PATHS: usize, const DEDUP: usize>
    RouteEngine<NEIGHBOURS, PATHS, DEDUP>
{
    /// Create with default strategy.
    pub fn new() -> Self {
        RouteEngine {
            neighbours: NeighbourTable::new(),
            paths: PathTable::new(),
            dedup: DedupCache::new(),
            strategy: StrategyVector::default(),
        }
    }

    /// Create with a custom strategy vector.
    pub fn with_strategy(strategy: StrategyVector) -> Self {
        RouteEngine {
            strategy,
            ..Self::new()
        }
    }

    /// Current strategy vector.
    pub fn strategy(&self) -> &StrategyVector {
        &self.strategy
    }

    /// Mutable access to strategy vector.
    pub fn strategy_mut(&mut self) -> &mut StrategyVector {
        &mut self.strategy
    }

    /// The neighbour table.
    pub fn neighbours(&self) -> &NeighbourTable<NEIGHBOURS> {
        &self.neighbours
    }

    /// Mutable access to the neighbour table.
    pub fn neighbours_mut(&mut self) -> &mut NeighbourTable<NEIGHBOURS> {
        &mut self.neighbours
    }

    /// The path table.
    pub fn paths(&self) -> &PathTable<PATHS> {
        &self.paths
    }

    /// Ingest a neighbour observation (BLE scan, received message, etc.).
    pub fn ingest_observation(&mut self, obs: Observation) -> &mut NeighbourEntry {
        self.neighbours.upsert(obs)
    }

    /// Decay all neighbour confidences and evict stale entries.
    pub fn decay_neighbours(&mut self, now: u32) {
        self.neighbours.decay(now);
    }

    /// Decay all path confidences and evict stale entries.
    pub fn decay_paths(&mut self, now: u32) {
        self.paths.decay(now);
    }

    /// Record a confirmed delivery success (positive path reinforcement).
    pub fn record_delivery_success(&mut self, destination: u32, next_hop: u32, now: u32) {
        self.paths.record_positive(destination, next_hop, now);
    }

    /// Record an indirect observation (overheard relay success).
    pub fn record_indirect_success(&mut self, destination: u32, via: u32, now: u32) {
        self.paths.record_indirect(destination, via, now);
    }

    /// Seed a path entry with known confidence (e.g., from a trust group peer).
    pub fn seed_path(&mut self, destination: u32, via: u32, now: u32, confidence: f32) {
        self.paths.seed(destination, via, now, confidence);
    }

    /// Produce a forwarding decision for an incoming message (SPEC.md §3.1).
    pub fn plan_forward(&mut self, req: ForwardRequest) -> ForwardAdvice<NEIGHBOURS> {
        if req.msg_type != MsgType::GroupMgmt && !req.relay_enabled {
            return ForwardAdvice::drop(DropReason::RelayDisabled);
        }

        if self.dedup.is_duplicate(req.now, req.msg_id, req.source_hop) {
            return ForwardAdvice::drop(DropReason::Duplicate);
        }

        if req.msg_type != MsgType::GroupMgmt && req.dice_roll > self.strategy.relay_probability {
            return ForwardAdvice::drop(DropReason::RelayProbability);
        }

        let budget = match hop::enforce_ttl_k(
            req.ttl,
            req.k,
            req.congested,
            req.msg_type == MsgType::GroupMgmt,
        ) {
            Ok(b) => b,
            Err(reason) => return ForwardAdvice::drop(reason),
        };

        if req.destination.is_broadcast() {
            return self.build_flood_plan(req.payload_len, budget);
        }

        if let Some(advice) = self.try_directed(req.destination.id(), req.payload_len, budget) {
            return advice;
        }

        self.build_flood_plan(req.payload_len, budget)
    }

    fn try_directed(
        &self,
        destination: u32,
        payload_len: usize,
        budget: HopBudget,
    ) -> Option<ForwardAdvice<NEIGHBOURS>> {
        let best = self.paths.best_for(destination)?;
        if best.confidence < self.strategy.forwarding_threshold {
            return None;
        }
        let neighbour = self.neighbours.get(best.next_hop)?;
        if !neighbour.is_viable(FORWARDING_CONFIDENCE_FLOOR) {
            return None;
        }
        let (transport, _) = self.best_transport(neighbour, payload_len)?;
        let hop = DirectedHop {
            neighbour: neighbour.hive_id,
            transport,
            confidence: best.confidence,
        };
        Some(ForwardAdvice::directed(budget, hop))
    }

    fn build_flood_plan(&self, payload_len: usize, budget: HopBudget) -> ForwardAdvice<NEIGHBOURS> {
        let mut hops: Vec<DirectedHop, NEIGHBOURS> = Vec::new();
        // Spray-and-wait carrier count is the *original* K (R2-WIRE §8.4):
        // "Originator sets K... the number of copies to spray". The originator
        // sprays K copies to K distinct carriers; each carrier then receives
        // `forwarded_k = floor(K/2)` and enters its own spray/wait phase.
        //
        // The original K is reconstructible from the budget because
        // enforce_ttl_k splits K into forwarded + retained where their sum is
        // the input K (R2-ROUTE hop::build_budget): forwarded = K/2, retained
        // = K - forwarded. Hence `forwarded_k + retained_k == K_input`.
        //
        // For flood mode (K=15) we ignore the budget split entirely and spray
        // to every viable neighbour, capped by NEIGHBOURS.
        let original_k = budget.forwarded_k as usize + budget.retained_k as usize;
        let mut limit = if budget.flood_mode {
            NEIGHBOURS
        } else {
            cmp::max(1, original_k)
        };
        if limit > NEIGHBOURS {
            limit = NEIGHBOURS;
        }
        for entry in self.neighbours.iter() {
            if !entry.is_viable(FORWARDING_CONFIDENCE_FLOOR) {
                continue;
            }
            if let Some((transport, _score)) = self.best_transport(entry, payload_len) {
                let hop = DirectedHop {
                    neighbour: entry.hive_id,
                    transport,
                    confidence: entry.confidence,
                };
                if hops.push(hop).is_err() {
                    break;
                }
                if hops.len() >= limit && !budget.flood_mode {
                    break;
                }
            }
        }
        if hops.is_empty() {
            return ForwardAdvice::drop(DropReason::NoViableNeighbour);
        }
        ForwardAdvice::flood(budget, hops)
    }

    fn best_transport(
        &self,
        neighbour: &NeighbourEntry,
        payload_len: usize,
    ) -> Option<(Transport, f32)> {
        let mut best: Option<(Transport, f32)> = None;
        for transport in Transport::all() {
            if !neighbour.transports.contains(transport) {
                continue;
            }
            if payload_len > transport.max_payload() {
                continue;
            }
            let quality = neighbour.link_quality[transport.index()];
            if quality <= 0.0 {
                continue;
            }
            let weight = self.strategy.transport_weight(transport);
            if weight <= 0.0 {
                continue;
            }
            let score = (quality * weight) / transport.power_cost();
            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((transport, score)),
            }
        }
        best
    }
}

impl Default for Target {
    fn default() -> Self {
        Target::Broadcast
    }
}

impl Default for ForwardRequest {
    fn default() -> Self {
        ForwardRequest {
            now: 0,
            msg_id: 0,
            source_hop: 0,
            ttl: 0,
            k: 0,
            destination: Target::Broadcast,
            msg_type: MsgType::Event,
            payload_len: 0,
            relay_enabled: true,
            congested: false,
            dice_roll: 0.0,
        }
    }
}
