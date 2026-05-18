//! Phase 5 — `/api/access/*` plumbing per `SPEC-R2-ROCKER-ACCESS.md`.
//!
//! Owns:
//!   * the in-memory `TrustGroup` (loaded from `tg_priv.bin` at startup),
//!   * the single-use enrolment-token table (5 min expiry, in-memory only
//!     per spec §3.3),
//!   * the helpers that mint a token + its three representations and that
//!     consume one with idempotency on the same `device_pk`.
//!
//! This module is the dashboard-server side of the spec. The webapp side
//! (Access tab, `?join=` handler, IndexedDB persistence) lands in a
//! follow-up slice and consumes the JSON shapes produced here.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{SigningKey, VerifyingKey};
use r2_trust::{
    DeviceRole, EncryptedJoinResponse, RevocationReason, TrustGroup,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Local Result alias — the dashboard doesn't depend on anyhow.
type Result<T> = std::result::Result<T, String>;

/// SPEC-R2-ROCKER-ACCESS §3.1 — single-use invite token expires 5 minutes
/// after issuance, server-side enforced.
const TOKEN_TTL_SECS: u64 = 300;

/// Cert validity for issued member certs — one year. Long enough that the
/// operator doesn't have to think about renewal during an experiment; short
/// enough that a stale cert won't outlive the project.
const CERT_TTL_SECS: u64 = 365 * 24 * 3600;

/// Where the dashboard reads the KeyHolder signing key from by default.
/// `SECRETS-POLICY.md` says this is operator-managed, off-tree, mode 0600.
/// Override via the `R2_TG_PRIV` environment variable for non-standard
/// deployments.
pub fn default_tg_priv_path() -> PathBuf {
    if let Ok(env) = std::env::var("R2_TG_PRIV") {
        return PathBuf::from(env);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/r2-rocker/tg_signer/tg_priv.bin")
}

/// In-memory record of one outstanding invite token (SPEC §3.3).
#[derive(Clone)]
struct TokenRecord {
    /// The 16-byte entropy that goes into the QR / URL / 3-word code.
    /// Doubles as the `r2-trust` JoinCode value.
    entropy: [u8; 16],
    /// Unix-ms wall-clock timestamps. `expires_at = issued_at + 300_000`.
    issued_at_ms: i64,
    expires_at_ms: i64,
    /// First-claim record. `None` until a viewer successfully POSTs
    /// `/api/access/claim`. Subsequent same-device claims return the
    /// cached response (idempotent); different-device claims 409.
    claim: Option<TokenClaim>,
}

#[derive(Clone)]
struct TokenClaim {
    device_pk: [u8; 32],
    claimed_at_ms: i64,
    /// Cached response body so re-claims return byte-identical JSON.
    cached_response: serde_json::Value,
}

/// What `/api/access/invite` returns to the operator's browser.
#[derive(Serialize)]
pub struct InviteEnvelope {
    /// 8-hex-char SHA-256 prefix of `TG_PK` — identifying, not authenticating.
    pub tg_hash: String,
    /// 32-hex-char raw entropy. The webapp embeds this in URLs alongside
    /// `tg_hash` separated by '.'; the wire form is `{tg_hash}.{entropy_hex}`.
    pub entropy_hex: String,
    /// `data:image/png;base64,...` source for the QR `<img>`.
    pub qr_png_data_url: String,
    /// `http://<controller_lan_ip>:8080/?join=<tg_hash>.<entropy_hex>` —
    /// always present.
    pub url_local: String,
    /// Static-host URL — only present if `--relay-url` was configured.
    pub url_relay: Option<String>,
    /// Unix-ms wall-clock when this token expires.
    pub expires_at_ms: i64,
}

/// Request body for `/api/access/claim`.
#[derive(Deserialize)]
pub struct ClaimRequest {
    pub tg_hash: String,
    pub entropy_hex: String,
    pub device_pk: String,
    pub device_name: String,
}

/// One row of `/api/access/members`.
#[derive(Serialize)]
pub struct MemberRow {
    pub device_pk: String, // hex
    pub name: String,
    pub role: String, // "controller" | "sensor" | "viewer"
    pub paired_at_ms: i64,
    pub revoked: bool,
}

/// All the dashboard-side Access state.
///
/// Held behind an `Arc<Mutex>` on `AppState`. The mutex is short-lived
/// per HTTP request — every `/api/access/*` handler acquires, mutates,
/// releases. There's no long-running borrow on this state.
pub struct Access {
    /// The KeyHolder's TG instance. Tracks members + revocations.
    tg: TrustGroup,
    /// Outstanding tokens keyed by their entropy.
    tokens: HashMap<[u8; 16], TokenRecord>,
    /// Cached `tg_hash` (first 8 hex chars of SHA-256(TG_PK)).
    tg_hash: String,
    /// Operator-supplied; embedded in QR + `url_relay`. `None` → no
    /// off-network path advertised in this deployment (spec §3.4).
    relay_url: Option<String>,
    /// `http://<host>:<port>` prefix used to build `url_local` —
    /// supplied at startup from the bind config.
    local_origin: String,
    /// Per-device human-readable name from claim time, keyed by
    /// `device_pk`. The TG itself stores the name on `MemberInfo`,
    /// but this side-cache lets us serve `/api/access/members`
    /// without re-parsing certs.
    names: HashMap<[u8; 32], String>,
    /// Map of `device_pk` → first-seen wall-clock ms, so the members
    /// list can show paired-at timestamps. (r2-trust's cert carries
    /// `issued_at` in seconds — close enough for v0.1 — but storing
    /// the ms here keeps the JSON shape uniform with the rest of
    /// the dashboard.)
    paired_at_ms: HashMap<[u8; 32], i64>,
}

impl Access {
    /// Load `tg_priv.bin` from disk and build the TrustGroup. The file
    /// is a raw 32-byte Ed25519 seed (compatible with
    /// `SigningKey::from_bytes`); `tools/r2-rocker-tg/keygen` writes it
    /// that way. Returns an error if the file is missing or the wrong
    /// length — the operator is expected to run keygen first.
    pub fn load(
        tg_priv_path: &Path,
        local_origin: String,
        relay_url: Option<String>,
    ) -> Result<Self> {
        let bytes = std::fs::read(tg_priv_path).map_err(|e| {
            format!(
                "Open KeyHolder signing key at {:?}: {}. \
                 Run `tools/r2-rocker-tg keygen` first, or set R2_TG_PRIV to point at it.",
                tg_priv_path, e
            )
        })?;
        if bytes.len() != 32 {
            return Err(format!(
                "{:?} is {} bytes; expected 32 (raw Ed25519 seed)",
                tg_priv_path,
                bytes.len()
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        let signing_key = SigningKey::from_bytes(&seed);
        let now = now_secs();
        let tg = TrustGroup::from_signing_key(signing_key, now)
            .map_err(|e| format!("TrustGroup::from_signing_key: {e}"))?;

        // tg_hash per SPEC §3.1: first 8 hex chars of SHA-256(TG_PK).
        let tg_pk = tg.verifying_key().to_bytes();
        let mut h = Sha256::new();
        h.update(tg_pk);
        let digest = h.finalize();
        let tg_hash = hex::encode(&digest[..4]); // 4 bytes = 8 hex chars

        Ok(Self {
            tg,
            tokens: HashMap::new(),
            tg_hash,
            relay_url,
            local_origin,
            names: HashMap::new(),
            paired_at_ms: HashMap::new(),
        })
    }

    /// `tg_hash` (8 hex chars). Returned in /api/keyholder/tg-info and
    /// embedded in tokens.
    pub fn tg_hash(&self) -> &str {
        &self.tg_hash
    }

    /// TG public key as hex (64 chars). Webapps verify cert chains
    /// against this.
    pub fn tg_pk_hex(&self) -> String {
        hex::encode(self.tg.verifying_key().to_bytes())
    }

    /// SPEC §4.1 — mint a single-use 5-min-expiring invite token and
    /// build the three representations.
    pub fn mint_invite(&mut self) -> std::result::Result<InviteEnvelope, String> {
        let now_secs = now_secs();
        let now_ms = (now_secs as i64) * 1000;
        let expires_at_ms = now_ms + (TOKEN_TTL_SECS as i64) * 1000;

        // Generate the JoinCode inside the TG (it'll be the candidate
        // process_join_request validates against) AND mirror its
        // entropy into our token table for idempotency tracking.
        let join_code =
            self.tg.generate_join_code(&mut OsRng, now_secs, TOKEN_TTL_SECS);
        let entropy: [u8; 16] = *join_code.value();
        self.tokens.insert(
            entropy,
            TokenRecord {
                entropy,
                issued_at_ms: now_ms,
                expires_at_ms,
                claim: None,
            },
        );

        // Build the URLs.
        let entropy_hex = hex::encode(entropy);
        let token_param = format!("{}.{}", self.tg_hash, entropy_hex);
        let url_local = format!("{}/?join={}", self.local_origin, token_param);
        let url_relay = self.relay_url.as_ref().map(|relay| {
            format!(
                "https://reality2-ai.github.io/r2-rocker/?join={}&relay={}",
                token_param,
                urlencode(relay),
            )
        });

        // The QR encodes the `r2:` deeplink form (matches notekeeper).
        let r2_url = match &self.relay_url {
            Some(r) => format!(
                "r2://join/{}/{}?relay={}",
                self.tg_hash, entropy_hex, urlencode(r)
            ),
            None => format!("r2://join/{}/{}", self.tg_hash, entropy_hex),
        };
        let qr_png_data_url = render_qr_png(&r2_url)?;

        Ok(InviteEnvelope {
            tg_hash: self.tg_hash.clone(),
            entropy_hex,
            qr_png_data_url,
            url_local,
            url_relay,
            expires_at_ms,
        })
    }

    /// SPEC §4.2 — consume a token, issue a cert, return the encrypted
    /// credential bundle. Idempotent on the same `device_pk` within the
    /// original window.
    ///
    /// Returns `Ok(json)` on success — caller serialises as the HTTP
    /// response body. Errors carry a sentinel that the caller maps to
    /// the appropriate status code.
    pub fn process_claim(&mut self, req: &ClaimRequest) -> ClaimOutcome {
        // 1. tg_hash must match ours.
        if req.tg_hash.to_ascii_lowercase() != self.tg_hash {
            return ClaimOutcome::NotFound;
        }

        // 2. Parse entropy + device_pk.
        let entropy = match hex_to_arr16(&req.entropy_hex) {
            Some(e) => e,
            None => return ClaimOutcome::BadRequest("entropy_hex must be 32 hex chars"),
        };
        let device_pk = match hex_to_arr32(&req.device_pk) {
            Some(d) => d,
            None => return ClaimOutcome::BadRequest("device_pk must be 64 hex chars"),
        };

        // 3. Name validation per spec §4.2 step 5.
        if !is_valid_device_name(&req.device_name) {
            return ClaimOutcome::BadRequest(
                "device_name must be 1..=64 chars in [A-Za-z0-9 ._-]",
            );
        }

        let now_ms = now_ms() as i64;
        let now_secs = now_secs();

        // 4. Look up the record. We hold the only authoritative copy.
        let Some(rec) = self.tokens.get(&entropy).cloned() else {
            return ClaimOutcome::NotFound;
        };

        // 5. Expiry.
        if now_ms >= rec.expires_at_ms {
            // Drop the dead record on the way out.
            self.tokens.remove(&entropy);
            return ClaimOutcome::Gone;
        }

        // 6. Idempotency / conflict.
        if let Some(prev) = &rec.claim {
            if prev.device_pk == device_pk {
                return ClaimOutcome::Success(prev.cached_response.clone());
            }
            return ClaimOutcome::Conflict;
        }

        // 7. First claim. Run the TG's join handshake.
        let device_vk = match VerifyingKey::from_bytes(&device_pk) {
            Ok(v) => v,
            Err(_) => return ClaimOutcome::BadRequest("device_pk is not a valid Ed25519 point"),
        };
        let encrypted = match self.tg.process_join_request(
            &mut OsRng,
            now_secs,
            &entropy,
            &device_vk,
            req.device_name.clone(),
            CERT_TTL_SECS,
        ) {
            Ok(e) => e,
            Err(e) => return ClaimOutcome::BadRequest(string_box(format!(
                "process_join_request: {e}"
            ))),
        };

        // 8. Side-caches for /api/access/members.
        self.names.insert(device_pk, req.device_name.clone());
        self.paired_at_ms.insert(device_pk, now_ms);

        // 9. Cache the response body for idempotent re-claims.
        let response_body = encode_claim_response(&self.tg, &encrypted, now_ms);
        self.tokens.entry(entropy).and_modify(|t| {
            t.claim = Some(TokenClaim {
                device_pk,
                claimed_at_ms: now_ms,
                cached_response: response_body.clone(),
            });
        });

        ClaimOutcome::Success(response_body)
    }

    /// SPEC §4.3 — list of paired devices. The TG owns the canonical
    /// member list; this method assembles the JSON-friendly view.
    pub fn members(&self) -> Vec<MemberRow> {
        let mut rows: Vec<MemberRow> = self
            .tg
            .members()
            .iter()
            .map(|m| {
                let pk = m.certificate.device_public_key;
                let name = self
                    .names
                    .get(&pk)
                    .cloned()
                    .unwrap_or_else(|| m.name.clone());
                let paired_at_ms = self
                    .paired_at_ms
                    .get(&pk)
                    .copied()
                    .unwrap_or_else(|| (m.certificate.issued_at as i64) * 1000);
                MemberRow {
                    device_pk: hex::encode(pk),
                    name,
                    role: cert_role_name(m.certificate.role),
                    paired_at_ms,
                    revoked: false,
                }
            })
            .collect();

        // Append revoked entries we still know about so the operator
        // can audit "who was previously paired."
        for entry in self.tg.revocations().iter() {
            // Avoid double-listing in the unlikely case the TG
            // returns both active + revoked refs for the same key.
            if rows.iter().any(|r| r.device_pk == hex::encode(entry.device_public_key)) {
                continue;
            }
            let pk = entry.device_public_key;
            let name = self.names.get(&pk).cloned().unwrap_or_else(|| "(revoked)".into());
            let paired_at_ms = self.paired_at_ms.get(&pk).copied().unwrap_or(0);
            rows.push(MemberRow {
                device_pk: hex::encode(pk),
                name,
                role: "viewer".to_string(),
                paired_at_ms,
                revoked: true,
            });
        }
        rows
    }

    /// SPEC §4.4 — revoke a member by `device_pk`. Returns whether the
    /// device was found (so the handler can return 200 vs 404).
    /// Succeeds regardless of online state per §7.6.
    pub fn revoke(&mut self, device_pk_hex: &str) -> RevokeOutcome {
        let Some(pk) = hex_to_arr32(device_pk_hex) else {
            return RevokeOutcome::BadRequest;
        };
        let now_secs = now_secs();
        match self.tg.revoke_device(now_secs, &pk, RevocationReason::ForcedRemoval) {
            Ok(_entry) => RevokeOutcome::Revoked(pk),
            Err(r2_trust::Error::MemberNotFound) => RevokeOutcome::NotFound,
            Err(e) => RevokeOutcome::Other(format!("{e}")),
        }
    }
}

pub enum ClaimOutcome {
    Success(serde_json::Value),
    BadRequest(&'static str),
    /// Same as BadRequest but with a runtime-built string. Two variants
    /// avoid an allocation in the common case.
    BadRequestBoxed(String),
    NotFound,
    Conflict,
    Gone,
}

pub enum RevokeOutcome {
    Revoked([u8; 32]),
    NotFound,
    BadRequest,
    Other(String),
}

/// Convert `BadRequest(&'static)` builder into a String-flavoured
/// `BadRequestBoxed`. The `process_claim` impl uses this for errors
/// whose text is built at runtime.
fn string_box(s: String) -> &'static str {
    // We intentionally leak — these strings are produced on an error
    // path that's not in any hot loop. Total leak per process is
    // bounded by the size of the operator-side typo surface.
    Box::leak(s.into_boxed_str())
}

fn encode_claim_response(
    tg: &TrustGroup,
    encrypted: &EncryptedJoinResponse,
    paired_at_ms: i64,
) -> serde_json::Value {
    // Packed wire format: 24-byte nonce ++ 4-byte BE u32 ciphertext_len
    // ++ ciphertext. This is the layout that r2-wasm's
    // `deserialize_encrypted_response` (consumed by `complete_join`)
    // expects, so the browser-side WASM can decrypt the bundle in one
    // step rather than re-packing two separate b64 fields. See
    // crates/r2-wasm/src/lib.rs:925.
    let mut packed = Vec::with_capacity(28 + encrypted.ciphertext.len());
    packed.extend_from_slice(&encrypted.nonce);
    let ct_len = encrypted.ciphertext.len() as u32;
    packed.extend_from_slice(&ct_len.to_be_bytes());
    packed.extend_from_slice(&encrypted.ciphertext);

    serde_json::json!({
        "encrypted_b64": B64.encode(&packed),
        "tg_pk_hex": hex::encode(tg.verifying_key().to_bytes()),
        "paired_at_ms": paired_at_ms,
    })
}

fn cert_role_name(role: DeviceRole) -> String {
    match role {
        DeviceRole::KeyHolder => "controller".into(),
        DeviceRole::Member => "viewer".into(),
    }
}

/// Render a small PNG QR code as a `data:` URL. The QR holds the
/// `r2:` deeplink so a phone scanning it opens the webapp at the
/// right URL with `?join=` already set.
fn render_qr_png(payload: &str) -> Result<String> {
    use image::Luma;
    use qrcode::QrCode;
    let code = QrCode::new(payload.as_bytes())
        .map_err(|e| format!("QrCode::new: {e}"))?;
    let image = code
        .render::<Luma<u8>>()
        .min_dimensions(256, 256)
        .build();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(format!("data:image/png;base64,{}", B64.encode(&buf)))
}

// ─── small helpers ────────────────────────────────────────────────────

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hex_to_arr16(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 { return None; }
    let bytes = hex::decode(s).ok()?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn hex_to_arr32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 { return None; }
    let bytes = hex::decode(s).ok()?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

/// Spec §4.2 step 5 — `[A-Za-z0-9 ._-]{1,64}`, no leading/trailing space.
fn is_valid_device_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 { return false; }
    if name.starts_with(' ') || name.ends_with(' ') { return false; }
    name.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-')
    })
}

fn urlencode(s: &str) -> String {
    // Minimal URL-component encoder — we only ever embed our own
    // operator-configured relay URL, so the worst case is `:` and `/`.
    s.chars().map(|c| match c {
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
        other => format!("%{:02X}", other as u32),
    }).collect()
}

/// `AppState`-shaped wrapper.
pub type AccessHandle = Arc<Mutex<Access>>;

/// Helper for `main` — try to load Access; log + return `None` on
/// failure so the dashboard still boots without an Access tab on
/// installs that haven't yet generated a TG keypair. The /api/access
/// routes return 503 in that case.
pub async fn maybe_load(local_origin: String, relay_url: Option<String>) -> Option<AccessHandle> {
    let path = default_tg_priv_path();
    match Access::load(&path, local_origin, relay_url) {
        Ok(a) => {
            eprintln!(
                "[access] KeyHolder loaded from {:?} — TG hash {}",
                path,
                a.tg_hash()
            );
            Some(Arc::new(Mutex::new(a)))
        }
        Err(e) => {
            eprintln!(
                "[access] WARNING: TG key not loaded — /api/access/* will return 503. \
                 Reason: {e:#}"
            );
            None
        }
    }
}
