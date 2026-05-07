//! Relay jitter for collision avoidance (SPEC.md §4).

use crate::transport::Transport;

/// Source of random `u32` values. Implement for your platform's RNG.
pub trait RandomSource {
    /// Return the next random `u32`.
    fn next_u32(&mut self) -> u32;
}

/// Compute relay jitter in milliseconds for a given transport (SPEC.md §4).
///
/// Returns a uniformly distributed value in the transport's jitter range.
pub fn relay_jitter_ms<R: RandomSource>(rng: &mut R, transport: Transport, congested: bool) -> u32 {
    let (min, max) = transport.jitter_range(congested);
    if min >= max {
        return max;
    }
    let span = (max - min) + 1;
    min + (rng.next_u32() % span)
}

/// Deterministic XorShift32 RNG for tests and embedded use (no `rand` dependency).
#[derive(Debug, Clone, Copy)]
pub struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    /// Create with a seed. Seed must be nonzero.
    pub const fn new(seed: u32) -> Self {
        XorShift32 { state: seed }
    }
}

impl Default for XorShift32 {
    fn default() -> Self {
        Self::new(0x1234_5678)
    }
}

impl RandomSource for XorShift32 {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }
}
