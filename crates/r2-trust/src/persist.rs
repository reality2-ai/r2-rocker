//! Platform-agnostic persistence for trust group membership state.
//!
//! Serializes [`MemberState`] and [`TrustGroup`] to/from compact byte
//! representations suitable for storage in any platform's keystore:
//! browser `localStorage`, mobile keychain, filesystem, or MCU flash.
//!
//! ## Security Notes
//!
//! - These bytes contain **secret key material** (`DEV_SK`, `TG_SK`, `DEK`, `HK`).
//! - Callers MUST encrypt or protect the bytes at rest using platform-appropriate
//!   mechanisms (Web Crypto, keychain, encrypted flash).
//! - R2-TRUST §3.3 states DEK/HK SHOULD be derived on demand for key holders.
//!   For members (who lack `TG_SK`), DEK/HK must be persisted — they were received
//!   during the join handshake and cannot be re-derived.
//!
//! ## Wire Format
//!
//! All formats are versioned (first byte = format version) so future changes
//! can be detected and migrated.

use alloc::vec::Vec;

use ed25519_dalek::SigningKey;

use crate::cert::DeviceCertificate;
use crate::error::{Error, Result};
use crate::lifecycle::{MemberState, TrustGroup};
use crate::types::{KEY_LEN, DEVICE_CERT_LEN, MinCryptoLevel};

/// Current format version for member state persistence.
const MEMBER_STATE_VERSION: u8 = 0x01;

/// Current format version for trust group (key holder) persistence.
const TRUST_GROUP_VERSION: u8 = 0x01;

/// Expected byte length of serialized member state (v1).
///
/// 1 (version) + 32 (dev_sk) + 32 (tg_pk) + 147 (cert) + 32 (dek) + 32 (hk) + 1 (crypto_level) = 277
pub const MEMBER_STATE_LEN: usize = 1 + KEY_LEN + KEY_LEN + DEVICE_CERT_LEN + 32 + 32 + 1;

// ---------------------------------------------------------------------------
// MemberState persistence
// ---------------------------------------------------------------------------

/// Serialize a [`MemberState`] to bytes for platform storage.
///
/// Format v1 (277 bytes):
/// ```text
/// [0]       version (0x01)
/// [1..33]   DEV_SK (device secret key, 32 bytes)
/// [33..65]  TG_PK (trust group public key, 32 bytes)
/// [65..212] DeviceCertificate (147 bytes, self-describing)
/// [212..244] DEK (data encryption key, 32 bytes)
/// [244..276] HK (HMAC key, 32 bytes)
/// [276]     MinCryptoLevel (1 byte)
/// ```
pub fn serialize_member_state(state: &MemberState) -> [u8; MEMBER_STATE_LEN] {
    let mut out = [0u8; MEMBER_STATE_LEN];
    let mut pos = 0;

    out[pos] = MEMBER_STATE_VERSION;
    pos += 1;

    out[pos..pos + KEY_LEN].copy_from_slice(&state.device_key().to_bytes());
    pos += KEY_LEN;

    out[pos..pos + KEY_LEN].copy_from_slice(state.trust_group_public().as_bytes());
    pos += KEY_LEN;

    out[pos..pos + DEVICE_CERT_LEN].copy_from_slice(&state.certificate().to_bytes());
    pos += DEVICE_CERT_LEN;

    out[pos..pos + 32].copy_from_slice(state.dek());
    pos += 32;

    out[pos..pos + 32].copy_from_slice(state.hk());
    pos += 32;

    out[pos] = state.min_crypto_level() as u8;

    out
}

/// Deserialize a [`MemberState`] from bytes.
///
/// Returns an error if the format version is unsupported or data is malformed.
pub fn deserialize_member_state(bytes: &[u8]) -> Result<MemberState> {
    if bytes.len() < 1 {
        return Err(Error::PayloadTooShort);
    }
    if bytes[0] != MEMBER_STATE_VERSION {
        return Err(Error::InvalidVersion(bytes[0]));
    }
    if bytes.len() != MEMBER_STATE_LEN {
        return Err(Error::PayloadTooShort);
    }

    let mut pos = 1;

    let mut dev_sk_bytes = [0u8; KEY_LEN];
    dev_sk_bytes.copy_from_slice(&bytes[pos..pos + KEY_LEN]);
    let device_key = SigningKey::from_bytes(&dev_sk_bytes);
    pos += KEY_LEN;

    let mut tg_pk_bytes = [0u8; KEY_LEN];
    tg_pk_bytes.copy_from_slice(&bytes[pos..pos + KEY_LEN]);
    let trust_group_public = ed25519_dalek::VerifyingKey::from_bytes(&tg_pk_bytes)
        .map_err(|_| Error::InvalidPublicKey)?;
    pos += KEY_LEN;

    let certificate = DeviceCertificate::from_bytes(&bytes[pos..pos + DEVICE_CERT_LEN])?;
    pos += DEVICE_CERT_LEN;

    let mut dek = [0u8; 32];
    dek.copy_from_slice(&bytes[pos..pos + 32]);
    pos += 32;

    let mut hk = [0u8; 32];
    hk.copy_from_slice(&bytes[pos..pos + 32]);
    pos += 32;

    let min_crypto_level = MinCryptoLevel::try_from(bytes[pos])?;

    Ok(MemberState::restore(
        device_key,
        trust_group_public,
        certificate,
        dek,
        hk,
        min_crypto_level,
    ))
}

// ---------------------------------------------------------------------------
// TrustGroup (key holder) persistence
// ---------------------------------------------------------------------------

/// Serialize a [`TrustGroup`] key holder state to bytes.
///
/// This serializes the **minimal recovery set**: signing key + sequence + crypto level.
/// Member list and revocations are reconstructed from GROUP_MGMT message history
/// on restore, or can be persisted separately if needed.
///
/// Format v1:
/// ```text
/// [0]       version (0x01)
/// [1..33]   TG_SK (trust group signing key, 32 bytes)
/// [33..37]  sequence (u32, big-endian)
/// [37]      MinCryptoLevel (1 byte)
/// ```
pub const TRUST_GROUP_MINIMAL_LEN: usize = 1 + KEY_LEN + 4 + 1;

/// Serialize trust group key holder state (minimal — signing key + metadata).
pub fn serialize_trust_group_minimal(tg: &TrustGroup) -> [u8; TRUST_GROUP_MINIMAL_LEN] {
    let mut out = [0u8; TRUST_GROUP_MINIMAL_LEN];
    let mut pos = 0;

    out[pos] = TRUST_GROUP_VERSION;
    pos += 1;

    out[pos..pos + KEY_LEN].copy_from_slice(&tg.signing_key().to_bytes());
    pos += KEY_LEN;

    out[pos..pos + 4].copy_from_slice(&tg.sequence().to_be_bytes());
    pos += 4;

    out[pos] = tg.min_crypto_level() as u8;

    out
}

/// Deserialize trust group key holder state (minimal).
///
/// Reconstructs the trust group with an empty member list.
/// Call `process_join_request` to re-add members, or use
/// `serialize_trust_group_full` / `deserialize_trust_group_full`
/// for complete state persistence including members.
pub fn deserialize_trust_group_minimal(bytes: &[u8], now: u64) -> Result<TrustGroup> {
    if bytes.len() < 1 {
        return Err(Error::PayloadTooShort);
    }
    if bytes[0] != TRUST_GROUP_VERSION {
        return Err(Error::InvalidVersion(bytes[0]));
    }
    if bytes.len() != TRUST_GROUP_MINIMAL_LEN {
        return Err(Error::PayloadTooShort);
    }

    let mut pos = 1;

    let mut sk_bytes = [0u8; KEY_LEN];
    sk_bytes.copy_from_slice(&bytes[pos..pos + KEY_LEN]);
    let signing_key = SigningKey::from_bytes(&sk_bytes);
    pos += KEY_LEN;

    let sequence = u32::from_be_bytes([
        bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3],
    ]);
    pos += 4;

    let min_crypto_level = MinCryptoLevel::try_from(bytes[pos])?;

    // Reconstruct with empty member list. The self-cert is re-derived.
    let tg = TrustGroup::from_signing_key(signing_key, now)?;

    // Restore sequence and crypto level via the restore constructor.
    // Since from_signing_key starts at sequence 0, we use restore instead.
    let self_cert = tg.self_certificate().clone();
    let revocations = crate::revocation::RevocationSet::new();
    TrustGroup::restore(
        SigningKey::from_bytes(&sk_bytes),
        self_cert,
        Vec::new(),
        revocations,
        sequence,
        min_crypto_level,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::TrustGroup;

    struct TestRng(u64);

    impl rand_core::RngCore for TestRng {
        fn next_u32(&mut self) -> u32 { self.0 as u32 }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            self.0
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                let val = self.next_u64();
                let bytes = val.to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> core::result::Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl rand_core::CryptoRng for TestRng {}

    #[test]
    fn member_state_roundtrip() {
        let mut rng = TestRng(42);
        let now = 1700000000u64;

        // Create trust group and join a device
        let mut tg = TrustGroup::create(&mut rng, now).unwrap();
        let code = tg.generate_join_code(&mut rng, now, 300);
        let code_value = *code.value();

        let device_key = SigningKey::generate(&mut rng);
        let device_pk = device_key.verifying_key();

        let encrypted = tg.process_join_request(
            &mut rng, now, &code_value, &device_pk,
            "test-device".into(), 365 * 86400,
        ).unwrap();

        let member = MemberState::from_join_response(
            device_key, &tg.verifying_key(), &encrypted, now,
        ).unwrap();

        // Serialize and deserialize
        let bytes = serialize_member_state(&member);
        assert_eq!(bytes.len(), MEMBER_STATE_LEN);

        let restored = deserialize_member_state(&bytes).unwrap();

        // Verify fields match
        assert_eq!(restored.device_key().verifying_key(), member.device_key().verifying_key());
        assert_eq!(restored.trust_group_public(), member.trust_group_public());
        assert_eq!(restored.dek(), member.dek());
        assert_eq!(restored.hk(), member.hk());
        assert_eq!(restored.certificate(), member.certificate());
        assert!(restored.is_valid(now));
    }

    #[test]
    fn trust_group_minimal_roundtrip() {
        let mut rng = TestRng(99);
        let now = 1700000000u64;

        let tg = TrustGroup::create(&mut rng, now).unwrap();
        let original_id = tg.trust_group_id();

        let bytes = serialize_trust_group_minimal(&tg);
        assert_eq!(bytes.len(), TRUST_GROUP_MINIMAL_LEN);

        let restored = deserialize_trust_group_minimal(&bytes, now).unwrap();
        assert_eq!(restored.trust_group_id(), original_id);
        assert_eq!(restored.derived_keys().dek, tg.derived_keys().dek);
        assert_eq!(restored.derived_keys().hk, tg.derived_keys().hk);
    }
}
