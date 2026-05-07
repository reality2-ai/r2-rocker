//! Tuning constants for the routing engine (SPEC.md §7).

/// Default time-to-live for new messages.
pub const DEFAULT_TTL: u8 = 5;
/// Deduplication cache entry lifetime in seconds (SPEC.md §6).
pub const DEDUP_TTL_SECS: u32 = 60;
/// Initial confidence for a newly observed neighbour (SPEC.md §2.1).
pub const NEIGHBOUR_INIT_CONF: f32 = 0.5;
/// Confidence below which a neighbour is evicted (SPEC.md §2.1).
pub const NEIGHBOUR_EVICT_THRESHOLD: f32 = 0.01;
/// Hard timeout: evict neighbour regardless of confidence (SPEC.md §2.1).
pub const NEIGHBOUR_HARD_TIMEOUT: u32 = 30 * 60;
/// EWMA smoothing factor for link quality samples (SPEC.md §2.1).
pub const LINK_QUALITY_ALPHA: f32 = 0.3;
/// EWMA factor for neighbour positive reinforcement (SPEC.md §2.1).
pub const NEIGHBOUR_POS_ALPHA: f32 = 0.2;
/// Exponential decay rate for mobile neighbours (SPEC.md §2.1).
pub const MOBILE_LAMBDA: f32 = 0.0023;
/// Exponential decay rate for infrastructure neighbours (SPEC.md §2.1).
pub const INFRA_LAMBDA: f32 = 0.000077;
/// Exponential decay rate for path confidence (SPEC.md §2.2).
pub const PATH_DECAY_MU: f32 = 0.00077;
/// EWMA factor for path positive reinforcement (SPEC.md §2.2).
pub const PATH_POS_ALPHA: f32 = 0.2;
/// EWMA factor for indirect (overheard) path reinforcement (SPEC.md §2.2).
pub const PATH_INDIRECT_ALPHA: f32 = 0.05;
/// Confidence below which a path entry is evicted (SPEC.md §2.2).
pub const PATH_EVICT_THRESHOLD: f32 = 0.01;
/// Maximum BLE payload in bytes (SPEC.md §4).
pub const BLE_MAX_PAYLOAD: usize = 200;
/// Maximum WiFi/Internet payload in bytes (SPEC.md §4).
pub const WIFI_MAX_PAYLOAD: usize = 64 * 1024;
/// Maximum LoRa payload in bytes (SPEC.md §4).
pub const LORA_MAX_PAYLOAD: usize = 200;
/// Maximum Internet payload in bytes (SPEC.md §4).
pub const INTERNET_MAX_PAYLOAD: usize = 64 * 1024;
/// BLE relay jitter range (min_ms, max_ms) — normal conditions.
pub const BLE_JITTER: (u32, u32) = (10, 50);
/// WiFi relay jitter range — normal conditions.
pub const WIFI_JITTER: (u32, u32) = (5, 20);
/// LoRa relay jitter range — normal conditions.
pub const LORA_JITTER: (u32, u32) = (100, 500);
/// BLE relay jitter range — congested conditions.
pub const BLE_JITTER_CONGESTED: (u32, u32) = (20, 100);
/// WiFi relay jitter range — congested conditions.
pub const WIFI_JITTER_CONGESTED: (u32, u32) = (10, 40);
/// LoRa relay jitter range — congested conditions.
pub const LORA_JITTER_CONGESTED: (u32, u32) = (200, 1000);
/// Internet relay jitter range — normal conditions.
pub const INTERNET_JITTER: (u32, u32) = (5, 20);
/// Internet relay jitter range — congested conditions.
pub const INTERNET_JITTER_CONGESTED: (u32, u32) = (10, 40);
/// K value indicating flood mode — all copies forwarded (SPEC.md §3.1).
pub const FLOOD_SENTINEL_K: u8 = 15;
/// Minimum neighbour confidence for forwarding consideration.
pub const FORWARDING_CONFIDENCE_FLOOR: f32 = 0.1;
