//! TTL and K-budget enforcement (SPEC.md §3.1).

use crate::constants::FLOOD_SENTINEL_K;

/// Reason a message was dropped at this hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Already seen (dedup cache hit).
    Duplicate,
    /// Relay is disabled on this device.
    RelayDisabled,
    /// Probabilistic relay check failed.
    RelayProbability,
    /// TTL reached zero.
    TtlExpired,
    /// Spray budget K exhausted.
    SprayBudgetZero,
    /// No reachable neighbour for directed routing.
    NoViableNeighbour,
    /// Payload too large for any available transport.
    NoViableTransport,
}

/// Result of TTL/K enforcement: the budget available for forwarding (SPEC.md §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopBudget {
    /// Decremented TTL for the forwarded copy.
    pub ttl: u8,
    /// K value for the forwarded copy.
    pub forwarded_k: u8,
    /// K value retained by this node.
    pub retained_k: u8,
    /// If true, forward to ALL neighbours (K=15 sentinel).
    pub flood_mode: bool,
}

impl HopBudget {
    /// Construct a hop budget.
    pub fn new(ttl: u8, forwarded_k: u8, retained_k: u8, flood_mode: bool) -> Self {
        HopBudget {
            ttl,
            forwarded_k,
            retained_k,
            flood_mode,
        }
    }
}

/// Enforce TTL and K-budget rules, returning the forwarding budget or a drop reason.
///
/// - TTL is decremented; TTL ≤ 1 → drop
/// - K=0 → drop (no spray budget)
/// - K=15 → flood mode (forward to all neighbours)
/// - Otherwise: `forwarded = k/2`, `retained = k - forwarded`
/// - Congestion halves K (except for GROUP_MGMT messages)
pub fn enforce_ttl_k(
    ttl: u8,
    k: u8,
    congested: bool,
    is_group_mgmt: bool,
) -> Result<HopBudget, DropReason> {
    if ttl <= 1 {
        return Err(DropReason::TtlExpired);
    }
    let ttl_after = ttl - 1;

    // K=0 enters the "wait phase" — no further spraying, but direct delivery
    // to a high-confidence 1-hop neighbour is still allowed (R2-ROUTE §3.1).
    if k == 0 {
        return Ok(HopBudget::new(ttl_after, 0, 0, false));
    }

    if congested && !is_group_mgmt {
        if k > 1 && k != FLOOD_SENTINEL_K {
            return build_budget(ttl_after, core::cmp::max(1, k / 2), false);
        } else if k == FLOOD_SENTINEL_K {
            return build_budget(ttl_after, FLOOD_SENTINEL_K / 2, false);
        }
    }

    build_budget(ttl_after, k, k == FLOOD_SENTINEL_K)
}

fn build_budget(ttl: u8, k: u8, flood_mode: bool) -> Result<HopBudget, DropReason> {
    if flood_mode {
        return Ok(HopBudget::new(
            ttl,
            FLOOD_SENTINEL_K,
            FLOOD_SENTINEL_K,
            true,
        ));
    }
    if k == 0 {
        return Err(DropReason::SprayBudgetZero);
    }
    let forwarded = k / 2;
    let retained = k - forwarded;
    Ok(HopBudget::new(ttl, forwarded, retained, false))
}
