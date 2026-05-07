use super::*;

use crate::dedup::DedupCache;
use r2_wire::types::{CompactRouteStack, MsgType};
use serde_json::Value;

const ROUTE_VECTORS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../r2-specifications/testing/test-vectors/r2-route-vectors.json"
));

fn vectors() -> Value {
    serde_json::from_str(ROUTE_VECTORS_JSON).expect("valid route vectors JSON")
}

fn vector(id: &str) -> Value {
    let data = vectors();
    data["vectors"]
        .as_array()
        .expect("vectors array")
        .iter()
        .find(|item| item["id"].as_str() == Some(id))
        .cloned()
        .unwrap_or_else(|| panic!("missing vector {id}"))
}

fn approx_eq(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-3, "{a} vs {b}");
}

#[test]
fn vector_rssi_mapping() {
    let v = vector("ROUTE-RSSI-LINEAR");
    let rssi = v["input"]["rssi"].as_i64().unwrap() as i8;
    let expected = v["expected"]["quality"].as_f64().unwrap() as f32;
    approx_eq(quality_from_rssi(rssi), expected);
}

#[test]
fn neighbour_decay_vector() {
    let v = vector("ROUTE-NEIGH-DECAY");
    let elapsed = v["input"]["elapsed"].as_u64().unwrap() as u32;
    let mut entry = NeighbourEntry::EMPTY;
    entry.confidence = v["input"]["confidence"].as_f64().unwrap() as f32;
    entry.last_seen = 0;
    entry.mobility = MobilityClass::Mobile;
    entry.decay(elapsed);
    let expected = v["expected"]["confidence"].as_f64().unwrap() as f32;
    approx_eq(entry.confidence, expected);
}

#[test]
fn path_positive_vector() {
    let v = vector("ROUTE-PATH-POS");
    let mut table: PathTable<4> = PathTable::new();
    table.seed(
        0x11,
        0x22,
        0,
        v["input"]["confidence"].as_f64().unwrap() as f32,
    );
    table.record_positive(0x11, 0x22, 1);
    let entry = table.best_for(0x11).unwrap();
    let expected = v["expected"]["confidence"].as_f64().unwrap() as f32;
    approx_eq(entry.confidence, expected);
}

#[test]
fn path_decay_vector() {
    let v = vector("ROUTE-PATH-DECAY");
    let mut table: PathTable<4> = PathTable::new();
    table.seed(
        0x33,
        0x44,
        0,
        v["input"]["confidence"].as_f64().unwrap() as f32,
    );
    table.decay(v["input"]["elapsed"].as_u64().unwrap() as u32);
    let entry = table.best_for(0x33).unwrap();
    let expected = v["expected"]["confidence"].as_f64().unwrap() as f32;
    approx_eq(entry.confidence, expected);
}

#[test]
fn hop_budget_vectors() {
    let split = vector("ROUTE-K-SPLIT");
    let budget = hop::enforce_ttl_k(
        split["input"]["ttl"].as_u64().unwrap() as u8,
        split["input"]["k"].as_u64().unwrap() as u8,
        false,
        false,
    )
    .unwrap();
    assert_eq!(budget.ttl, split["expected"]["ttl"].as_u64().unwrap() as u8);
    assert_eq!(
        budget.forwarded_k,
        split["expected"]["forwarded"].as_u64().unwrap() as u8
    );
    assert_eq!(
        budget.retained_k,
        split["expected"]["retained"].as_u64().unwrap() as u8
    );

    let flood = vector("ROUTE-K-FLOOD");
    let budget = hop::enforce_ttl_k(
        flood["input"]["ttl"].as_u64().unwrap() as u8,
        flood["input"]["k"].as_u64().unwrap() as u8,
        false,
        false,
    )
    .unwrap();
    assert!(budget.flood_mode);
    assert_eq!(budget.forwarded_k, constants::FLOOD_SENTINEL_K);
    assert_eq!(budget.retained_k, constants::FLOOD_SENTINEL_K);
}

#[test]
fn dedup_cache_behaviour() {
    let mut cache: DedupCache<8> = DedupCache::new();
    assert!(!cache.is_duplicate(0, 0x1001, 0xAA));
    assert!(cache.is_duplicate(10, 0x1001, 0xAA));
    // after TTL
    assert!(!cache.is_duplicate(100, 0x1001, 0xAA));
}

#[test]
fn route_stack_append_pop() {
    let mut missing: Option<CompactRouteStack> = None;
    assert!(matches!(
        append_compact(&mut missing, 0xAABB0000),
        Err(RouteStackError::Missing)
    ));

    let mut stack = Some(CompactRouteStack::new());
    append_compact(&mut stack, 0xAABBCCDD).unwrap();
    let mut holder = stack.take();
    append_compact(&mut holder, 0xEEFF0011).unwrap();
    let mut route = holder.unwrap();
    let next = pop_for_reply_compact(&mut route, compress_hive_id_16(0xEEFF0011)).unwrap();
    assert_eq!(next, Some(compress_hive_id_16(0xAABBCCDD)));
}

#[test]
fn route_engine_directed_and_flood() {
    let mut engine: RouteEngine = RouteEngine::new();
    engine.ingest_observation(Observation {
        hive_id: 0xAAAA0001,
        transport: Transport::Lora,
        timestamp: 0,
        quality: QualitySample::Direct(1.0),
        rssi: Some(-40),
        mcu_origin: false,
        mobility: MobilityClass::Infrastructure,
    });
    engine.record_delivery_success(0xBEEF0002, 0xAAAA0001, 1);

    let advice = engine.plan_forward(ForwardRequest {
        now: 10,
        msg_id: 0x10,
        source_hop: 0xFFFF,
        ttl: 5,
        k: 4,
        destination: Target::Address(0xBEEF0002),
        msg_type: MsgType::Event,
        payload_len: 32,
        relay_enabled: true,
        congested: false,
        dice_roll: 0.0,
    });
    match advice.action {
        ForwardAction::Directed(hop) => assert_eq!(hop.neighbour, 0xAAAA0001),
        other => panic!("expected directed, got {other:?}"),
    }

    let advice = engine.plan_forward(ForwardRequest {
        now: 20,
        msg_id: 0x11,
        source_hop: 0xABCD,
        ttl: 5,
        k: constants::FLOOD_SENTINEL_K,
        destination: Target::Broadcast,
        msg_type: MsgType::Event,
        payload_len: 16,
        relay_enabled: true,
        congested: false,
        dice_roll: 0.0,
    });
    match advice.action {
        ForwardAction::Flood(hops) => assert!(!hops.is_empty()),
        other => panic!("expected flood, got {other:?}"),
    }
}
