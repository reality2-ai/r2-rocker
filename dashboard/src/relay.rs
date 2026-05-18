//! Phase 5 / SPEC-R2-ROCKER-ACCESS §5.2 — off-network viewer path.
//!
//! When the dashboard is started with `--relay-url`, this module
//! opens a persistent WebSocket to the R2 relay and:
//!
//!   * authenticates as the KeyHolder (HELLO signed with `tg_priv`),
//!   * publishes every R2-WIRE frame the sensors send to /ws/raw
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
    text_tx: broadcast::Sender<String>,
    state: std::sync::Arc<crate::AppState>,
) {
    tokio::spawn(async move {
        let mut backoff_ms = RECONNECT_MIN_MS;
        loop {
            match run_one_session(&relay_url, &signing_key, &tg_pub, &raw_frame_tx, &text_tx, &state).await {
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
    text_tx: &broadcast::Sender<String>,
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

    // Outbound: subscribe to the dashboard's raw-frame broadcast.
    // Every R2-WIRE frame the dashboard forwards to /ws/raw also
    // gets pushed up to the relay. Viewers in the same TG bucket
    // see it in real time.
    let mut raw_rx  = raw_frame_tx.subscribe();
    let mut text_rx = text_tx.subscribe();

    // Inbound: viewer-to-controller frames could arrive here in a
    // later slice. For v0.1 we drain and discard — the bridge
    // policy (SPEC-R2-ROCKER-BRIDGE §3-§4) for inbound viewer ops
    // hasn't been wired yet.
    let mut ping_tick = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_tick.tick().await; // skip the immediate fire

    loop {
        tokio::select! {
            // Outbound: frame arrived from a sensor → forward to relay.
            res = raw_rx.recv() => {
                match res {
                    Ok(rf) => {
                        // Wrap the same envelope shape /ws/raw uses
                        // so viewers can decode it with the existing
                        // path. See encode_raw_frame_envelope in
                        // main.rs.
                        let envelope = crate::encode_raw_frame_envelope(&rf);
                        if let Err(e) = sink.send(WsMessage::Binary(envelope.into())).await {
                            return Err(format!("write outbound frame: {e}"));
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
                        let _ = sink.send(WsMessage::Pong(p)).await;
                    }
                    Some(Ok(WsMessage::Pong(_))) => { /* ours, expected */ }
                    Some(Ok(WsMessage::Text(t))) => {
                        // JSON access-protocol frames from off-network
                        // viewers (pairing flow per SPEC-R2-ROCKER-ACCESS).
                        // Other text (pong / catchup_incomplete) is
                        // dropped silently.
                        if t.contains("\"access.request\"") {
                            handle_relay_access_request(state, text_tx, &t).await;
                        } else if !t.contains("\"pong\"") {
                            eprintln!("[relay] text in: {}", t.trim());
                        }
                    }
                    Some(Ok(WsMessage::Binary(b))) => {
                        // Viewer→controller frame. v0.1: log size + drop.
                        // Wired into the existing inbound dispatch in a
                        // follow-up slice.
                        let _ = b;
                    }
                    Some(Ok(WsMessage::Close(frame))) => {
                        return Err(format!("relay closed: {frame:?}"));
                    }
                    Some(Ok(WsMessage::Frame(_))) => {}
                    Some(Err(e)) => return Err(format!("ws read: {e}")),
                    None => return Err(format!("ws stream ended")),
                }
            }

            // Outbound text: anything pushed to AppState.relay_text_tx
            // (e.g. access::approve_request pushing an access.response
            // JSON) goes out as a WS text frame for viewers in this
            // trust-group bucket.
            res = text_rx.recv() => {
                match res {
                    Ok(s) => {
                        if let Err(e) = sink.send(WsMessage::Text(s.into())).await {
                            return Err(format!("write text: {e}"));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[relay] text_rx lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("text_rx closed".to_string());
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

/// Handle an inbound `access.request` text frame from a viewer that
/// arrived via the relay. Same effect as if the viewer had POSTed to
/// `/api/access/request` over HTTP: we record a pending request the
/// operator can approve in the Link tab.
async fn handle_relay_access_request(
    state: &std::sync::Arc<crate::AppState>,
    text_tx: &broadcast::Sender<String>,
    text: &str,
) {
    #[derive(serde::Deserialize)]
    struct ReqBody {
        device_pk: String,
        name: String,
        // tg_hash + entropy_hex are present but unused here — relay
        // forwards by tg_hash so it's already verified, and the
        // request-and-approve flow doesn't consume an invite token.
    }
    let Ok(body) = serde_json::from_str::<ReqBody>(text) else {
        eprintln!("[relay] malformed access.request: {}", text.chars().take(120).collect::<String>());
        return;
    };
    let Some(handle) = state.access.as_ref() else { return; };
    let outcome = {
        let mut a = handle.lock().await;
        a.submit_request(&body.device_pk, &body.name, "relay")
    };
    use crate::access::RequestOutcome;
    if let RequestOutcome::Submitted(pk) = outcome {
        // Mirror what the HTTP handler does — broadcast on /ws/status
        // so the controller's Link tab shows the new pending row.
        let _ = state.ws_broadcast_tx.send(serde_json::json!({
            "type": "access",
            "event": "request_pending",
            "device_pk": hex::encode(&pk[..]),
            "name": body.name,
            "hint": "via relay",
        }).to_string());
    }
    // Suppress unused-warning when text_tx is only used in select arm.
    let _ = text_tx;
}
