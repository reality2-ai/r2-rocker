//! # r2-wasm
//!
//! R2 protocol stack compiled to WebAssembly. The browser becomes an R2 hive.
//!
//! This crate wraps the core R2 protocol crates (`r2-fnv`, `r2-cbor`, `r2-wire`,
//! `r2-trust`, `r2-transport`, `r2-route`) via `wasm-bindgen`, exposing them to
//! JavaScript in the browser.
//!
//! The WASM module handles: frame encode/decode, HMAC sign/verify, trust group
//! key derivation, device certificate operations, and event name hashing.
//! The JavaScript layer connects these to browser transport APIs (WebSocket,
//! WebBluetooth, WebUSB).
//!
//! See R2-INTERNET §8.3 for the full browser hive architecture.

extern crate alloc;

use wasm_bindgen::prelude::*;

pub mod notekeeper;
pub mod sync_plugin;
pub mod hive;
pub mod wordlist;

// ---------------------------------------------------------------------------
// r2-fnv: Event name hashing
// ---------------------------------------------------------------------------

/// Hash an event name to a 32-bit FNV-1a identifier.
///
/// Canonicalises the input (lowercase, whitespace-stripped) before hashing.
/// Returns the hash, or throws on empty/reserved names.
#[wasm_bindgen]
pub fn r2_hash(event_name: &str) -> Result<u32, JsError> {
    r2_fnv::r2_hash(event_name).map_err(|e| JsError::new(&format!("{:?}", e)))
}

/// Raw FNV-1a 32-bit hash of pre-canonicalised bytes.
#[wasm_bindgen]
pub fn fnv1a_32(data: &[u8]) -> u32 {
    r2_fnv::fnv1a_32(data)
}

// ---------------------------------------------------------------------------
// r2-cbor: CBOR encoding
// ---------------------------------------------------------------------------

/// Encode a simple CBOR map: { key0: val0, key1: val1, ... }
///
/// Takes parallel arrays of integer keys and integer values.
/// Returns CBOR-encoded bytes (compact mode).
#[wasm_bindgen]
pub fn cbor_encode_int_map(keys: &[u8], values: &[u32]) -> Result<Vec<u8>, JsError> {
    use r2_cbor::{Encoder, Value};

    if keys.len() != values.len() {
        return Err(JsError::new("keys and values must have same length"));
    }

    let mut buf = [0u8; 180];
    let mut enc = Encoder::new(&mut buf);
    enc.map(keys.len())
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    for (k, v) in keys.iter().zip(values.iter()) {
        enc.kv(*k as u64, &Value::UInt(*v as u64))
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    }

    Ok(enc.as_bytes().to_vec())
}

/// Encode a note event CBOR payload: {0: opCode, 1: noteId, 2: timestamp, 3?: encryptedContent}.
///
/// Key 3 (encrypted content) is only included if `encrypted_content` is non-empty.
/// This packs both metadata and content into a single R2-WIRE frame payload.
#[wasm_bindgen]
pub fn cbor_encode_note_event(
    op_code: u32,
    note_id: u32,
    timestamp: u32,
    encrypted_content: &[u8],
) -> Result<Vec<u8>, JsError> {
    use r2_cbor::{Encoder, Value};

    let has_content = !encrypted_content.is_empty();
    let map_len = if has_content { 4 } else { 3 };

    let mut buf = alloc::vec![0u8; 20 + encrypted_content.len()];
    let mut enc = Encoder::new(&mut buf);
    enc.map(map_len)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(0, &Value::UInt(op_code as u64))
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(1, &Value::UInt(note_id as u64))
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(2, &Value::UInt(timestamp as u64))
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    if has_content {
        enc.kv(3, &Value::Bytes(encrypted_content))
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    }

    Ok(enc.as_bytes().to_vec())
}

/// Decode a note event CBOR payload: {0: opCode, 1: noteId, 2: timestamp, 3?: encryptedContent}.
///
/// Returns a JS object with `op_code`, `note_id`, `timestamp`, and optionally `encrypted_content` (Uint8Array).
#[wasm_bindgen]
pub fn cbor_decode_note_event(payload: &[u8]) -> Result<JsValue, JsError> {
    use r2_cbor::{Decoder, Item, Mode};

    let mut dec = Decoder::new_with_mode(payload, Mode::Compact);
    let map_len = match dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? {
        Item::Map(n) => n,
        _ => return Err(JsError::new("expected CBOR map")),
    };

    let mut op_code: u32 = 0;
    let mut note_id: u32 = 0;
    let mut timestamp: u32 = 0;
    let mut encrypted_content: Option<Vec<u8>> = None;

    for _ in 0..map_len {
        let key = match dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? {
            Item::UInt(k) => k,
            _ => return Err(JsError::new("expected integer key")),
        };
        match key {
            0 => if let Item::UInt(v) = dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? { op_code = v as u32; },
            1 => if let Item::UInt(v) = dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? { note_id = v as u32; },
            2 => if let Item::UInt(v) = dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? { timestamp = v as u32; },
            3 => if let Item::Bytes(b) = dec.next().map_err(|e| JsError::new(&format!("{:?}", e)))? { encrypted_content = Some(b.to_vec()); },
            _ => { let _ = dec.next(); } // skip unknown keys
        }
    }

    let result = NoteEventInfo {
        op_code,
        note_id,
        timestamp,
        encrypted_content,
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct NoteEventInfo {
    op_code: u32,
    note_id: u32,
    timestamp: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_content: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// r2-wire: Compact frame encode/decode
// ---------------------------------------------------------------------------

/// Encode a compact R2-WIRE frame.
///
/// Parameters:
/// - `msg_type`: 0=Event, 2=Reply, 3=Ack, 4=Nack, 5=Heartbeat
/// - `ttl`: time-to-live (0-15)
/// - `k`: relay budget (0-15)
/// - `msg_id`: 16-bit message ID
/// - `event_hash`: 32-bit FNV-1a hash of event name
/// - `target`: 32-bit target hive address (0 = broadcast)
/// - `payload`: CBOR-encoded payload bytes
///
/// Returns the encoded frame bytes.
#[wasm_bindgen]
pub fn encode_compact_frame(
    msg_type: u8,
    ttl: u8,
    k: u8,
    msg_id: u16,
    event_hash: u32,
    target: u32,
    payload: &[u8],
) -> Result<Vec<u8>, JsError> {
    use r2_wire::types::{CompactHeader, CompactMessage, Flags};

    let mt = msg_type_from_u8(msg_type)?;

    let msg = CompactMessage {
        header: CompactHeader {
            version: 0,
            msg_type: mt,
            flags: Flags::default(),
            ttl,
            k,
            msg_id,
            event_hash,
            target,
        },
        route: None,
        payload,
        hmac_tag: None,
    };

    let mut buf = [0u8; 256];
    let len = r2_wire::encode_compact(&msg, &mut buf)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    Ok(buf[..len].to_vec())
}

/// Decode a compact R2-WIRE frame.
///
/// Returns a JS object with header fields and payload.
#[wasm_bindgen]
pub fn decode_compact_frame(data: &[u8]) -> Result<JsValue, JsError> {
    let msg = r2_wire::decode_compact(data)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let result = CompactFrameInfo {
        msg_type: msg.header.msg_type as u8,
        ttl: msg.header.ttl,
        k: msg.header.k,
        msg_id: msg.header.msg_id,
        event_hash: msg.header.event_hash,
        target: msg.header.target,
        payload: msg.payload.to_vec(),
        has_hmac: msg.hmac_tag.is_some(),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct CompactFrameInfo {
    msg_type: u8,
    ttl: u8,
    k: u8,
    msg_id: u16,
    event_hash: u32,
    target: u32,
    payload: Vec<u8>,
    has_hmac: bool,
}

// ---------------------------------------------------------------------------
// r2-wire: Extended frame encode/decode
// ---------------------------------------------------------------------------

/// Encode an extended R2-WIRE frame.
#[wasm_bindgen]
pub fn encode_extended_frame(
    msg_type: u8,
    ttl: u8,
    k: u8,
    msg_id: u32,
    event_hash: u32,
    target_group: u32,
    target_hive: u32,
    payload: &[u8],
) -> Result<Vec<u8>, JsError> {
    use r2_wire::types::{ExtendedHeader, ExtendedMessage, Flags};

    let mt = msg_type_from_u8(msg_type)?;

    let msg = ExtendedMessage {
        header: ExtendedHeader {
            version: 0,
            msg_type: mt,
            flags: Flags::default(),
            ttl,
            k,
            msg_id,
            event_hash,
            payload_len: payload.len() as u32,
            target_group,
            target_hive,
        },
        route: None,
        payload,
        hmac_tag: None,
    };

    let mut buf = vec![0u8; 22 + payload.len() + 32]; // header + payload + hmac
    let len = r2_wire::encode_extended(&msg, &mut buf)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    buf.truncate(len);
    Ok(buf)
}

/// Decode an extended R2-WIRE frame.
#[wasm_bindgen]
pub fn decode_extended_frame(data: &[u8]) -> Result<JsValue, JsError> {
    let msg = r2_wire::decode_extended(data)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let result = ExtendedFrameInfo {
        msg_type: msg.header.msg_type as u8,
        ttl: msg.header.ttl,
        k: msg.header.k,
        msg_id: msg.header.msg_id,
        event_hash: msg.header.event_hash,
        target_group: msg.header.target_group,
        target_hive: msg.header.target_hive,
        payload: msg.payload.to_vec(),
        has_hmac: msg.hmac_tag.is_some(),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct ExtendedFrameInfo {
    msg_type: u8,
    ttl: u8,
    k: u8,
    msg_id: u32,
    event_hash: u32,
    target_group: u32,
    target_hive: u32,
    payload: Vec<u8>,
    has_hmac: bool,
}

// ---------------------------------------------------------------------------
// r2-wire: Transcoding
// ---------------------------------------------------------------------------

/// Transcode a compact frame to extended format.
#[wasm_bindgen]
pub fn transcode_to_extended(compact_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    let mut buf = vec![0u8; compact_bytes.len() + 64];
    let len = r2_wire::transcode_compact_to_extended(compact_bytes, &mut buf)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    buf.truncate(len);
    Ok(buf)
}

/// Transcode an extended frame to compact format.
#[wasm_bindgen]
pub fn transcode_to_compact(extended_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    let mut buf = [0u8; 256];
    let len = r2_wire::transcode_extended_to_compact(extended_bytes, &mut buf)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;
    Ok(buf[..len].to_vec())
}

// ---------------------------------------------------------------------------
// r2-trust: Key derivation
// ---------------------------------------------------------------------------

/// Derive trust group keys (DEK + HK) from raw secret and public key bytes.
///
/// Both `tg_secret` and `tg_public` must be 32 bytes (Ed25519 key material).
/// Returns a JS object with `dek` and `hk` as byte arrays.
#[wasm_bindgen]
pub fn derive_group_keys(tg_secret: &[u8], tg_public: &[u8]) -> Result<JsValue, JsError> {
    if tg_secret.len() != 32 || tg_public.len() != 32 {
        return Err(JsError::new("Both keys must be 32 bytes"));
    }

    let mut sk = [0u8; 32];
    let mut pk = [0u8; 32];
    sk.copy_from_slice(tg_secret);
    pk.copy_from_slice(tg_public);

    let keys = r2_trust::hkdf::derive_group_keys_raw(&sk, &pk)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let result = GroupKeysInfo {
        dek: keys.dek.to_vec(),
        hk: keys.hk.to_vec(),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct GroupKeysInfo {
    dek: Vec<u8>,
    hk: Vec<u8>,
}

#[derive(serde::Serialize)]
struct MemberListEntry {
    name: String,
    public_key_hex: String,
}

fn hex_to_bytes_32(hex: &str) -> Result<[u8; 32], JsError> {
    if hex.len() != 64 {
        return Err(JsError::new("public key hex must be 64 chars"));
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| JsError::new("invalid hex"))?;
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// r2-trust: HMAC frame signing/verification
// ---------------------------------------------------------------------------

/// Compute an HMAC tag for a compact frame.
///
/// `hk` must be 32 bytes. Returns the 8-byte truncated HMAC tag.
/// The caller is responsible for appending the tag to the frame and setting
/// the has_hmac flag.
#[wasm_bindgen]
pub fn hmac_compact_tag(frame_bytes: &[u8], hk: &[u8]) -> Result<Vec<u8>, JsError> {
    if hk.len() != 32 {
        return Err(JsError::new("HK must be 32 bytes"));
    }

    // Decode the frame first, then sign it
    let msg = r2_wire::decode_compact(frame_bytes)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let mut hk_arr = [0u8; 32];
    hk_arr.copy_from_slice(hk);
    let hmac = r2_trust::GroupHmac::new(hk_arr);

    let (_flags, tag) = r2_wire::sign_compact(&msg, &hmac);
    Ok(tag.to_vec())
}

/// Verify a compact frame's HMAC tag.
///
/// The frame must include the HMAC tag (has_hmac flag set).
/// `hk` must be 32 bytes. Returns true if valid.
#[wasm_bindgen]
pub fn verify_compact_hmac(signed_frame: &[u8], hk: &[u8]) -> Result<bool, JsError> {
    if hk.len() != 32 {
        return Err(JsError::new("HK must be 32 bytes"));
    }

    let msg = r2_wire::decode_compact(signed_frame)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let mut hk_arr = [0u8; 32];
    hk_arr.copy_from_slice(hk);
    let hmac = r2_trust::GroupHmac::new(hk_arr);

    Ok(r2_wire::verify_compact(&msg, &hmac))
}

/// Compute the HMAC tag for an extended R2-WIRE frame.
///
/// `frame_bytes` must be a valid extended R2-WIRE frame (without HMAC).
/// `hk` must be 32 bytes (the trust group's HMAC key).
/// Returns the 32-byte HMAC-SHA256 tag.
#[wasm_bindgen]
pub fn hmac_extended_tag(frame_bytes: &[u8], hk: &[u8]) -> Result<Vec<u8>, JsError> {
    if hk.len() != 32 {
        return Err(JsError::new("HK must be 32 bytes"));
    }

    let msg = r2_wire::decode_extended(frame_bytes)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let mut hk_arr = [0u8; 32];
    hk_arr.copy_from_slice(hk);
    let hmac = r2_trust::GroupHmac::new(hk_arr);

    let (_flags, tag) = r2_wire::sign_extended(&msg, &hmac);
    Ok(tag.to_vec())
}

/// Verify an extended frame's HMAC tag.
///
/// The frame must include the HMAC tag (has_hmac flag set).
/// `hk` must be 32 bytes. Returns true if valid.
#[wasm_bindgen]
pub fn verify_extended_hmac(signed_frame: &[u8], hk: &[u8]) -> Result<bool, JsError> {
    if hk.len() != 32 {
        return Err(JsError::new("HK must be 32 bytes"));
    }

    let msg = r2_wire::decode_extended(signed_frame)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    let mut hk_arr = [0u8; 32];
    hk_arr.copy_from_slice(hk);
    let hmac = r2_trust::GroupHmac::new(hk_arr);

    Ok(r2_wire::verify_extended(&msg, &hmac))
}

// ---------------------------------------------------------------------------
// r2-transport: Framing helpers
// ---------------------------------------------------------------------------

/// Wrap a frame with a 2-byte little-endian length prefix (BLE/USB framing).
#[wasm_bindgen]
pub fn frame_with_le_prefix(frame: &[u8]) -> Vec<u8> {
    let len = frame.len() as u16;
    let mut out = Vec::with_capacity(2 + frame.len());
    out.push((len & 0xFF) as u8);
    out.push((len >> 8) as u8);
    out.extend_from_slice(frame);
    out
}

/// Wrap a frame with a 4-byte big-endian length prefix (TCP framing).
#[wasm_bindgen]
pub fn frame_with_be_prefix(frame: &[u8]) -> Vec<u8> {
    let len = frame.len() as u32;
    let mut out = Vec::with_capacity(4 + frame.len());
    out.push((len >> 24) as u8);
    out.push((len >> 16) as u8);
    out.push((len >> 8) as u8);
    out.push((len & 0xFF) as u8);
    out.extend_from_slice(frame);
    out
}

// ---------------------------------------------------------------------------
// r2-trust: Trust group lifecycle
// ---------------------------------------------------------------------------

/// Opaque handle to a TrustGroup (key holder side).
/// Stored in WASM memory; JS holds the index.
#[wasm_bindgen]
pub struct R2TrustGroup {
    inner: r2_trust::TrustGroup,
}

#[wasm_bindgen]
impl R2TrustGroup {
    /// Create a new trust group. Returns the key holder's trust group handle.
    ///
    /// `now` is the current Unix timestamp in seconds.
    #[wasm_bindgen(constructor)]
    pub fn new(now: u64) -> Result<R2TrustGroup, JsError> {
        let mut rng = getrandom_rng();
        let tg = r2_trust::TrustGroup::create(&mut rng, now)
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
        Ok(R2TrustGroup { inner: tg })
    }

    /// Trust group public key (32 bytes). This is the trust group ID.
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.trust_group_id().to_vec()
    }

    /// DEK (data encryption key), 32 bytes.
    #[wasm_bindgen(getter)]
    pub fn dek(&self) -> Vec<u8> {
        self.inner.derived_keys().dek.to_vec()
    }

    /// HK (HMAC key), 32 bytes.
    #[wasm_bindgen(getter)]
    pub fn hk(&self) -> Vec<u8> {
        self.inner.derived_keys().hk.to_vec()
    }

    /// Number of members (excluding key holder).
    #[wasm_bindgen(getter)]
    pub fn member_count(&self) -> usize {
        self.inner.members().len()
    }

    /// Generate a join code. Returns the 16-byte code as hex string.
    ///
    /// `now` is current Unix timestamp, `ttl_secs` is validity duration.
    pub fn generate_join_code(&mut self, now: u64, ttl_secs: u64) -> String {
        let mut rng = getrandom_rng();
        let code = self.inner.generate_join_code(&mut rng, now, ttl_secs);
        hex_encode(code.value())
    }

    /// Process a join request from a device.
    ///
    /// - `join_code_hex`: the 16-byte join code as hex string
    /// - `device_public_key`: the joiner's Ed25519 public key (32 bytes)
    /// - `device_name`: human-readable name for the device
    /// - `now`: current Unix timestamp
    ///
    /// Returns the encrypted join response as bytes (to send to the joiner).
    pub fn process_join(
        &mut self,
        join_code_hex: &str,
        device_public_key: &[u8],
        device_name: &str,
        now: u64,
    ) -> Result<Vec<u8>, JsError> {
        use ed25519_dalek::VerifyingKey;

        let code_bytes = hex_decode(join_code_hex)?;
        if code_bytes.len() != 16 {
            return Err(JsError::new("Join code must be 16 bytes (32 hex chars)"));
        }
        let mut code = [0u8; 16];
        code.copy_from_slice(&code_bytes);

        if device_public_key.len() != 32 {
            return Err(JsError::new("Device public key must be 32 bytes"));
        }
        let vk = VerifyingKey::from_bytes(device_public_key.try_into().unwrap())
            .map_err(|e| JsError::new(&format!("Invalid public key: {}", e)))?;

        let mut rng = getrandom_rng();
        let encrypted = self.inner.process_join_request(
            &mut rng,
            now,
            &code,
            &vk,
            device_name.into(),
            r2_trust::lifecycle::DEFAULT_CERT_TTL_SECS,
        ).map_err(|e| JsError::new(&format!("{:?}", e)))?;

        // Serialize the encrypted response for transport
        Ok(serialize_encrypted_response(&encrypted))
    }

    /// List member names as a JSON array.
    pub fn member_names(&self) -> JsValue {
        let names: Vec<&str> = self.inner.members().iter().map(|m| m.name.as_str()).collect();
        serde_wasm_bindgen::to_value(&names).unwrap_or(JsValue::NULL)
    }

    /// List members as JSON array of {name, public_key_hex} objects.
    pub fn member_list(&self) -> JsValue {
        let members: Vec<MemberListEntry> = self.inner.members().iter().map(|m| {
            MemberListEntry {
                name: m.name.clone(),
                public_key_hex: m.certificate.device_public_key.iter()
                    .map(|b| format!("{:02x}", b)).collect(),
            }
        }).collect();
        serde_wasm_bindgen::to_value(&members).unwrap_or(JsValue::NULL)
    }

    /// Revoke a member by their public key hex string. Key holder only.
    pub fn revoke_member(&mut self, public_key_hex: &str, now: u64) -> Result<(), JsError> {
        let bytes = hex_to_bytes_32(public_key_hex)?;
        self.inner.revoke_device(now, &bytes, r2_trust::revocation::RevocationReason::ForcedRemoval)
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
        Ok(())
    }

    /// Serialize key holder state to bytes for persistent storage.
    ///
    /// Returns 38 bytes (signing key + sequence + crypto level).
    /// **Contains TG_SK — the root secret. Encrypt before storing.**
    pub fn to_bytes(&self) -> Vec<u8> {
        r2_trust::persist::serialize_trust_group_minimal(&self.inner).to_vec()
    }

    /// Restore key holder state from previously serialized bytes.
    ///
    /// Restores the signing key and derived keys. Member list starts empty —
    /// members rejoin via the normal join protocol or are restored separately.
    #[wasm_bindgen(static_method_of = R2TrustGroup)]
    pub fn from_bytes(bytes: &[u8], now: u64) -> Result<R2TrustGroup, JsError> {
        let inner = r2_trust::persist::deserialize_trust_group_minimal(bytes, now)
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
        Ok(R2TrustGroup { inner })
    }
}

/// Opaque handle to a MemberState (device/joiner side).
#[wasm_bindgen]
pub struct R2Member {
    inner: r2_trust::MemberState,
}

/// Generate a new Ed25519 device keypair.
///
/// Returns a JS object with `secret_key` (32 bytes) and `public_key` (32 bytes).
#[wasm_bindgen]
pub fn generate_device_keypair() -> Result<JsValue, JsError> {
    use ed25519_dalek::SigningKey;
    let mut rng = getrandom_rng();
    let sk = SigningKey::generate(&mut rng);

    let result = DeviceKeypair {
        secret_key: sk.to_bytes().to_vec(),
        public_key: sk.verifying_key().to_bytes().to_vec(),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct DeviceKeypair {
    secret_key: Vec<u8>,
    public_key: Vec<u8>,
}

/// Complete the join handshake (device side).
///
/// - `device_secret_key`: the device's Ed25519 secret key (32 bytes)
/// - `trust_group_public_key`: the trust group's public key (32 bytes)
/// - `encrypted_response`: the encrypted join response bytes from the key holder
/// - `now`: current Unix timestamp
///
/// Returns an R2Member handle on success.
#[wasm_bindgen]
pub fn complete_join(
    device_secret_key: &[u8],
    trust_group_public_key: &[u8],
    encrypted_response: &[u8],
    now: u64,
) -> Result<R2Member, JsError> {
    use ed25519_dalek::{SigningKey, VerifyingKey};

    if device_secret_key.len() != 32 {
        return Err(JsError::new("Device secret key must be 32 bytes"));
    }
    if trust_group_public_key.len() != 32 {
        return Err(JsError::new("Trust group public key must be 32 bytes"));
    }

    let sk = SigningKey::from_bytes(device_secret_key.try_into().unwrap());
    let tg_pk = VerifyingKey::from_bytes(trust_group_public_key.try_into().unwrap())
        .map_err(|e| JsError::new(&format!("Invalid TG public key: {}", e)))?;

    let encrypted = deserialize_encrypted_response(encrypted_response)?;

    let member = r2_trust::MemberState::from_join_response(sk, &tg_pk, &encrypted, now)
        .map_err(|e| JsError::new(&format!("{:?}", e)))?;

    Ok(R2Member { inner: member })
}

#[wasm_bindgen]
impl R2Member {
    /// Device's public key (32 bytes).
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.device_key().verifying_key().to_bytes().to_vec()
    }

    /// Trust group public key (32 bytes).
    #[wasm_bindgen(getter)]
    pub fn trust_group_id(&self) -> Vec<u8> {
        self.inner.trust_group_public().to_bytes().to_vec()
    }

    /// HK for HMAC operations (32 bytes).
    #[wasm_bindgen(getter)]
    pub fn hk(&self) -> Vec<u8> {
        self.inner.hk().to_vec()
    }

    /// DEK for encryption (32 bytes).
    #[wasm_bindgen(getter)]
    pub fn dek(&self) -> Vec<u8> {
        self.inner.dek().to_vec()
    }

    /// Check if the membership certificate is valid at the given time.
    pub fn is_valid(&self, now: u64) -> bool {
        self.inner.is_valid(now)
    }

    /// Trust group hash for relay HELLO (first 8 bytes of SHA-256 of TG_PK, as 16 hex chars).
    pub fn trust_group_hash(&self) -> String {
        use sha2::{Sha256, Digest};
        let hash = Sha256::digest(self.inner.trust_group_public().as_bytes());
        hex_encode(&hash[..8])
    }

    /// Sign a relay HELLO message and return the complete JSON string.
    ///
    /// Produces: `{"type":"hello","version":1,"trust_group":"...","device_id":"...","timestamp":N,"signature":"..."}`
    ///
    /// The signature is Ed25519 over `"{trust_group}:{device_id}:{timestamp}"`.
    pub fn sign_relay_hello(&self, timestamp: u64) -> Result<String, JsError> {
        use ed25519_dalek::Signer;

        let tg_hash = self.trust_group_hash();
        let device_id = hex_encode(&self.inner.device_key().verifying_key().to_bytes());
        let msg = format!("{}:{}:{}", tg_hash, device_id, timestamp);
        let sig = self.inner.device_key().sign(msg.as_bytes());
        let sig_hex = hex_encode(&sig.to_bytes());

        let hello = format!(
            r#"{{"type":"hello","version":1,"trust_group":"{}","device_id":"{}","timestamp":{},"signature":"{}"}}"#,
            tg_hash, device_id, timestamp, sig_hex
        );
        Ok(hello)
    }

    /// Serialize member state to bytes for persistent storage.
    ///
    /// Returns 277 bytes containing device key, certificate, DEK, HK.
    /// **These bytes contain secret key material — encrypt before storing.**
    pub fn to_bytes(&self) -> Vec<u8> {
        r2_trust::persist::serialize_member_state(&self.inner).to_vec()
    }

    /// Restore member state from previously serialized bytes.
    ///
    /// Use this on page load to restore trust group membership from localStorage.
    #[wasm_bindgen(static_method_of = R2Member)]
    pub fn from_bytes(bytes: &[u8]) -> Result<R2Member, JsError> {
        let inner = r2_trust::persist::deserialize_member_state(bytes)
            .map_err(|e| JsError::new(&format!("{:?}", e)))?;
        Ok(R2Member { inner })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn msg_type_from_u8(v: u8) -> Result<r2_wire::types::MsgType, JsError> {
    use r2_wire::types::MsgType;
    MsgType::from_u8(v).map_err(|e| JsError::new(&format!("{:?}", e)))
}

/// Browser-safe RNG using getrandom (backed by Web Crypto API).
fn getrandom_rng() -> GetrandomRng {
    GetrandomRng
}

struct GetrandomRng;

impl rand_core::RngCore for GetrandomRng {
    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        getrandom::getrandom(&mut buf).expect("getrandom failed");
        u32::from_le_bytes(buf)
    }
    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).expect("getrandom failed");
        u64::from_le_bytes(buf)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        getrandom::getrandom(dest).expect("getrandom failed");
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        getrandom::getrandom(dest).map_err(|e| {
            rand_core::Error::from(core::num::NonZeroU32::new(e.code().get()).unwrap())
        })
    }
}

impl rand_core::CryptoRng for GetrandomRng {}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, JsError> {
    if hex.len() % 2 != 0 {
        return Err(JsError::new("Hex string must have even length"));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| JsError::new("Invalid hex character"))
        })
        .collect()
}

fn base64url_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 { out.push(CHARS[((n >> 6) & 63) as usize] as char); }
        if chunk.len() > 2 { out.push(CHARS[(n & 63) as usize] as char); }
    }
    out
}

fn base64url_decode(s: &str) -> Result<Vec<u8>, JsError> {
    fn val(c: u8) -> Result<u32, JsError> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(JsError::new("Invalid base64url character")),
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i < bytes.len() {
        let a = val(bytes[i])?;
        let b = if i + 1 < bytes.len() { val(bytes[i + 1])? } else { 0 };
        let c = if i + 2 < bytes.len() { val(bytes[i + 2])? } else { 0 };
        let d = if i + 3 < bytes.len() { val(bytes[i + 3])? } else { 0 };
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        out.push((n >> 16) as u8);
        if i + 2 < bytes.len() { out.push((n >> 8) as u8); }
        if i + 3 < bytes.len() { out.push(n as u8); }
        i += 4;
    }
    Ok(out)
}

/// Serialize EncryptedJoinResponse to bytes for transport.
/// Format: [nonce: 24] [ciphertext_len: 4 BE] [ciphertext: N]
fn serialize_encrypted_response(resp: &r2_trust::EncryptedJoinResponse) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&resp.nonce);
    let ct_len = resp.ciphertext.len() as u32;
    out.extend_from_slice(&ct_len.to_be_bytes());
    out.extend_from_slice(&resp.ciphertext);
    out
}

fn deserialize_encrypted_response(data: &[u8]) -> Result<r2_trust::EncryptedJoinResponse, JsError> {
    if data.len() < 28 {
        return Err(JsError::new("Encrypted response too short"));
    }
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&data[..24]);
    let ct_len = u32::from_be_bytes([data[24], data[25], data[26], data[27]]) as usize;
    if data.len() < 28 + ct_len {
        return Err(JsError::new("Encrypted response truncated"));
    }
    Ok(r2_trust::EncryptedJoinResponse {
        nonce,
        ciphertext: data[28..28 + ct_len].to_vec(),
    })
}

// ---------------------------------------------------------------------------
// r2-trust: Plugin data encryption (DEK)
// ---------------------------------------------------------------------------

/// Encrypt data with the trust group DEK (XChaCha20-Poly1305).
///
/// Returns: [nonce: 24 bytes] [ciphertext + auth tag]
/// Used for plugin-to-plugin data exchange (note content, files, etc.)
/// The relay sees only ciphertext.
#[wasm_bindgen]
pub fn encrypt_with_dek(dek: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, JsError> {
    use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};

    if dek.len() != 32 {
        return Err(JsError::new("DEK must be 32 bytes"));
    }

    let key = chacha20poly1305::Key::from_slice(dek);
    let cipher = XChaCha20Poly1305::new(key);

    let mut nonce_bytes = [0u8; 24];
    getrandom::getrandom(&mut nonce_bytes).map_err(|_| JsError::new("RNG failed"))?;
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| JsError::new("Encryption failed"))?;

    let mut out = Vec::with_capacity(24 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt data with the trust group DEK (XChaCha20-Poly1305).
///
/// Input: [nonce: 24 bytes] [ciphertext + auth tag] (as produced by encrypt_with_dek).
/// Returns the plaintext, or throws if decryption/authentication fails.
#[wasm_bindgen]
pub fn decrypt_with_dek(dek: &[u8], encrypted: &[u8]) -> Result<Vec<u8>, JsError> {
    use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};

    if dek.len() != 32 {
        return Err(JsError::new("DEK must be 32 bytes"));
    }
    if encrypted.len() < 24 + 16 {
        return Err(JsError::new("Encrypted data too short (need nonce + tag)"));
    }

    let key = chacha20poly1305::Key::from_slice(dek);
    let cipher = XChaCha20Poly1305::new(key);

    let nonce = XNonce::from_slice(&encrypted[..24]);
    let ciphertext = &encrypted[24..];

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| JsError::new("Decryption failed (wrong key or tampered data)"))
}

// ---------------------------------------------------------------------------
// Invite encode/decode
// ---------------------------------------------------------------------------

/// Encode an invite: TG_PK (32 bytes) + join_code (16 bytes) → base64url string.
///
/// The invite contains everything a joiner needs to connect and join:
/// the trust group public key (to compute hash and decrypt response)
/// and the join code secret.
#[wasm_bindgen]
pub fn encode_invite(tg_public_key: &[u8], join_code_hex: &str) -> Result<String, JsError> {
    if tg_public_key.len() != 32 {
        return Err(JsError::new("TG public key must be 32 bytes"));
    }
    let code_bytes = hex_decode(join_code_hex)?;
    if code_bytes.len() != 16 {
        return Err(JsError::new("Join code must be 16 bytes (32 hex chars)"));
    }

    let mut data = Vec::with_capacity(48);
    data.extend_from_slice(tg_public_key);
    data.extend_from_slice(&code_bytes);
    Ok(base64url_encode(&data))
}

/// Decode an invite string → { tg_public_key: [32], join_code_hex: string, trust_group_hash: string }
#[wasm_bindgen]
pub fn decode_invite(invite: &str) -> Result<JsValue, JsError> {
    let data = base64url_decode(invite)?;
    if data.len() != 48 {
        return Err(JsError::new("Invalid invite (expected 48 bytes)"));
    }

    let tg_pk = &data[..32];
    let join_code = &data[32..48];

    // Compute trust group hash (first 8 bytes of SHA-256)
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(tg_pk);
    let tg_hash = &hash[..8];

    let result = InviteInfo {
        tg_public_key: tg_pk.to_vec(),
        join_code_hex: hex_encode(join_code),
        trust_group_hash: hex_encode(tg_hash),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct InviteInfo {
    tg_public_key: Vec<u8>,
    join_code_hex: String,
    trust_group_hash: String,
}

/// Compute trust group hash from a public key (first 8 bytes of SHA-256, as 16 hex chars).
#[wasm_bindgen]
pub fn compute_trust_group_hash(tg_public_key: &[u8]) -> Result<String, JsError> {
    if tg_public_key.len() != 32 {
        return Err(JsError::new("TG public key must be 32 bytes"));
    }
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(tg_public_key);
    Ok(hex_encode(&hash[..8]))
}

// ---------------------------------------------------------------------------
// Version info
// ---------------------------------------------------------------------------

/// Sign a message with an Ed25519 secret key.
///
/// Used for relay HELLO signing when joining (before we have a full R2Member).
/// `secret_key` must be 32 bytes. Returns 64-byte Ed25519 signature.
#[wasm_bindgen]
pub fn sign_ed25519(secret_key: &[u8], message: &[u8]) -> Result<Vec<u8>, JsError> {
    use ed25519_dalek::{Signer, SigningKey};

    if secret_key.len() != 32 {
        return Err(JsError::new("Secret key must be 32 bytes"));
    }
    let sk = SigningKey::from_bytes(secret_key.try_into().unwrap());
    let sig = sk.sign(message);
    Ok(sig.to_bytes().to_vec())
}

/// Encode a note event payload as CBOR.
///
/// {0: id, 1: title, 2: content, 3: timestamp}
/// Used by JavaScript to build payloads for hive.send_event().
#[wasm_bindgen]
pub fn encode_note_payload(id: &str, title: &str, content: &str, timestamp: u64) -> Result<Vec<u8>, JsError> {
    use r2_cbor::{Encoder, Value};
    // CBOR overhead: map(4) + 4 key headers + 3 text length headers + uint header ≤ 32 bytes
    let capacity = id.len() + title.len() + content.len() + 32;
    let mut buf = vec![0u8; capacity];
    let mut enc = Encoder::new(&mut buf);
    enc.map(4).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(0, &Value::Text(id)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(1, &Value::Text(title)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(2, &Value::Text(content)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(3, &Value::UInt(timestamp)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    Ok(enc.as_bytes().to_vec())
}

/// Encode a note ID-only payload as CBOR.
///
/// {0: id, 3: timestamp}
/// Used for delete events.
#[wasm_bindgen]
pub fn encode_note_id_payload(id: &str, timestamp: u64) -> Result<Vec<u8>, JsError> {
    use r2_cbor::{Encoder, Value};
    let mut buf = [0u8; 64];
    let mut enc = Encoder::new(&mut buf);
    enc.map(2).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(0, &Value::Text(id)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    enc.kv(3, &Value::UInt(timestamp)).map_err(|e| JsError::new(&format!("{:?}", e)))?;
    Ok(enc.as_bytes().to_vec())
}

/// Encode a trust group hash + join code as 3 words.
///
/// `tg_hash_hex`: 16-char hex trust group hash
/// `join_code_hex`: 32-char hex join code
/// Returns: "word1-word2-word3"
#[wasm_bindgen]
pub fn encode_word_code(tg_hash_hex: &str, join_code_hex: &str) -> Result<String, JsError> {
    let prefix = wordlist::hash_to_prefix(tg_hash_hex)
        .ok_or_else(|| JsError::new("Invalid trust group hash"))?;

    // Take first 22 bits of join code (first 6 hex chars = 24 bits, mask to 22)
    if join_code_hex.len() < 6 {
        return Err(JsError::new("Join code too short"));
    }
    let join_val = u32::from_str_radix(&join_code_hex[..6], 16)
        .map_err(|_| JsError::new("Invalid join code hex"))?;
    let join_secret = join_val & 0x3FFFFF; // 22 bits

    let [w1, w2, w3] = wordlist::encode_words(prefix, join_secret);
    Ok(format!("{}-{}-{}", w1, w2, w3))
}

/// Decode 3 words back to trust group prefix (3 hex chars) + join code fragment.
///
/// Input: "word1-word2-word3" or "word1 word2 word3"
/// Returns JS object: { tg_prefix_hex: "abc", join_secret_hex: "123456" }
#[wasm_bindgen]
pub fn decode_word_code(words: &str) -> Result<JsValue, JsError> {
    let parts: Vec<&str> = words.split(|c| c == '-' || c == ' ')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if parts.len() != 3 {
        return Err(JsError::new("Expected 3 words separated by dashes or spaces"));
    }

    let (prefix, secret) = wordlist::decode_words(parts[0], parts[1], parts[2])
        .ok_or_else(|| JsError::new("Unrecognised word in code"))?;

    let result = WordCodeInfo {
        tg_prefix_hex: wordlist::prefix_to_hex(prefix),
        join_secret_hex: format!("{:06x}", secret),
    };

    serde_wasm_bindgen::to_value(&result)
        .map_err(|e| JsError::new(&format!("{}", e)))
}

#[derive(serde::Serialize)]
struct WordCodeInfo {
    tg_prefix_hex: String,
    join_secret_hex: String,
}

/// Returns the r2-wasm version string.
#[wasm_bindgen]
pub fn r2_version() -> String {
    format!("r2-wasm {}", env!("CARGO_PKG_VERSION"))
}
