//! Dynamic event bus — dispatches events to subscribed sentants.
//!
//! This is the **dynamic routing** variant, used when sentants are
//! registered at runtime (hand-written code, testing, Linux targets).
//!
//! For compiler-generated code, the routing is a static `match` block —
//! faster and smaller, but fixed at compile time. Both approaches
//! produce identical wire behaviour.
//!
//! Requires the `alloc` feature.

extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::action::Action;
use crate::action_buf::ActionBuf;
use crate::event::{Event, EventSource, Target};
use crate::plugin::{Plugin, PluginId};
use crate::queue::{EventQueue, QueuedEvent};
use crate::sentant::{Sentant, SentantId};
use crate::timer::TimerRegistry;

/// Maximum sentants in one hive.
const MAX_SENTANTS: usize = 16;
/// Maximum plugins in one hive.
const MAX_PLUGINS: usize = 8;

/// A registered sentant with its subscriptions.
struct SentantSlot {
    sentant: Box<dyn Sentant>,
    id: SentantId,
    /// Event hashes this sentant subscribes to.
    subscriptions: Vec<u32>,
}

/// A registered plugin.
struct PluginSlot {
    plugin: Box<dyn Plugin>,
    id: PluginId,
}

/// Dynamic event bus for a single hive.
///
/// Manages sentants, plugins, and event dispatch.
///
/// # Main Loop
///
/// ```rust,ignore
/// let mut bus = EventBus::new();
/// bus.register_sentant(Box::new(my_sentant));
/// bus.register_plugin(Box::new(my_plugin));
/// bus.init_all();
///
/// loop {
///     // Feed events from transport
///     bus.enqueue(transport_event);
///     // Poll plugins (ISR batches, timers)
///     bus.poll_plugins();
///     // Process all pending events
///     bus.tick();
///     // Collect outbound events for transport
///     for event in bus.drain_outbound() {
///         transport.send(event);
///     }
/// }
/// ```
pub struct EventBus {
    sentants: Vec<SentantSlot>,
    plugins: Vec<PluginSlot>,
    queue: EventQueue,
    /// Events that need to go to transport (remote targets).
    outbound: Vec<QueuedEvent>,
    /// Reusable action buffer.
    action_buf: ActionBuf,
    /// Timer registry for delayed sends (R2-SENTANT §3.1.5).
    timers: TimerRegistry,
}

impl EventBus {
    /// Create a new empty event bus.
    pub fn new() -> Self {
        Self {
            sentants: Vec::with_capacity(MAX_SENTANTS),
            plugins: Vec::with_capacity(MAX_PLUGINS),
            queue: EventQueue::new(),
            outbound: Vec::with_capacity(16),
            action_buf: ActionBuf::new(),
            timers: TimerRegistry::new(),
        }
    }

    /// Register a sentant. Returns its assigned ID.
    pub fn register_sentant(&mut self, sentant: Box<dyn Sentant>) -> SentantId {
        let id = self.sentants.len() as SentantId;
        let subscriptions = sentant.subscriptions().to_vec();
        self.sentants.push(SentantSlot {
            sentant,
            id,
            subscriptions,
        });
        id
    }

    /// Register a plugin. Returns its assigned ID.
    pub fn register_plugin(&mut self, plugin: Box<dyn Plugin>) -> PluginId {
        let id = self.plugins.len() as PluginId;
        self.plugins.push(PluginSlot { plugin, id });
        id
    }

    /// Initialise all sentants and plugins.
    ///
    /// Call once after registration, before the main loop.
    pub fn init_all(&mut self) {
        // Init plugins first (hardware setup)
        for slot in &mut self.plugins {
            slot.plugin.init();
        }

        // Init sentants (may produce actions like start timers)
        for i in 0..self.sentants.len() {
            self.action_buf.clear();
            self.sentants[i].sentant.init(&mut self.action_buf);
            self.process_actions(i as SentantId);
        }
    }

    /// Enqueue an event from transport or external source.
    pub fn enqueue(&mut self, event: QueuedEvent) -> bool {
        self.queue.push(event)
    }

    /// Poll all plugins for events (ISR batches, timers, etc.).
    pub fn poll_plugins(&mut self) {
        for slot in &mut self.plugins {
            if let Some((hash, payload)) = slot.plugin.poll() {
                let event = QueuedEvent::new(
                    hash,
                    0xFF, // plugin source
                    false,
                    0,
                    payload,
                );
                self.queue.push(event);
            }
        }
    }

    /// Process all pending events in the queue.
    ///
    /// For each event, finds subscribed sentants and calls their handlers.
    /// Actions produced by handlers are executed immediately (which may
    /// enqueue more events for the next tick).
    pub fn tick(&mut self) {
        // Process up to queue capacity events per tick to avoid infinite loops
        let mut budget = 64u32;

        while let Some(queued) = self.queue.pop() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                break;
            }

            let event = Event {
                hash: queued.hash,
                payload: queued.payload(),
                source: if queued.remote {
                    EventSource::Remote(queued.remote_rbid)
                } else if queued.source_id == 0xFF {
                    EventSource::Plugin(0)
                } else {
                    EventSource::Local(queued.source_id)
                },
                msg_id: queued.msg_id,
            };

            // Find all sentants subscribed to this event hash
            let subscriber_ids: Vec<SentantId> = self
                .sentants
                .iter()
                .filter(|s| s.subscriptions.contains(&event.hash))
                .map(|s| s.id)
                .collect();

            for sid in subscriber_ids {
                let idx = sid as usize;
                if idx >= self.sentants.len() {
                    continue;
                }

                self.action_buf.clear();
                self.sentants[idx]
                    .sentant
                    .handle_event(&event, &mut self.action_buf);
                self.process_actions(sid);
            }
        }
    }

    /// Advance timers by `elapsed_ms` milliseconds and dispatch any that fire.
    ///
    /// The platform layer should call this regularly (e.g. every tick or
    /// every N ms from a hardware timer). Fired timers are dispatched as
    /// if the delayed send happened now.
    pub fn advance_time(&mut self, elapsed_ms: u32) {
        let fired = self.timers.advance(elapsed_ms);
        for t in fired {
            self.dispatch_send(t.source_id, t.target, t.event_hash, t.payload.as_slice());
        }
    }

    /// Number of pending delayed sends.
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    /// Drain outbound events (for transport to send to remote hives).
    pub fn drain_outbound(&mut self) -> Vec<QueuedEvent> {
        core::mem::take(&mut self.outbound)
    }

    /// Process actions produced by a sentant handler.
    fn process_actions(&mut self, source_id: SentantId) {
        // Drain actions into a local vec to avoid borrow issues
        let actions: Vec<Action> = self.action_buf.drain().collect();

        for action in actions {
            match action {
                Action::Send {
                    target,
                    event_hash,
                    payload,
                } => {
                    self.dispatch_send(source_id, target, event_hash, payload.as_slice());
                }

                Action::Transition(new_state) => {
                    // State transitions are handled internally by the sentant
                    // (it mutates its own state in handle_event).
                    // This action is for logging/audit only.
                    let _ = new_state;
                }

                Action::PluginCall {
                    plugin_id,
                    command,
                    data,
                } => {
                    if let Some(slot) = self.plugins.iter_mut().find(|s| s.id == plugin_id) {
                        let _result = slot.plugin.execute(command, data.as_slice());
                        // Plugin results can be fed back as events if needed
                    }
                }

                Action::DelayedSend {
                    delay_ms,
                    target,
                    event_hash,
                    payload,
                } => {
                    if delay_ms == 0 {
                        self.dispatch_send(source_id, target, event_hash, payload.as_slice());
                    } else {
                        self.timers.schedule(
                            source_id,
                            event_hash,
                            target,
                            payload.as_slice(),
                            delay_ms,
                        );
                    }
                }

                Action::Log { level: _, message: _message } => {
                    #[cfg(feature = "std")]
                    {
                        let msg = core::str::from_utf8(_message.as_slice()).unwrap_or("?");
                        let name = if (source_id as usize) < self.sentants.len() {
                            self.sentants[source_id as usize].sentant.name()
                        } else {
                            "?"
                        };
                        // Use println since we can't depend on log crate without std
                        println!("[{}] {}", name, msg);
                    }
                }
            }
        }
    }

    /// Route a Send action to its target(s).
    fn dispatch_send(
        &mut self,
        source_id: SentantId,
        target: Target,
        event_hash: u32,
        payload: &[u8],
    ) {
        match target {
            Target::Sentant(_sid) => {
                // Direct send to a specific local sentant
                self.queue.push(QueuedEvent::new(
                    event_hash, source_id, false, 0, payload,
                ));
            }
            Target::Local => {
                // Dispatch to all local subscribers (enqueue for next tick)
                self.queue.push(QueuedEvent::new(
                    event_hash, source_id, false, 0, payload,
                ));
            }
            Target::TrustGroup | Target::Broadcast => {
                // Local delivery
                self.queue.push(QueuedEvent::new(
                    event_hash, source_id, false, 0, payload,
                ));
                // Also queue for outbound transport
                self.outbound.push(QueuedEvent::new(
                    event_hash, source_id, false, 0, payload,
                ));
            }
            Target::Sender => {
                // Route back to whoever sent the triggering event.
                // For now, enqueue locally (the engine would need to
                // track the current event source for proper routing).
                self.outbound.push(QueuedEvent::new(
                    event_hash, source_id, false, 0, payload,
                ));
            }
        }
    }
}
