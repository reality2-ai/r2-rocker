use alloc::vec::Vec;

use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::Result;
use crate::types::KEY_LEN;

/// Derived trust group keys (DEK + HK).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivedGroupKeys {
    /// Data encryption key (for payload encryption within the trust group).
    pub dek: [u8; 32],
    /// HMAC key (for message authentication within the trust group).
    pub hk: [u8; 32],
}

/// Derived entanglement (peering) keys for cross-group communication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeeringKeys {
    /// HMAC key for cross-group message authentication.
    pub hmac: [u8; 32],
    /// Encryption key for cross-group payload encryption.
    pub enc: [u8; 32],
}

/// Derive trust group DEK and HK from the signing key.
pub fn derive_group_keys(signing_key: &SigningKey) -> Result<DerivedGroupKeys> {
    let sk = signing_key.to_bytes();
    let pk = signing_key.verifying_key().to_bytes();
    derive_group_keys_raw(&sk, &pk)
}

/// Derive trust group keys from raw key bytes.
pub fn derive_group_keys_raw(
    trust_group_secret: &[u8; KEY_LEN],
    trust_group_public: &[u8; KEY_LEN],
) -> Result<DerivedGroupKeys> {
    let dek = derive_label(trust_group_secret, trust_group_public, b"R2-TRUST-v0.1-DEK")?;
    let hk = derive_label(
        trust_group_secret,
        trust_group_public,
        b"R2-TRUST-v0.1-HMAC",
    )?;
    Ok(DerivedGroupKeys { dek, hk })
}

/// Derive peering keys from the shared secret produced by X25519.
pub fn derive_peering_keys(
    shared_secret: &[u8; 32],
    trust_group_a: &[u8; KEY_LEN],
    trust_group_b: &[u8; KEY_LEN],
) -> Result<PeeringKeys> {
    // Lexicographic ordering ensures both sides derive the same keys (R2-TRUST §7.5).
    let (first, second) = if trust_group_a <= trust_group_b {
        (trust_group_a, trust_group_b)
    } else {
        (trust_group_b, trust_group_a)
    };
    let mut salt = Vec::with_capacity(KEY_LEN * 2);
    salt.extend_from_slice(first);
    salt.extend_from_slice(second);
    let hmac = hkdf_expand(shared_secret, &salt, b"R2-TRUST-v0.1-PEER-HMAC")?;
    let enc = hkdf_expand(shared_secret, &salt, b"R2-TRUST-v0.1-PEER-ENC")?;
    Ok(PeeringKeys { hmac, enc })
}

/// HKDF key derivation helper for trust group keys (info = label || TG_PK).
fn derive_label(
    trust_group_secret: &[u8; KEY_LEN],
    trust_group_public: &[u8; KEY_LEN],
    label: &[u8],
) -> Result<[u8; 32]> {
    let mut info = Vec::with_capacity(label.len() + KEY_LEN);
    info.extend_from_slice(label);
    info.extend_from_slice(trust_group_public);
    hkdf_expand(trust_group_secret, trust_group_public, &info)
}

pub(crate) fn hkdf_expand(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)?;
    Ok(okm)
}
