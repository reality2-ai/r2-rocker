//! Phase 5 / SPEC-R2-ROCKER-ACCESS §5.2 — off-network viewer path.
//!
//! When the dashboard is started with `--relay-url`, this module
//! opens a persistent WebSocket to the R2 relay and:
//!
//!   * authenticates as the KeyHolder (HELLO signed with `tg_priv`),
//!   * publishes every R2-WIRE frame the sensors send to /r2
//!     out via the relay so viewer browsers connected to the same
//!     trust-group bucket see them in real time,
//!   * (future) listens for inbound viewer→controller frames and
//!     routes them through the existing per-peer dispatch.
//!
//! Wire protocol matches `/mnt/data/Development/R2/r2-relay/src/protocol.rs`
//! exactly — same HELLO + binary-frame fan-out that notekeeper's
//! browser-side `R2Member::sign_relay_hello` produces. The relay is
//! a dumb forwarder by trust-group hash; end-to-end crypto is the
//! caller's job (frame HMACs via R2-WIRE / R2-TRUST).

use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Result<T> = std::result::Result<T, String>;

/// Reconnect backoff bounds (matches the firmware sender's policy).
const RECONNECT_MIN_MS: u64 = 1_000;
const RECONNECT_MAX_MS: u64 = 30_000;
/// Client-side keepalive — relay's tolerance is 60s per
/// CLOSE_HEARTBEAT_TIMEOUT in r2-relay; ping every 25 s.
const PING_INTERVAL_MS: u64 = 25_000;

/// Spawn the relay-session task. Owns its WebSocket; subscribes to
/// `raw_frame_rx` for outbound frames; runs until the process exits.
///
/// `signing_key` is the TG signing key (loaded from `tg_priv.bin`).
/// `tg_pub` is its public key (the HELLO `device_id`, also used to
/// derive the trust-group hash).
pub fn spawn_relay_session(
    relay_url: String,
    signing_key: SigningKey,
    tg_pub: [u8; 32],
    raw_frame_tx: broadcast::Sender<crate::RawFrame>,
    binary_tx: broadcast::Sender<Vec<u8>>,
    state: std::sync::Arc<crate::AppState>,
) {
    tokio::spawn(async move {
        let mut backoff_ms = RECONNECT_MIN_MS;
        loop {
            match run_one_session(&relay_url, &signing_key, &tg_pub, &raw_frame_tx, &binary_tx, &state).await {
                Ok(()) => {
                    eprintln!("[relay] session ended cleanly — reconnecting in {} ms", backoff_ms);
                }
                Err(e) => {
                    eprintln!("[relay] session error: {e:#} — reconnect in {} ms", backoff_ms);
                }
            }
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(RECONNECT_MAX_MS);
        }
    });
}

async fn run_one_session(
    relay_url: &str,
    signing_key: &SigningKey,
    tg_pub: &[u8; 32],
    raw_frame_tx: &broadcast::Sender<crate::RawFrame>,
    binary_tx: &broadcast::Sender<Vec<u8>>,
    state: &std::sync::Arc<crate::AppState>,
) -> Result<()> {
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(relay_url)
        .await
        .map_err(|e| format!("connect_async {relay_url}: {e}"))?;
    eprintln!("[relay] WebSocket open → {relay_url}");

    let (mut sink, mut stream) = ws_stream.split();

    // HELLO. trust_group is the first 8 bytes of SHA-256(tg_pub) as
    // 16-hex chars (matches r2-relay's expected "standard HELLO"
    // form and r2-wasm's R2Member::trust_group_hash).
    let tg_hash = {
        let mut h = Sha256::new();
        h.update(tg_pub);
        let d = h.finalize();
        hex::encode(&d[..8])
    };
    let device_id_hex = hex::encode(tg_pub);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let msg = format!("{}:{}:{}", tg_hash, device_id_hex, timestamp);
    let signature = signing_key.sign(msg.as_bytes());
    let signature_hex = hex::encode(signature.to_bytes());

    let hello = serde_json::json!({
        "type": "hello",
        "version": 1,
        "trust_group": tg_hash,
        "device_id":   device_id_hex,
        "timestamp":   timestamp,
        "signature":   signature_hex,
    });
    sink.send(WsMessage::Text(hello.to_string().into()))
        .await
        .map_err(|e| format!("send HELLO: {e}"))?;

    // Wait for WELCOME (or auth failure → close frame).
    match stream.next().await {
        Some(Ok(WsMessage::Text(t))) => {
            // Parse welcome — we don't actually inspect it beyond
            // logging. Anything that's not an error close means OK.
            eprintln!("[relay] WELCOME: {}", t.trim());
        }
        Some(Ok(WsMessage::Close(frame))) => {
            return Err(format!(
                "relay closed during handshake: {:?}",
                frame
            ));
        }
        Some(Ok(other)) => {
            return Err(format!(
                "unexpected first frame from relay: {other:?}"
            ));
        }
        Some(Err(e)) => return Err(format!("relay stream error: {e}")),
        None => return Err(format!("relay closed before WELCOME")),
    }

    // Catchup: ask the relay to replay every binary frame from the
    // last ~60 seconds. After a brief disconnect (e.g. relay-side
    // backpressure / ConnReset from sensor-frame flood), the phone's
    // JOIN_REQUEST may already be sitting in the per-bucket buffer.
    // Without a catchup request, those frames stay buffered and never
    // reach our binary-recv arm. r2-relay's catchup handler is at
    // r2-relay/src/ws.rs:76-91; the wire form is plain text JSON.
    let catchup_since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(60))
        .unwrap_or(0);
    let catchup_msg = serde_json::json!({
        "type": "catchup",
        "since": catchup_since,
    }).to_string();
    sink.send(WsMessage::Text(catchup_msg.into()))
        .await
        .map_err(|e| format!("send catchup: {e}"))?;
    eprintln!("[relay] sent catchup since={catchup_since}");

    // Outbound: subscribe to the dashboard's raw-frame broadcast.
    // Every R2-WIRE frame the dashboard forwards to /r2 also
    // gets pushed up to the relay. Viewers in the same TG bucket
    // see it in real time.
    let mut raw_rx    = raw_frame_tx.subscribe();
    let mut binary_rx = binary_tx.subscribe();

    // Per-sensor outbound rate limit. The dashboard fan-outs every
    // sensor frame (~100 Hz × N sensors = 200+ Hz) to the relay,
    // which is enough to trigger backpressure / ConnReset on the
    // public r2-relay (we saw OS error 104 mid-session before this
    // throttle landed). Viewers over the relay don't need full-rate
    // acceleration — they're paired devices monitoring the rig from
    // outside the hotspot, where ~10 Hz is plenty and the SD ring
    // is the source of truth for high-fidelity history.
    //
    // Rule: drop a frame for a given src_addr if its last forwarded
    // frame was less than RELAY_FRAME_MIN_INTERVAL_MS ago. Per
    // sensor (not global) so independent senders aren't blocked by
    // each other. Cheap HashMap, no allocator hotspot — keys are
    // SocketAddr-as-String (the existing `rf.src` field).
    const RELAY_FRAME_MIN_INTERVAL_MS: u128 = 100; // 10 Hz per sensor
    let mut last_forward_at: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    // Debug counters — logged every 50 frames so we can verify the
    // path is alive without spamming the log.
    let mut frames_seen = 0u64;
    let mut frames_forwarded = 0u64;
    let mut frames_skipped = 0u64;

    let mut ping_tick = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_tick.tick().await; // skip the immediate fire

    loop {
        tokio::select! {
            // Outbound: frame arrived from a sensor → forward to relay
            // (rate-limited per src_addr, see RELAY_FRAME_MIN_INTERVAL_MS).
            res = raw_rx.recv() => {
                match res {
                    Ok(rf) => {
                        frames_seen += 1;
                        let now = std::time::Instant::now();
                        let skip = match last_forward_at.get(&rf.src) {
                            Some(prev) => now.duration_since(*prev).as_millis() < RELAY_FRAME_MIN_INTERVAL_MS,
                            None => false,
                        };
                        if skip {
                            frames_skipped += 1;
                            continue;
                        }
                        last_forward_at.insert(rf.src.clone(), now);
                        // Wrap the same envelope shape /r2 uses
                        // so viewers can decode it with the existing
                        // path. See encode_raw_frame_envelope in
                        // main.rs.
                        let envelope = crate::encode_raw_frame_envelope(&rf);
                        if let Err(e) = sink.send(WsMessage::Binary(envelope.into())).await {
                            return Err(format!("write outbound frame: {e}"));
                        }
                        frames_forwarded += 1;
                        if frames_forwarded % 50 == 0 {
                            eprintln!("[relay] frames_seen={frames_seen} forwarded={frames_forwarded} skipped={frames_skipped}");
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[relay] raw_frame_rx lagged by {n} frames");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(format!("raw_frame_rx closed"));
                    }
                }
            }

            // Inbound: drain anything the relay sends (pings, peer
            // frames, etc.). v0.1 only handles ping/pong + close.
            res = stream.next() => {
                match res {
                    Some(Ok(WsMessage::Ping(p))) => {
                        eprintln!("[relay] ping in ({} bytes)", p.len());
                        let _ = sink.send(WsMessage::Pong(p)).await;
                    }
                    Some(Ok(WsMessage::Pong(_))) => {
                        eprintln!("[relay] pong in");
                    }
                    Some(Ok(WsMessage::Text(t))) => {
                        // r2-relay only inspects text for `ping` /
                        // `catchup`; everything else from peers is
                        // dropped without forwarding (r2-relay
                        // ws.rs:67-93). The only text we should see
                        // here is the relay's own pong / informational
                        // frames. Log anything unexpected.
                        if !t.contains("\"pong\"") {
                            eprintln!("[relay] text in: {}", t.trim());
                        }
                    }
                    Some(Ok(WsMessage::Binary(b))) => {
                        // Binary frames from other peers. Two shapes:
                        //   1. Sensor R2-WIRE envelope from another
                        //      dashboard (cross-controller bridging —
                        //      not used in v0.1, drop).
                        //   2. Access-protocol JOIN_REQUEST frame in
                        //      notekeeper's wire format:
                        //        [0xFF, 0x01, devicePk(32),
                        //         joinCode(16), name(rest)]
                        //      Matches r2-notekeeper/index.html so
                        //      r2-rocker speaks the same join protocol
                        //      on the same relay path. Operator-side
                        //      gate (submit_request → approve in Link
                        //      tab) is the r2-rocker addition.
                        if is_join_request(&b) {
                            eprintln!("[relay] JOIN_REQUEST in ({} bytes)", b.len());
                            handle_join_request(state, &b).await;
                        } else if b.first() == Some(&0xFF) {
                            eprintln!(
                                "[relay] 0xFF frame opcode={:?} in ({} bytes)",
                                b.get(1), b.len()
                            );
                        } else {
                            eprintln!("[relay] bin in ({} bytes, not a join frame)", b.len());
                        }
                    }
                    Some(Ok(WsMessage::Close(frame))) => {
                        return Err(format!("relay closed: {frame:?}"));
                    }
                    Some(Ok(WsMessage::Frame(_))) => {}
                    Some(Err(e)) => return Err(format!("ws read: {e}")),
                    None => return Err(format!("ws stream ended")),
                }
            }

            // Outbound JOIN_RESPONSE: anything pushed to
            // AppState.relay_binary_tx (e.g. access_approve_handler
            // building the [0xFF, 0x02, ...] frame) goes out as a
            // straight binary WS frame. Matches notekeeper's wire
            // format on the relay.
            res = binary_rx.recv() => {
                match res {
                    Ok(frame) => {
                        eprintln!("[relay] JOIN_RESPONSE out ({} bytes)", frame.len());
                        if let Err(e) = sink.send(WsMessage::Binary(frame.into())).await {
                            return Err(format!("write join_response: {e}"));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[relay] binary_rx lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("binary_rx closed".to_string());
                    }
                }
            }

            // Keepalive — ping the relay every 25 s so it doesn't
            // close us at the 60-s heartbeat-timeout boundary.
            _ = ping_tick.tick() => {
                let payload = serde_json::json!({"type":"ping"}).to_string();
                if let Err(e) = sink.send(WsMessage::Text(payload.into())).await {
                    return Err(format!("send ping: {e}"));
                }
            }
        }
    }
}

/// Join-protocol wire constants — match r2-notekeeper byte for byte
/// (notekeeper/index.html:1336-1338). r2-relay forwards every binary
/// frame between peers in the same TG bucket; we tag join frames with
/// `0xFF` as the first byte so they're unambiguous against sensor
/// R2-WIRE envelopes (which start with `[u16 BE src_addr_len]`, first
/// byte always 0x00 because src_addr is ≤ 64 chars).
///
/// Frame layouts:
///   JOIN_REQUEST  = [0xFF, 0x01, devicePk(32), joinCode(16), name(rest)]
///   JOIN_RESPONSE = [0xFF, 0x02, devicePk(32), tgPk(32),     encrypted(rest)]
const JOIN_MAGIC:    u8 = 0xFF;
const JOIN_REQUEST:  u8 = 0x01;
pub const JOIN_RESPONSE: u8 = 0x02;

fn is_join_request(frame: &[u8]) -> bool {
    frame.len() >= 2 + 32 + 16
        && frame[0] == JOIN_MAGIC
        && frame[1] == JOIN_REQUEST
}

/// Build a JOIN_RESPONSE frame. `encrypted` is the
/// XChaCha20-Poly1305-sealed cert+key bundle produced by
/// `TrustGroup::process_join_request`.
pub fn build_join_response(
    device_pk: &[u8; 32],
    tg_pk: &[u8; 32],
    encrypted: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + 32 + 32 + encrypted.len());
    v.push(JOIN_MAGIC);
    v.push(JOIN_RESPONSE);
    v.extend_from_slice(device_pk);
    v.extend_from_slice(tg_pk);
    v.extend_from_slice(encrypted);
    v
}

/// Handle an inbound JOIN_REQUEST binary frame from a viewer that
/// arrived via the relay. Same effect as if the viewer had POSTed to
/// `/api/access/request` over HTTP: we record a pending request the
/// operator can approve in the Link tab.
///
/// The joinCode (16 bytes / 32 hex chars) is included for protocol
/// parity with notekeeper but isn't checked against the invite token
/// table here — r2-rocker uses the request-and-approve gate as the
/// authority, so the operator's explicit click is what admits the
/// device, not the join-code match.
async fn handle_join_request(
    state: &std::sync::Arc<crate::AppState>,
    frame: &[u8],
) {
    if !is_join_request(frame) {
        return;
    }
    let device_pk = &frame[2..34];
    let _join_code = &frame[34..50]; // unused; see doc-comment above
    let name = std::str::from_utf8(&frame[50..])
        .unwrap_or("(relay)")
        .trim()
        .to_string();
    let name = if name.is_empty() { "(relay)".to_string() } else { name };

    let device_pk_hex = hex::encode(device_pk);
    eprintln!("[relay] JOIN_REQUEST device_pk={} name=\"{}\"", &device_pk_hex[..16], name);

    let Some(handle) = state.access.as_ref() else { return; };
    let outcome = {
        let mut a = handle.lock().await;
        a.submit_request(&device_pk_hex, &name, "relay")
    };
    use crate::access::RequestOutcome;
    if let RequestOutcome::Submitted(pk) = outcome {
        // Mirror what the cmd dispatcher does — emit r2.dash.access.event
        // on /r2 so the controller's Link tab shows the new pending row
        // (SPEC-R2-ROCKER-WIRE row 26, post-v0.2 — /ws/status is gone).
        crate::emit_access_event(
            state,
            "request_pending",
            &hex::encode(&pk[..]),
            Some(&name),
            Some("via relay"),
        );
    } else {
        eprintln!("[relay] JOIN_REQUEST submit_request outcome was not Submitted: {:?}", outcome);
    }
}
