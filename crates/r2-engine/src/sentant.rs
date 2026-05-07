//! Sentant trait — the core abstraction for R2 agents.
//!
//! A sentant is a deterministic state machine that handles events and
//! produces actions. The same trait is implemented by:
//!
//! - **Compiler-generated code** (R2-COMPILE: YAML → Rust)
//! - **Hand-written Rust** (for platform sentants, plugins, testing)
//!
//! Both produce identical wire behaviour — a compiled sentant is
//! indistinguishable from an interpreted one on the mesh.

use crate::action_buf::ActionBuf;
use crate::event::Event;

/// Sentant identifier — unique within a hive (local scope only).
///
/// Assigned at startup. Small integer index, not a UUID.
/// Trust group addressing uses RBID + event hash, not sentant IDs.
pub type SentantId = u8;

/// State identifier — index into the sentant's state enum.
///
/// State 0 is always the initial state (typically "idle" or "start").
/// Maximum 256 states per sentant (u8).
pub type StateId = u8;

/// The sentant trait.
///
/// Sentants are **synchronous and non-blocking**. `handle_event` runs
/// to completion, pushing [`Action`]s into the provided buffer. The
/// engine then executes those actions (send events, call plugins, etc.).
///
/// This separation of "decide" and "execute" keeps sentants pure and
/// testable — you can feed events in and inspect the action list without
/// any I/O.
///
/// # Example (hand-written)
///
/// ```rust,ignore
/// struct PingSentant { state: StateId }
///
/// impl Sentant for PingSentant {
///     fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
///         if event.hash == PING_HASH {
///             self.state = 1; // "responded"
///             actions.push(Action::send(Target::Sender, PONG_HASH, &pong_payload));
///         }
///     }
///     fn state(&self) -> StateId { self.state }
///     fn class_hash(&self) -> u32 { CLASS_HASH }
///     fn name(&self) -> &str { "ping" }
/// }
/// ```
pub trait Sentant {
    /// Handle an incoming event.
    ///
    /// The sentant inspects the event, optionally transitions state, and
    /// pushes zero or more [`Action`]s into the buffer. The engine calls
    /// this synchronously — it MUST NOT block or perform I/O.
    fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf);

    /// Current state index.
    fn state(&self) -> StateId;

    /// FNV-1a hash of the sentant's class string (e.g., `"nz.ac.friction.sensor.adxl355"`).
    fn class_hash(&self) -> u32;

    /// Human-readable name (for logging/debug). May be empty on constrained targets.
    fn name(&self) -> &str;

    /// Event hashes this sentant wants to receive.
    ///
    /// Used by the dynamic [`EventBus`](crate::bus::EventBus) for subscription.
    /// Compiler-generated routing ignores this (routing is static).
    ///
    /// Returns a fixed slice — subscriptions don't change at runtime.
    fn subscriptions(&self) -> &[u32] {
        &[]
    }

    /// Called once at startup, after all sentants and plugins are registered.
    ///
    /// Use for initial state setup, starting timers, etc.
    fn init(&mut self, _actions: &mut ActionBuf) {}
}
