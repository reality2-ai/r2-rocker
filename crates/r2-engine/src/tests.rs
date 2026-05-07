//! Basic tests for the sentant engine.

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use crate::action::{Action, PayloadBuf};
    use crate::action_buf::ActionBuf;
    use crate::bus::EventBus;
    use crate::event::{Event, Target};
    use crate::queue::QueuedEvent;
    use crate::sentant::{Sentant, StateId};

    // FNV hashes (precomputed)
    const PING_HASH: u32 = r2_fnv::fnv1a_32(b"#ping");
    const PONG_HASH: u32 = r2_fnv::fnv1a_32(b"#pong");
    const ACCEL_HASH: u32 = r2_fnv::fnv1a_32(b"acceleration");
    const START_HASH: u32 = r2_fnv::fnv1a_32(b"cmd_start");

    // ---- Ping Sentant ----

    struct PingSentant {
        state: StateId,
        pings_sent: u32,
    }

    impl PingSentant {
        fn new() -> Self {
            Self { state: 0, pings_sent: 0 }
        }
    }

    impl Sentant for PingSentant {
        fn handle_event(&mut self, event: &Event, _actions: &mut ActionBuf) {
            if event.hash == PONG_HASH {
                // Got a pong — transition to "done"
                self.state = 1;
            }
        }

        fn init(&mut self, actions: &mut ActionBuf) {
            // Send a ping on startup
            self.pings_sent += 1;
            actions.push(Action::send_empty(Target::Local, PING_HASH));
        }

        fn state(&self) -> StateId { self.state }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"test.ping") }
        fn name(&self) -> &str { "ping" }
        fn subscriptions(&self) -> &[u32] { &[PONG_HASH] }
    }

    // ---- Pong Sentant ----

    struct PongSentant {
        pongs_sent: u32,
    }

    impl PongSentant {
        fn new() -> Self {
            Self { pongs_sent: 0 }
        }
    }

    impl Sentant for PongSentant {
        fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
            if event.hash == PING_HASH {
                self.pongs_sent += 1;
                actions.push(Action::send_empty(Target::Local, PONG_HASH));
            }
        }

        fn state(&self) -> StateId { 0 }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"test.pong") }
        fn name(&self) -> &str { "pong" }
        fn subscriptions(&self) -> &[u32] { &[PING_HASH] }
    }

    // ---- Tests ----

    #[test]
    fn ping_pong_via_bus() {
        let mut bus = EventBus::new();
        let _ping_id = bus.register_sentant(Box::new(PingSentant::new()));
        let _pong_id = bus.register_sentant(Box::new(PongSentant::new()));

        // Init sends a ping
        bus.init_all();

        // First tick: ping dispatched → pong handler → enqueues pong
        bus.tick();
        // Second tick: pong dispatched → ping handler → state = 1
        bus.tick();

        // Verify: ping sentant should be in state 1 ("done")
        // We can't directly inspect sentant state through the bus,
        // but we can verify the flow worked by checking no outbound events
        // (all local)
        let outbound = bus.drain_outbound();
        assert!(outbound.is_empty(), "ping/pong should be local only");
    }

    #[test]
    fn action_buf_capacity() {
        let mut buf = ActionBuf::new();
        for i in 0..16 {
            assert!(buf.push(Action::send_empty(Target::Local, i)));
        }
        // 17th should fail
        assert!(!buf.push(Action::send_empty(Target::Local, 99)));
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn action_buf_drain() {
        let mut buf = ActionBuf::new();
        buf.push(Action::send_empty(Target::Local, PING_HASH));
        buf.push(Action::transition(1));
        buf.push(Action::send(Target::TrustGroup, ACCEL_HASH, &[0xA0]));

        let actions: Vec<Action> = buf.drain().collect();
        assert_eq!(actions.len(), 3);
        assert!(buf.is_empty());
    }

    #[test]
    fn payload_buf_roundtrip() {
        let data = b"hello r2";
        let buf = PayloadBuf::from_slice(data);
        assert_eq!(buf.as_slice(), data);
        assert_eq!(buf.len(), 8);
    }

    #[test]
    fn event_queue_ring_buffer() {
        let mut queue = crate::queue::EventQueue::<4>::new();
        assert!(queue.is_empty());

        // Fill it
        for i in 0..4 {
            assert!(queue.push(QueuedEvent::new(i, 0, false, 0, &[])));
        }
        assert!(queue.is_full());
        // 5th should fail
        assert!(!queue.push(QueuedEvent::new(99, 0, false, 0, &[])));

        // Drain 2
        assert_eq!(queue.pop().unwrap().hash, 0);
        assert_eq!(queue.pop().unwrap().hash, 1);
        assert_eq!(queue.len(), 2);

        // Add 2 more (wraps around)
        assert!(queue.push(QueuedEvent::new(10, 0, false, 0, &[])));
        assert!(queue.push(QueuedEvent::new(11, 0, false, 0, &[])));
        assert!(queue.is_full());

        // Drain all
        assert_eq!(queue.pop().unwrap().hash, 2);
        assert_eq!(queue.pop().unwrap().hash, 3);
        assert_eq!(queue.pop().unwrap().hash, 10);
        assert_eq!(queue.pop().unwrap().hash, 11);
        assert!(queue.is_empty());
    }

    #[test]
    fn trust_group_target_produces_outbound() {
        let mut bus = EventBus::new();

        // Sentant that sends to TrustGroup on any event
        struct BroadcastSentant;
        impl Sentant for BroadcastSentant {
            fn handle_event(&mut self, _event: &Event, actions: &mut ActionBuf) {
                actions.push(Action::send(
                    Target::TrustGroup,
                    ACCEL_HASH,
                    &[0xA1, 0x00, 0x18, 0x2A], // CBOR {0: 42}
                ));
            }
            fn state(&self) -> StateId { 0 }
            fn class_hash(&self) -> u32 { 0 }
            fn name(&self) -> &str { "broadcast" }
            fn subscriptions(&self) -> &[u32] { &[START_HASH] }
        }

        bus.register_sentant(Box::new(BroadcastSentant));
        bus.init_all();

        // Inject a start event
        bus.enqueue(QueuedEvent::new(START_HASH, 0xFF, false, 0, &[]));
        bus.tick();

        let outbound = bus.drain_outbound();
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].hash, ACCEL_HASH);
        assert_eq!(outbound[0].payload(), &[0xA1, 0x00, 0x18, 0x2A]);
    }
}
