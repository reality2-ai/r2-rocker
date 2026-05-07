//! Hive identity — R2-WIRE §6.2.1 derivation, NVS persistence.
//!
//! Per R2-WIRE §6.2.1, a hive's `hive_id` is **derived deterministically** from
//! two persisted values:
//!
//! ```text
//! hive_id_bytes = HKDF-SHA256(salt = "r2-hive-id-v1",
//!                             ikm  = device_master_secret,
//!                             info = trust_group_id)[0:16]
//! hive_id_uuid  = UUID-format(hive_id_bytes)   (RFC 4122 §4.4)
//! ```
//!
//! The `device_master_secret` is a ≥256-bit random value generated once on
//! first boot, never leaves the device, and never rotates short of explicit
//! factory reset. The `trust_group_id` is the UUID string of the hive's
//! current trust group; on a fresh device it is the device's self-generated
//! trust-group-of-one (R2-TRUST §2.3).
//!
//! Behaviour:
//!   * Reboot, OTA app update, deep sleep → same `hive_id` (deterministic
//!     from persisted master_secret + tg_id).
//!   * Leave then rejoin the same TG → same `hive_id` reinstated.
//!   * Join a different TG → different `hive_id`. Cross-TG identities are
//!     unlinkable at the protocol layer.
//!   * Master-secret loss (factory reset / NVS wipe) → entirely new device
//!     lineage; prior identities cannot be recovered.
//!
//! NVS layout (namespace `"r2"`):
//!   * `master_secret` — 32 raw bytes, blob.
//!   * `tg_id`        — UUID string of the current trust group.
//!   * `hive_id`      — *legacy* UUIDv4 from the pre-§6.2.1 implementation.
//!     If present at boot, this module logs a one-time migration warning,
//!     deletes the legacy key, and generates a fresh master_secret + TG-of-one.
//!     Devices using the old form will appear to the mesh as new hives —
//!     intentional, since the old random UUID has no cryptographic relation to
//!     the new derivation.

use anyhow::{Context, Result};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys::esp_fill_random;
use hkdf::Hkdf;
use log::{info, warn};
use sha2::Sha256;

const NVS_NAMESPACE: &str = "r2";
const NVS_KEY_MASTER_SECRET: &str = "master_secret";
const NVS_KEY_TG_ID: &str = "tg_id";
const NVS_KEY_LEGACY_HIVE_ID: &str = "hive_id";

const MASTER_SECRET_LEN: usize = 32;

/// UUID string length in canonical 8-4-4-4-12 hex form: 36 chars.
const UUID_LEN: usize = 36;

/// HKDF salt for the `hive_id_bytes` derivation. R2-WIRE §6.2.1.
const HIVE_ID_SALT: &[u8] = b"r2-hive-id-v1";

/// A hive's complete §6.2.1-derived identity.
#[derive(Clone)]
pub struct HiveIdentity {
    /// 16 raw bytes from HKDF — what goes in CAPS field 0 (R2-USB §3.6) and
    /// in the §6.4 link-key store key + reconnect HMAC message.
    pub hive_id_bytes: [u8; 16],
    /// UUID-formatted form of `hive_id_bytes` (RFC 4122 §4.4 version 4 + variant
    /// overlay applied). The 36-character display form used in logs and as the
    /// human identity. The wire protocol carries `fnv1a_32(hive_id_uuid.bytes())`.
    pub hive_id_uuid: String,
    /// UUID string of the trust group this hive is currently a member of.
    pub trust_group_id: String,
}

/// Load the persistent identity from NVS, deriving `hive_id` per R2-WIRE §6.2.1.
///
/// On first boot (no `master_secret` in NVS):
///   1. Generate 32 bytes from `esp_fill_random` and persist as `master_secret`.
///   2. Generate a UUIDv4 and persist as `tg_id` (the device's self trust-group-of-one).
///   3. Derive and return the identity.
///
/// On subsequent boots: read both values, derive, return.
pub fn load_identity(nvs_part: EspDefaultNvsPartition) -> Result<HiveIdentity> {
    let mut nvs: EspNvs<NvsDefault> = EspNvs::new(nvs_part, NVS_NAMESPACE, true)
        .context("open NVS namespace 'r2'")?;

    migrate_legacy_hive_id(&mut nvs);

    let master_secret = load_or_generate_master_secret(&mut nvs)?;
    let trust_group_id = load_or_generate_tg_id(&mut nvs)?;

    let identity = derive_identity(&master_secret, &trust_group_id);
    info!(
        "[HIVE_ID] derived per §6.2.1 — hive_id={} (TG={})",
        identity.hive_id_uuid, identity.trust_group_id
    );
    Ok(identity)
}

/// Convenience wrapper preserving the v0.1 API surface — returns the
/// hive_id UUID string only. New code SHOULD use [`load_identity`] to get
/// `hive_id_bytes` and `trust_group_id` as well.
pub fn load_or_generate(nvs_part: EspDefaultNvsPartition) -> Result<String> {
    Ok(load_identity(nvs_part)?.hive_id_uuid)
}

fn load_or_generate_master_secret(
    nvs: &mut EspNvs<NvsDefault>,
) -> Result<[u8; MASTER_SECRET_LEN]> {
    let mut buf = [0u8; MASTER_SECRET_LEN];
    match nvs.get_blob(NVS_KEY_MASTER_SECRET, &mut buf) {
        Ok(Some(slice)) if slice.len() == MASTER_SECRET_LEN => {
            let mut secret = [0u8; MASTER_SECRET_LEN];
            secret.copy_from_slice(slice);
            Ok(secret)
        }
        Ok(Some(other)) => {
            warn!(
                "[HIVE_ID] master_secret in NVS has wrong length {} — regenerating",
                other.len()
            );
            generate_and_store_master_secret(nvs)
        }
        Ok(None) => {
            info!("[HIVE_ID] no master_secret in NVS — generating fresh");
            generate_and_store_master_secret(nvs)
        }
        Err(e) => {
            warn!("[HIVE_ID] master_secret read failed ({e}) — regenerating");
            generate_and_store_master_secret(nvs)
        }
    }
}

fn generate_and_store_master_secret(
    nvs: &mut EspNvs<NvsDefault>,
) -> Result<[u8; MASTER_SECRET_LEN]> {
    let mut secret = [0u8; MASTER_SECRET_LEN];
    // SAFETY: esp_fill_random takes (*mut void, size_t) and writes exactly the
    // requested length of bytes. Buffer is owned and sized correctly.
    unsafe {
        esp_fill_random(secret.as_mut_ptr() as *mut _, secret.len());
    }
    nvs.set_blob(NVS_KEY_MASTER_SECRET, &secret)
        .context("persist master_secret to NVS")?;
    Ok(secret)
}

fn load_or_generate_tg_id(nvs: &mut EspNvs<NvsDefault>) -> Result<String> {
    let mut buf = [0u8; 64];
    match nvs.get_str(NVS_KEY_TG_ID, &mut buf) {
        Ok(Some(existing)) if existing.len() == UUID_LEN => Ok(existing.to_string()),
        Ok(Some(other)) => {
            warn!(
                "[HIVE_ID] tg_id in NVS has wrong length {} — regenerating",
                other.len()
            );
            generate_and_store_tg_id(nvs)
        }
        Ok(None) => {
            info!("[HIVE_ID] no tg_id in NVS — minting fresh trust-group-of-one");
            generate_and_store_tg_id(nvs)
        }
        Err(e) => {
            warn!("[HIVE_ID] tg_id read failed ({e}) — regenerating");
            generate_and_store_tg_id(nvs)
        }
    }
}

fn generate_and_store_tg_id(nvs: &mut EspNvs<NvsDefault>) -> Result<String> {
    let uuid = generate_uuidv4();
    nvs.set_str(NVS_KEY_TG_ID, &uuid)
        .context("persist tg_id to NVS")?;
    Ok(uuid)
}

fn migrate_legacy_hive_id(nvs: &mut EspNvs<NvsDefault>) {
    let mut buf = [0u8; 64];
    if let Ok(Some(legacy)) = nvs.get_str(NVS_KEY_LEGACY_HIVE_ID, &mut buf) {
        if legacy.len() == UUID_LEN {
            warn!(
                "[HIVE_ID] legacy `hive_id` UUIDv4 found in NVS ({legacy}). \
                 Pre-§6.2.1 random-UUID identity has been replaced by the \
                 master_secret + tg_id derivation. Deleting the legacy key; \
                 a fresh master_secret + TG-of-one will be minted. The device \
                 will appear to the mesh as a new hive."
            );
        }
        let _ = nvs.remove(NVS_KEY_LEGACY_HIVE_ID);
    }
}

/// Compute `hive_id_bytes` and the formatted UUID from the persisted inputs.
fn derive_identity(master_secret: &[u8; MASTER_SECRET_LEN], tg_id: &str) -> HiveIdentity {
    let hk = Hkdf::<Sha256>::new(Some(HIVE_ID_SALT), master_secret);
    let mut bytes = [0u8; 16];
    hk.expand(tg_id.as_bytes(), &mut bytes)
        .expect("HKDF expand 16 bytes from SHA-256 always succeeds");
    let hive_id_uuid = format_uuid_v4(bytes);
    HiveIdentity {
        hive_id_bytes: bytes,
        hive_id_uuid,
        trust_group_id: tg_id.to_string(),
    }
}

/// Apply RFC 4122 §4.4 version 4 + variant overlay to 16 raw bytes and format
/// as the canonical 8-4-4-4-12 hex string. The 16-byte `hive_id_bytes` is
/// what hits the wire / CAPS / link-key store; the UUID string is for display.
fn format_uuid_v4(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-\
         {:02x}{:02x}-\
         {:02x}{:02x}-\
         {:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3],
        b[4], b[5],
        b[6], b[7],
        b[8], b[9],
        b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

fn generate_uuidv4() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: esp_fill_random writes exactly the requested length.
    unsafe {
        esp_fill_random(bytes.as_mut_ptr() as *mut _, bytes.len());
    }
    format_uuid_v4(bytes)
}
