//! # r2-core
//!
//! Platform-independent Reality2 protocol stack.
//!
//! This is a **facade crate** that re-exports the individual R2 protocol crates
//! and adds `alloc`-based convenience APIs for platforms with a heap (Linux, RTOS).
//!
//! ## Crate Structure
//!
//! | Crate | Purpose | `no_std` |
//! |-------|---------|----------|
//! | [`r2-fnv`](../r2-fnv) | Event name hashing (FNV-1a 32-bit) | ✅ |
//! | [`r2-cbor`](../r2-cbor) | Constrained CBOR encoding | ✅ |
//! | [`r2-wire`](../r2-wire) | Wire protocol framing | ✅ |
//!
//! For `no_std` / bare-metal targets, depend on the individual crates directly.
//! For Linux/RTOS targets, depend on `r2-core` for the full stack + alloc wrappers.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(test)]
extern crate std;

// ── Re-exports of protocol crates ──────────────────────────────

/// Event name hashing (re-exported from `r2-fnv`).
pub use r2_fnv as fnv;

// ── Alloc-based modules (convenience APIs for platforms with a heap) ──

/// CBOR encoding with `alloc` — tree-based `CborValue` API.
///
/// For `no_std` fixed-buffer encoding, use `r2-cbor` directly.
pub mod cbor;

/// Wire protocol framing with `alloc` — `Vec`-based route stacks.
///
/// For `no_std` fixed-array encoding, use `r2-wire` directly.
pub mod wire;

/// BLE beacon encode/decode.
pub mod beacon;
