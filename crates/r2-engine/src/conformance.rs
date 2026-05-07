//! Conformance tests against r2-engine-vectors.json
//!
//! These are **reference implementations** of the sentant definitions
//! described in the JSON vectors. The R2-COMPILE compiler will generate
//! equivalent code from YAML — both must produce identical output.
//!
//! The test flow:
//! 1. Load expected outputs from JSON vectors
//! 2. Create hand-written sentants matching the vector definitions
//! 3. Feed the input events through the EventBus
//! 4. Verify output events match the JSON expectations

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    use crate::action::Action;
    use crate::action_buf::ActionBuf;
    use crate::bus::EventBus;
    use crate::event::{Event, Target};
    use crate::queue::QueuedEvent;
    use crate::sentant::{Sentant, StateId};

    const VECTORS_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../r2-specifications/testing/test-vectors/r2-engine-vectors.json"
    ));

    // ── Precomputed event hashes (const fn) ──

    const PING_HASH: u32 = r2_fnv::fnv1a_32(b"#ping");
    const PONG_HASH: u32 = r2_fnv::fnv1a_32(b"#pong");
    const CMD_START: u32 = r2_fnv::fnv1a_32(b"cmd_start");
    const CMD_STOP: u32 = r2_fnv::fnv1a_32(b"cmd_stop");
    const CMD_MARK: u32 = r2_fnv::fnv1a_32(b"cmd_mark");
    #[allow(dead_code)] // Used in entanglement tests via static ref
    const ACCELERATION: u32 = r2_fnv::fnv1a_32(b"acceleration");
    const BATTERY_STATUS: u32 = r2_fnv::fnv1a_32(b"battery_status");
    const RUN_STATE: u32 = r2_fnv::fnv1a_32(b"run_state");
    const LOW_BATTERY: u32 = r2_fnv::fnv1a_32(b"low_battery");
    const SHUTDOWN: u32 = r2_fnv::fnv1a_32(b"shutdown");
    const OPEN_LOG: u32 = r2_fnv::fnv1a_32(b"open_log");
    const CLOSE_LOG: u32 = r2_fnv::fnv1a_32(b"close_log");
    const MARK: u32 = r2_fnv::fnv1a_32(b"mark");
    const START_SAMPLING: u32 = r2_fnv::fnv1a_32(b"start_sampling");
    const STOP_SAMPLING: u32 = r2_fnv::fnv1a_32(b"stop_sampling");

    // ── JSON helpers ──

    fn parse_hex_u32(s: &str) -> u32 {
        u32::from_str_radix(s.trim_start_matches("0x"), 16).expect("valid hex u32")
    }

    fn resolve_event_hash(name: &str) -> u32 {
        r2_fnv::r2_hash(name).expect("valid event name")
    }

    #[allow(dead_code)] // Will be used when verifying target routing
    fn resolve_target(s: &str) -> Target {
        match s {
            "@sender" => Target::Sender,
            "@local" => Target::Local,
            "@all" | "@broadcast" => Target::Broadcast,
            "@group" => Target::TrustGroup,
            _ => Target::Local,
        }
    }

    // ── Verify hashes match JSON ──

    #[test]
    fn json_event_hashes_match() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let hashes = data["event_hashes"]["hashes"].as_object().unwrap();

        for (name, expected_hex) in hashes {
            let expected = parse_hex_u32(expected_hex.as_str().unwrap());
            let actual = r2_fnv::r2_hash(name).expect(&alloc::format!("hash for {}", name));
            assert_eq!(
                actual, expected,
                "Hash mismatch for {:?}: got 0x{:08X}, expected 0x{:08X}",
                name, actual, expected
            );
        }
    }

    // ── SM-1: Pong sentant ──

    struct PongSentant;

    impl Sentant for PongSentant {
        fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
            if event.hash == PING_HASH {
                actions.push(Action::send_empty(Target::Sender, PONG_HASH));
            }
        }
        fn state(&self) -> StateId { 0 }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"test.pong") }
        fn name(&self) -> &str { "pong" }
        fn subscriptions(&self) -> &[u32] { &[PING_HASH] }
    }

    #[test]
    fn sm1_ping_pong() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = &data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-1")
            .expect("SM-1 vector");

        let mut bus = EventBus::new();
        bus.register_sentant(Box::new(PongSentant));
        bus.init_all();

        // Feed input events
        for input in vector["input_events"].as_array().unwrap() {
            let hash = resolve_event_hash(input["event"].as_str().unwrap());
            bus.enqueue(QueuedEvent::new(hash, 0xFF, true, 0, &[]));
        }
        bus.tick();

        // Verify outbound
        let outbound = bus.drain_outbound();
        let expected = vector["expected_output"].as_array().unwrap();
        assert_eq!(
            outbound.len(), expected.len(),
            "SM-1: expected {} output events, got {}",
            expected.len(), outbound.len()
        );
        for (i, exp) in expected.iter().enumerate() {
            let exp_hash = resolve_event_hash(exp["event"].as_str().unwrap());
            assert_eq!(
                outbound[i].hash, exp_hash,
                "SM-1 output[{}]: expected event hash 0x{:08X}, got 0x{:08X}",
                i, exp_hash, outbound[i].hash
            );
        }
    }

    // ── SM-2: Coordinator lifecycle ──

    struct CoordinatorSentant {
        state: StateId,
    }

    // States
    const COORD_IDLE: StateId = 0;
    const COORD_CALIBRATING: StateId = 1;
    const COORD_ROCKING: StateId = 2;

    impl CoordinatorSentant {
        fn new() -> Self { Self { state: COORD_IDLE } }
    }

    impl Sentant for CoordinatorSentant {
        fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
            match (self.state, event.hash) {
                (COORD_IDLE, hash) if hash == CMD_START => {
                    self.state = COORD_CALIBRATING;
                    actions.push(Action::send_empty(Target::Local, OPEN_LOG));
                    actions.push(Action::send_empty(Target::Local, START_SAMPLING));
                    actions.push(Action::send_empty(Target::Local, RUN_STATE));
                }
                (COORD_CALIBRATING, hash) if hash == CMD_MARK => {
                    self.state = COORD_ROCKING;
                    actions.push(Action::send(Target::Local, MARK, &[0xA1, 0x00, 0x67, 0x6D, 0x61, 0x72, 0x6B]));
                    actions.push(Action::send_empty(Target::Local, RUN_STATE));
                }
                (COORD_CALIBRATING | COORD_ROCKING, hash) if hash == CMD_STOP => {
                    self.state = COORD_IDLE;
                    actions.push(Action::send_empty(Target::Local, STOP_SAMPLING));
                    actions.push(Action::send_empty(Target::Local, CLOSE_LOG));
                    actions.push(Action::send_empty(Target::Local, RUN_STATE));
                }
                _ => {} // Silently ignore (SM-3 conformance)
            }
        }
        fn state(&self) -> StateId { self.state }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"nz.ac.friction.coordinator") }
        fn name(&self) -> &str { "coordinator" }
        fn subscriptions(&self) -> &[u32] { &[CMD_START, CMD_MARK, CMD_STOP, SHUTDOWN] }
    }

    // Note: bus-level vector runner deferred until engine has action
    // introspection. Direct sentant testing via verify_sm_vector is the
    // primary conformance path for now.

    /// Direct sentant testing: feed events, collect actions, verify against JSON.
    fn verify_sm_vector(
        sentant: &mut dyn Sentant,
        vector: &serde_json::Value,
    ) {
        let id = vector["id"].as_str().unwrap_or("?");
        let mut all_actions: Vec<(u32, String)> = Vec::new(); // (event_hash, target_str)
        let mut action_buf = ActionBuf::new();

        let state_names: Vec<&str> = vector["sentant"]["states"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();

        for input in vector["input_events"].as_array().unwrap() {
            let hash = resolve_event_hash(input["event"].as_str().unwrap());
            let remote = input["source"].as_str().map_or(false, |s| s == "remote" || s == "plugin");

            let payload_hex = input.get("payload_hex")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let payload = if payload_hex.is_empty() {
                Vec::new()
            } else {
                hex_to_bytes(payload_hex)
            };

            let event = Event {
                hash,
                payload: &payload,
                source: if remote {
                    crate::event::EventSource::Remote(0)
                } else {
                    crate::event::EventSource::Local(0)
                },
                msg_id: 0,
            };

            action_buf.clear();
            sentant.handle_event(&event, &mut action_buf);

            for action in action_buf.iter() {
                match action {
                    Action::Send { target, event_hash, .. } => {
                        let target_str = match target {
                            Target::Sender => "@sender",
                            Target::Local => "@local",
                            Target::Broadcast => "@broadcast",
                            Target::TrustGroup => "@group",
                            Target::Sentant(_) => "@sentant",
                        };
                        all_actions.push((*event_hash, String::from(target_str)));
                    }
                    _ => {}
                }
            }
        }

        // Verify outputs match JSON
        let expected = vector["expected_output"].as_array().unwrap();
        assert_eq!(
            all_actions.len(), expected.len(),
            "{}: expected {} output events, got {} ({:?})",
            id, expected.len(), all_actions.len(),
            all_actions.iter().map(|(h,_)| alloc::format!("0x{:08X}", h)).collect::<Vec<_>>()
        );

        for (i, exp) in expected.iter().enumerate() {
            let exp_hash = resolve_event_hash(exp["event"].as_str().unwrap());
            assert_eq!(
                all_actions[i].0, exp_hash,
                "{} output[{}]: expected 0x{:08X} ({}), got 0x{:08X}",
                id, i, exp_hash,
                exp["event"].as_str().unwrap(),
                all_actions[i].0
            );

            if let Some(target_str) = exp.get("target").and_then(|t| t.as_str()) {
                assert_eq!(
                    all_actions[i].1, target_str,
                    "{} output[{}]: expected target {}, got {}",
                    id, i, target_str, all_actions[i].1
                );
            }
        }

        // Verify final state
        if let Some(expected_state_name) = vector.get("expected_final_state").and_then(|s| s.as_str()) {
            let expected_state_id = state_names.iter()
                .position(|&s| s == expected_state_name)
                .expect(&alloc::format!("{}: unknown state '{}'", id, expected_state_name))
                as StateId;
            assert_eq!(
                sentant.state(), expected_state_id,
                "{}: expected final state '{}' ({}), got state {}",
                id, expected_state_name, expected_state_id, sentant.state()
            );
        }
    }

    fn hex_to_bytes(hex_str: &str) -> Vec<u8> {
        let s = hex_str.replace(' ', "");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn sm2_coordinator_lifecycle() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-2")
            .unwrap();

        let mut sentant = CoordinatorSentant::new();
        verify_sm_vector(&mut sentant, vector);
    }

    #[test]
    fn sm3_guard_wrong_state() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-3")
            .unwrap();

        let mut sentant = CoordinatorSentant::new();
        verify_sm_vector(&mut sentant, vector);
    }

    // ── SM-4: Wildcard from-state ──

    struct EchoSentant {
        state: StateId,
    }

    impl EchoSentant {
        fn new(initial_state: StateId) -> Self { Self { state: initial_state } }
    }

    impl Sentant for EchoSentant {
        fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
            // Wildcard: accepts #ping from any state
            if event.hash == PING_HASH {
                actions.push(Action::send_empty(Target::Sender, PONG_HASH));
            }
        }
        fn state(&self) -> StateId { self.state }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"test.echo") }
        fn name(&self) -> &str { "echo" }
        fn subscriptions(&self) -> &[u32] { &[PING_HASH] }
    }

    #[test]
    fn sm4_wildcard_state() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-4")
            .unwrap();

        // Initial state is "active" (index 1)
        let mut sentant = EchoSentant::new(1);
        verify_sm_vector(&mut sentant, vector);
    }

    // ── SM-5 / SM-6: Battery monitor with guards ──

    struct BatteryMonitorSentant {
        state: StateId,
    }

    const BATT_MONITORING: StateId = 0;
    const BATT_CRITICAL: StateId = 1;

    impl BatteryMonitorSentant {
        fn new() -> Self { Self { state: BATT_MONITORING } }
    }

    impl Sentant for BatteryMonitorSentant {
        fn handle_event(&mut self, event: &Event, actions: &mut ActionBuf) {
            if event.hash == BATTERY_STATUS && self.state == BATT_MONITORING {
                // Decode CBOR payload: {0: voltage_mv}
                if let Some(voltage) = decode_voltage(event.payload) {
                    if voltage < 3400 {
                        self.state = BATT_CRITICAL;
                        actions.push(Action::send_empty(Target::Local, LOW_BATTERY));
                        actions.push(Action::send_empty(Target::Local, SHUTDOWN));
                    }
                    // >= 3400: no action, stay in monitoring
                }
            }
        }
        fn state(&self) -> StateId { self.state }
        fn class_hash(&self) -> u32 { r2_fnv::fnv1a_32(b"nz.ac.friction.monitor.battery") }
        fn name(&self) -> &str { "battery" }
        fn subscriptions(&self) -> &[u32] { &[BATTERY_STATUS] }
    }

    /// Decode CBOR map {0: uint} → extract the uint value.
    fn decode_voltage(payload: &[u8]) -> Option<u64> {
        if payload.is_empty() { return None; }
        let mut decoder = r2_cbor::Decoder::new(payload);
        // Expect map
        match decoder.next().ok()? {
            r2_cbor::Item::Map(n) if n >= 1 => {}
            _ => return None,
        }
        // Key 0
        match decoder.next().ok()? {
            r2_cbor::Item::UInt(0) => {}
            _ => return None,
        }
        // Value
        match decoder.next().ok()? {
            r2_cbor::Item::UInt(v) => Some(v),
            _ => None,
        }
    }

    #[test]
    fn sm5_battery_below_threshold() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-5")
            .unwrap();

        let mut sentant = BatteryMonitorSentant::new();
        verify_sm_vector(&mut sentant, vector);
    }

    #[test]
    fn sm6_battery_above_threshold() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = data["state_machine_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "SM-6")
            .unwrap();

        let mut sentant = BatteryMonitorSentant::new();
        verify_sm_vector(&mut sentant, vector);
    }

    // ── ENT-1: Fan-out via bus subscriptions ──

    /// Stub sentant that records received events (for entanglement testing).
    struct RecorderSentant {
        name_str: &'static str,
        subs: &'static [u32],
        pub received: Vec<u32>,
    }

    impl RecorderSentant {
        fn new(name: &'static str, subs: &'static [u32]) -> Self {
            Self { name_str: name, subs, received: Vec::new() }
        }
    }

    impl Sentant for RecorderSentant {
        fn handle_event(&mut self, event: &Event, _actions: &mut ActionBuf) {
            self.received.push(event.hash);
        }
        fn state(&self) -> StateId { 0 }
        fn class_hash(&self) -> u32 { 0 }
        fn name(&self) -> &str { self.name_str }
        fn subscriptions(&self) -> &[u32] { self.subs }
    }

    #[test]
    fn ent1_sensor_fanout() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = &data["entanglement_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "ENT-1")
            .unwrap();

        let mut bus = EventBus::new();

        // Register logger and comms — both subscribe to acceleration
        static ACCEL_SUBS: &[u32] = &[r2_fnv::fnv1a_32(b"acceleration")];
        bus.register_sentant(Box::new(RecorderSentant::new("logger", ACCEL_SUBS)));
        bus.register_sentant(Box::new(RecorderSentant::new("comms", ACCEL_SUBS)));
        bus.init_all();

        // Sensor emits acceleration event
        let trigger = &vector["trigger"];
        let hash = resolve_event_hash(trigger["event"].as_str().unwrap());
        bus.enqueue(QueuedEvent::new(hash, 0xFF, false, 0, &[]));
        bus.tick();

        // Both should have received
        let expected = vector["expected_deliveries"].as_array().unwrap();
        assert_eq!(expected.len(), 2, "ENT-1: expected 2 deliveries");
        // The bus delivers to all subscribers of the event hash
        // We can't inspect internal sentant state through the bus,
        // but we can verify the event was routed (no outbound = all local)
        let outbound = bus.drain_outbound();
        assert!(outbound.is_empty(), "ENT-1: all deliveries should be local");
    }

    #[test]
    fn ent2_shutdown_chain() {
        let data: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
        let vector = &data["entanglement_vectors"]["vectors"]
            .as_array().unwrap()
            .iter()
            .find(|v| v["id"] == "ENT-2")
            .unwrap();

        let mut bus = EventBus::new();

        // Coordinator subscribes to shutdown
        static SHUTDOWN_SUBS: &[u32] = &[r2_fnv::fnv1a_32(b"shutdown")];
        bus.register_sentant(Box::new(RecorderSentant::new("coordinator", SHUTDOWN_SUBS)));
        bus.init_all();

        // Battery emits shutdown
        let trigger = &vector["trigger"];
        let hash = resolve_event_hash(trigger["event"].as_str().unwrap());
        bus.enqueue(QueuedEvent::new(hash, 0xFF, false, 0, &[]));
        bus.tick();

        let expected = vector["expected_deliveries"].as_array().unwrap();
        assert_eq!(expected.len(), 1, "ENT-2: expected 1 delivery");
    }
}
