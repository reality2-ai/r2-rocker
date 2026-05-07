use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Algorithm identifiers (R2-TRUST algorithm agility)
// ---------------------------------------------------------------------------

/// Signature algorithm identifier.
///
/// Carried in certificates, GROUP_MGMT messages, and revocation entries so the
/// wire format is future-proof for post-quantum hybrids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SigAlgo {
    /// Ed25519 (classical).
    Classical = 0x01,
    // PqHybrid = 0x02, // reserved — Ed25519 + ML-DSA-65 hybrid
}

impl TryFrom<u8> for SigAlgo {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(SigAlgo::Classical),
            other => Err(Error::UnsupportedAlgorithm(other)),
        }
    }
}

impl From<SigAlgo> for u8 {
    fn from(value: SigAlgo) -> Self {
        value as u8
    }
}

/// Key-encapsulation mechanism identifier.
///
/// Carried in join requests so the two sides can negotiate which KEM to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KemAlgo {
    /// X25519 (classical ECDH).
    Classical = 0x01,
    // PqHybrid = 0x02, // reserved — X25519 + ML-KEM-768 hybrid
}

impl TryFrom<u8> for KemAlgo {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(KemAlgo::Classical),
            other => Err(Error::UnsupportedAlgorithm(other)),
        }
    }
}

impl From<KemAlgo> for u8 {
    fn from(value: KemAlgo) -> Self {
        value as u8
    }
}

/// Minimum cryptographic level a group requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MinCryptoLevel {
    /// Classical algorithms only (Ed25519/X25519).
    Classical = 0x01,
    /// Post-quantum hybrid required.
    PqHybrid = 0x02,
}

impl TryFrom<u8> for MinCryptoLevel {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(MinCryptoLevel::Classical),
            0x02 => Ok(MinCryptoLevel::PqHybrid),
            other => Err(Error::UnsupportedAlgorithm(other)),
        }
    }
}

impl From<MinCryptoLevel> for u8 {
    fn from(value: MinCryptoLevel) -> Self {
        value as u8
    }
}

// ---------------------------------------------------------------------------
// Wire-format constants — apply to `SigAlgo::Classical` / `KemAlgo::Classical`
// ---------------------------------------------------------------------------

/// Length in bytes of a trust group or device public key (`SigAlgo::Classical`).
pub const KEY_LEN: usize = 32;
/// Ed25519 signature length (`SigAlgo::Classical`).
pub const SIGNATURE_LEN: usize = 64;
/// Device certificate version per R2-TRUST §4.1 (v2 includes `sig_algo`).
pub const DEVICE_CERT_VERSION: u8 = 0x02;
/// Device certificate binary length for `SigAlgo::Classical` (including signature).
///
/// v2 layout: version(1) + sig_algo(1) + dpk(32) + tgid(32) + role(1)
///            + issued_at(8) + expires_at(8) + signature(64) = 147
pub const DEVICE_CERT_LEN: usize = 147;
/// Join code size (128-bit random value).
pub const JOIN_CODE_LEN: usize = 16;
/// Length of the nonce supplied in join requests.
pub const JOIN_NONCE_LEN: usize = 32;
/// Nonce size for XChaCha20-Poly1305 in join responses.
pub const JOIN_RESPONSE_NONCE_LEN: usize = 24;
/// Total size of a join response bundle (certificate + DEK + HK + min_crypto_level).
pub const JOIN_RESPONSE_BUNDLE_LEN: usize = DEVICE_CERT_LEN + 65;
/// Total size of a `JoinInvite` per R2-PROVISION §3.1
/// (invite_code + trust_group_id + issuer_pk + created_at + expires_at + max_uses + signature).
pub const JOIN_INVITE_LEN: usize =
    JOIN_CODE_LEN + KEY_LEN + KEY_LEN + 8 + 8 + 1 + SIGNATURE_LEN;
/// Bytes covered by the `JoinInvite` signature (everything except the trailing 64-byte signature).
pub const JOIN_INVITE_SIGNED_LEN: usize = JOIN_INVITE_LEN - SIGNATURE_LEN;
/// GROUP_MGMT message version (R2-TRUST §9.2, v2 includes `sig_algo`).
pub const GROUP_MGMT_VERSION: u8 = 0x02;
