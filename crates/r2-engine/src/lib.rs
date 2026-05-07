//! # r2-engine
//!
//! Sentant runtime engine for [Reality2](https://github.com/reality2-ai).
//!
//! Provides the core abstractions that both hand-written and compiler-generated
//! sentant code links against:
//!
//! - [`Sentant`] trait — state machine with event handlers
//! - [`Plugin`] trait — hardware abstraction (SPI, SD, ADC, etc.)
//! - [`Event`] — FNV-hashed event with CBOR payload
//! - [`Action`] — what sentants produce in response to events
//! - [`EventBus`] — dispatches events between sentants (dynamic routing)
//!
//! ## Design
//!
//! The engine is `no_std` compatible with optional `alloc` support.
//! On microcontrollers, sentants use static allocation and fixed-capacity
//! buffers. On Linux/SBC targets, `alloc` enables dynamic dispatch.
//!
//! The R2-COMPILE compiler generates code that uses these types with
//! **static dispatch** — the routing table is a `match` block, not a
//! hash map. This crate also provides a dynamic [`EventBus`] for
//! hand-written sentants and testing.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐     ┌──────────┐     ┌──────────┐
//! │  Transport   │────▶│ EventBus │────▶│ Sentant  │
//! │ (BLE/WiFi)  │◀────│          │◀────│ handlers │
//! └─────────────┘     └──────────┘     └──────────┘
//!                           │
//!                     ┌─────┴─────┐
//!                     │  Plugins   │
//!                     │ (HW/IO)   │
//!                     └───────────┘
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

/// Core event types.
pub mod event;
/// Sentant trait and state machine types.
pub mod sentant;
/// Plugin trait for hardware abstraction.
pub mod plugin;
/// Action types — sentant outputs.
pub mod action;
/// Fixed-capacity action buffer (no alloc).
pub mod action_buf;
/// Event queue (ring buffer, no alloc).
pub mod queue;
/// Timer registry for delayed sends (requires alloc).
#[cfg(feature = "alloc")]
pub mod timer;
/// Dynamic event bus (requires alloc).
#[cfg(feature = "alloc")]
pub mod bus;

// Re-exports for convenience
pub use event::{Event, EventSource, Target};
pub use sentant::{Sentant, SentantId, StateId};
pub use plugin::{Plugin, PluginId, PluginCommand};
pub use action::Action;
pub use action_buf::ActionBuf;
pub use queue::EventQueue;
#[cfg(feature = "alloc")]
pub use bus::EventBus;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod conformance;
