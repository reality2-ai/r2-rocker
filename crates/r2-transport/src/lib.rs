//! # r2-transport
//!
//! Transport binding abstraction for [Reality2](https://github.com/reality2-ai),
//! implementing the R2-TRANSPORT specification (see [`SPEC.md`](../SPEC.md)).
//!
//! ## Scope
//!
//! This crate carries **R2-WIRE frames** — events, heartbeats, capabilities,
//! and GROUP_MGMT messages.  These are small (typically 16–200 bytes),
//! fire-and-forget messages that form the R2 mesh.
//!
//! Bulk data transfer (firmware updates, chat logs, AI responses, file sync)
//! is a **plugin** concern (R2-WIRE §1.1.1) and is explicitly out of scope.
//! Plugins use transport connectivity (TCP, WebSocket, etc.) for their own
//! reliable-delivery protocols, but they do not go through this crate.
//!
//! ## What This Crate Provides
//!
//! - **[`Transport`] trait** — medium-agnostic interface every transport
//!   implements (R2-ROUTE §1.4.4).  BLE, WiFi/UDP, LoRa, and TCP/IP are
//!   all peers.
//! - **[`framing`]** — transport-agnostic helpers for wrapping R2-WIRE
//!   frames for stream transports (TCP length prefix, BLE L2CAP length
//!   prefix).
//! - **[`format`]** — wire format selection by transport context (compact
//!   vs extended, per R2-WIRE §4.3.5).
//! - **[`tcp`]** / **[`udp`]** — binding helpers for IP transports.
//!
//! BLE and LoRa bindings live in their own crates because they depend on
//! hardware-specific libraries.  They implement the same [`Transport`]
//! trait.
//!
//! ## `no_std` Support
//!
//! The core trait and framing helpers are `no_std` compatible (with
//! `alloc`).  TCP/UDP helpers operate on byte slices, not sockets.

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "alloc")]
extern crate alloc;

/// The core transport trait — medium-agnostic interface (R2-ROUTE §1.4.4).
pub mod transport;

/// Wire format selection by transport context (R2-WIRE §4.3.5).
pub mod format;

/// Transport-agnostic framing helpers (length prefixes for stream transports).
pub mod framing;

/// TCP stream binding for R2-WIRE events (R2-WIRE §13.4).
pub mod tcp;

/// UDP datagram binding for R2-WIRE events (R2-WIRE §13.3 / R2-WIFI §4).
pub mod udp;

pub use format::WireFormat;
pub use framing::{read_length_prefix, write_length_prefix, FrameError};
pub use transport::{LinkQuality, SendError, Transport, TransportId, TransportState};

#[cfg(test)]
mod tests;
