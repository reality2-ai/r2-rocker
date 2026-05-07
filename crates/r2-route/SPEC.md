# R2-ROUTE: Mesh Routing Primitives for Reality2

**Version:** 0.1.0
**Status:** Active development

---

## 1. Purpose

R2-ROUTE implements the mesh routing layer for Reality2. It provides:
- Probabilistic neighbour tracking with exponential confidence decay
- Path learning via positive/indirect reinforcement
- Spray-and-wait message dissemination with K-budget splitting
- Message deduplication
- Route stack manipulation (compact and extended formats)
- Transport-aware forwarding strategy

All structures are `no_std` with fixed-capacity tables (const generics).

## 2. Routing Model

R2 uses **probabilistic opportunistic routing**: no topology tables, no route
advertisements. Nodes learn which neighbours are good next-hops for which
destinations by observing delivery success.

### 2.1 Neighbour Confidence

Each neighbour entry tracks:
- **Confidence** ∈ [0, 1]: probability the neighbour is still reachable
- **Mobility class**: Infrastructure (λ=0.000077) or Mobile (λ=0.0023)
- **Transport set**: which transports have been observed (BLE, WiFi, LoRa, Internet)
- **Link quality**: EWMA of per-observation quality samples (α=0.3)

Confidence decays exponentially: `c(t) = c₀ · e^(-λ · Δt)`

| Class | λ | Half-life |
|-------|---|-----------|
| Infrastructure | 0.000077 | ~2.5 hours |
| Mobile | 0.0023 | ~5 minutes |

Neighbours below threshold (0.01) are evicted. Hard timeout: 30 minutes.

### 2.2 Path Confidence

The path table records `(destination, next_hop) → confidence` entries:
- **Positive reinforcement** (direct delivery success): `c' = c + α·(1-c)`, α=0.2
- **Indirect reinforcement** (overheard relay): `c' = c + α·(1-c)`, α=0.05
- **Decay**: `c(t) = c₀ · e^(-μ · Δt)`, μ=0.00077

## 3. Forwarding Engine

`RouteEngine::plan_forward()` takes a `ForwardRequest` and returns `ForwardAdvice`:

### 3.1 Decision Flow

1. **Dedup check**: reject if `(msg_id, source)` seen within 60 seconds
2. **TTL check**: drop if TTL=0
3. **K-budget split**: `forwarded = k/2`, `retained = k - forwarded`
4. **Flood mode**: if K=15 (sentinel), flood to all neighbours except source
5. **Directed routing**: if path table has entry above threshold → direct to best next-hop
6. **Spray fallback**: probabilistic relay based on strategy vector

### 3.2 ForwardAction

| Action | When |
|--------|------|
| `Directed(hop)` | Path table has confident route |
| `Flood(hops)` | K=15 (broadcast/flood mode) |
| `Spray(hops)` | No confident route, K>0 |
| `Drop(reason)` | TTL=0, dedup, no budget, relay disabled |
| `LocalOnly` | TTL=0 or destination is self |

## 4. Transport Model

| Transport | Index | Max Payload | Power Cost | Jitter (normal) | Jitter (congested) |
|-----------|-------|-------------|------------|------------------|-------------------|
| BLE | 0 | 200 B | 0.1 | 0–50 ms | 50–200 ms |
| WiFi | 1 | 64 KB | 1.0 | 0–20 ms | 20–100 ms |
| LoRa | 2 | 222 B | 0.5 | 0–2000 ms | 2000–5000 ms |
| Internet | 3 | 64 KB | 0.01 | 0–10 ms | 10–50 ms |

### 4.1 Quality Mapping

- **RSSI → quality**: linear mapping, -30 dBm → 1.0, -100 dBm → 0.0
- **SNR → quality**: `clamp01((snr - 5.0) / 15.0)`
- Quality samples are EWMA-smoothed per neighbour (α=0.3)

## 5. Route Stack

Route entries are compressed hive IDs:
- **Compact**: upper 16 bits of FNV-1a 32-bit hash → `u16`
- **Extended**: full 32-bit hash → `u32`

Operations: `append`, `pop_for_reply`, `peek_next_hop`. Max 8 entries.

## 6. Deduplication

Fixed-capacity ring buffer of `(msg_id, source, timestamp)` tuples.
Entry expires after 60 seconds. Configurable capacity via const generic.

## 7. Constants

| Name | Value | Spec Reference |
|------|-------|----------------|
| `DEFAULT_TTL` | 5 | R2-WIRE §3.2 |
| `DEDUP_TTL_SECS` | 60 | R2-ROUTE §6 |
| `NEIGHBOUR_INIT_CONF` | 0.5 | §2.1 |
| `NEIGHBOUR_EVICT_THRESHOLD` | 0.01 | §2.1 |
| `NEIGHBOUR_HARD_TIMEOUT` | 1800 s | §2.1 |
| `LINK_QUALITY_ALPHA` | 0.3 | §2.1 |
| `MOBILE_LAMBDA` | 0.0023 | §2.1 |
| `INFRA_LAMBDA` | 0.000077 | §2.1 |
| `PATH_DECAY_MU` | 0.00077 | §2.2 |
| `PATH_POS_ALPHA` | 0.2 | §2.2 |
| `PATH_INDIRECT_ALPHA` | 0.05 | §2.2 |
| `PATH_EVICT_THRESHOLD` | 0.01 | §2.2 |
| `BLE_MAX_PAYLOAD` | 200 | §4 |
| `WIFI_MAX_PAYLOAD` | 65536 | §4 |
| `FLOOD_SENTINEL_K` | 15 | §3.1 |

## 8. Implementation Status

This crate implements the **data structures and basic decision flow** from
the full R2-ROUTE specification. The following maps crate coverage to the
normative spec (`r2-specifications/specs/r2-core/R2-ROUTE.md`):

### Implemented

| Spec Section | Crate Module | Notes |
|-------------|-------------|-------|
| §2 Neighbour Discovery | `neighbour.rs` | Table, confidence decay, EWMA link quality, eviction |
| §3.1–3.4 Relay basics | `engine.rs` | TTL, dedup, K-budget split, flood sentinel |
| §3.6 Relay jitter | `jitter.rs` | Range calculation per transport |
| §4.1–4.5 Path routing | `path.rs`, `engine.rs` | Confidence table, positive/indirect reinforcement, directed vs flood |
| §5.1–5.3 Transport selection | `transport.rs` | Quality mapping (RSSI/SNR), transport set, payload limits |
| §6.1–6.2 Route stack basics | `route_stack.rs` | Append, pop, peek, compact/extended |
| §9.1–9.2 Strategy vector | `strategy.rs` | Weight struct, default v0.1 values |

### Not Yet Implemented

| Spec Section | Description | Priority |
|-------------|-------------|----------|
| §3.5 Relay suppression | Suppress relay when better-positioned neighbour heard | Medium |
| §3.7–3.8 Relay pipeline | Full pipeline with ordered stages | Medium |
| §3A Congestion management | Detection, response, recovery, priority queuing | High |
| §4.6 Confidence seeding from route stacks | Learn paths by inspecting relay chains | High |
| §4A Entanglement routing | Cross-trust-group route reinforcement | Low (needs r2-trust integration) |
| §5.4 Multi-transport relay | Forward on different transport than received | Medium |
| §5.5 Transport transcoding | Reframe between compact/extended per transport MTU | Medium |
| §5.6 Transport state feedback | Feed transport health back into forwarding decisions | Low |
| §6.3 Route stack expiry | Age out stale route entries | Medium |
| §6.4 Reply without route stack | Flood-based reply fallback | Low |
| §7 Metrics & observability | Per-transport counters, relay stats | Low |
| §8 Shared mesh semantics | Trust-agnostic relay, traffic analysis mitigation | Medium |
| §13 Security mitigations | TTL inflation detection, Sybil protection, rate limiting | High |

### Known Discrepancies with Spec

| Item | Spec Says | Code Says | Resolution |
|------|-----------|-----------|------------|
| BLE power cost | 0.1 (§5.1) | 1.0 (`transport.rs`) | Needs investigation — spec or code wrong |
| RSSI mapping ceiling | -30 dBm → 1.0 | -50 dBm → 1.0 | Needs investigation |
| LoRa max payload | 222 B (§5.1) | 200 B (`constants.rs`) | Needs alignment |
| Jitter ranges | Spec §3.6 values | Code values differ | Needs cross-check |

---

*This spec is self-contained. For wire format details, see r2-wire SPEC.md.
For the full normative specification, see R2-ROUTE.md in r2-specifications.*
