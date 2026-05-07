//! # r2-route
//!
//! Mesh routing primitives for [Reality2](https://github.com/reality2-ai),
//! implementing the R2-ROUTE specification (see [`SPEC.md`](../SPEC.md) in this crate).
//!
//! Provides probabilistic neighbour tracking, path learning, spray-and-wait
//! forwarding, deduplication, and route stack manipulation. All structures are
//! `no_std` with fixed-capacity tables (const generics).
//!
//! ## Architecture
//!
//! ```text
//! Observation → NeighbourTable → PathTable → RouteEngine → ForwardAdvice
//!                                                ↑
//!                              DedupCache ───────┘
//! ```
//!
//! The [`RouteEngine`] combines all components: ingest neighbour observations,
//! learn paths from delivery success, and produce forwarding decisions.

#![no_std]
#![deny(missing_docs)]

/// Tuning constants (SPEC.md §7).
pub mod constants;
/// Message deduplication cache (SPEC.md §6).
pub mod dedup;
/// Forwarding engine — the main routing decision maker (SPEC.md §3).
pub mod engine;
/// TTL and K-budget enforcement (SPEC.md §3.1).
pub mod hop;
/// Relay jitter for collision avoidance (SPEC.md §4).
pub mod jitter;
/// Neighbour table with exponential confidence decay (SPEC.md §2.1).
pub mod neighbour;
/// Path confidence table with reinforcement learning (SPEC.md §2.2).
pub mod path;
/// Route stack manipulation for compact and extended formats (SPEC.md §5).
pub mod route_stack;
/// Forwarding strategy vector (SPEC.md §3.2).
pub mod strategy;
/// Transport types, quality mapping, and transport sets (SPEC.md §4).
pub mod transport;

pub use engine::{ForwardAction, ForwardAdvice, ForwardRequest, RouteEngine, Target};
pub use hop::{DropReason, HopBudget};
pub use jitter::{relay_jitter_ms, RandomSource, XorShift32};
pub use neighbour::{MobilityClass, NeighbourEntry, NeighbourTable, Observation};
pub use path::{PathConfidenceEntry, PathTable};
pub use route_stack::{
    append_compact, append_extended, compress_hive_id_16, peek_next_hop_compact,
    peek_next_hop_extended, pop_for_reply_compact, pop_for_reply_extended, RouteStackError,
};
pub use strategy::StrategyVector;
pub use transport::{
    clamp01, quality_from_rssi, quality_from_snr, QualitySample, Transport, TransportSet,
};

#[cfg(test)]
mod tests;
