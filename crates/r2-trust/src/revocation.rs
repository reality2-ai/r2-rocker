use alloc::vec::Vec;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::error::{Error, Result};
use crate::types::{SigAlgo, KEY_LEN, SIGNATURE_LEN};

/// Data: sig_algo(1) + device_pk(32) + revoked_at(8) + reason(1) = 42
const REVOCATION_DATA_LEN: usize = 1 + KEY_LEN + 8 + 1;
const REVOCATION_LEN: usize = REVOCATION_DATA_LEN + SIGNATURE_LEN;

/// Reason codes defined in R2-TRUST §4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationReason {
    /// Device left voluntarily.
    VoluntaryLeave = 0x01,
    /// Key holder removed the device.
    ForcedRemoval = 0x02,
    /// Device key believed compromised.
    KeyCompromise = 0x03,
}

impl TryFrom<u8> for RevocationReason {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(RevocationReason::VoluntaryLeave),
            0x02 => Ok(RevocationReason::ForcedRemoval),
            0x03 => Ok(RevocationReason::KeyCompromise),
            other => Err(Error::InvalidReason(other)),
        }
    }
}

impl From<RevocationReason> for u8 {
    fn from(value: RevocationReason) -> Self {
        value as u8
    }
}

/// Revocation entry as defined in R2-TRUST §4.4.
///
/// v2 adds `sig_algo` to the wire format for algorithm agility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevocationEntry {
    /// Signature algorithm.
    pub sig_algo: SigAlgo,
    /// Public key of the revoked device.
    pub device_public_key: [u8; KEY_LEN],
    /// Timestamp of revocation (Unix seconds).
    pub revoked_at: u64,
    /// Reason for revocation.
    pub reason: RevocationReason,
    /// Ed25519 signature by the trust group key holder.
    pub signature: Signature,
}

impl RevocationEntry {
    /// Create and sign a new revocation entry.
    pub fn issue(
        signer: &SigningKey,
        device_public_key: [u8; KEY_LEN],
        revoked_at: u64,
        reason: RevocationReason,
    ) -> Self {
        let sig_algo = SigAlgo::Classical;
        let data = build_revocation_data(sig_algo, &device_public_key, revoked_at, reason);
        let signature = signer.sign(&data);
        RevocationEntry {
            sig_algo,
            device_public_key,
            revoked_at,
            reason,
            signature,
        }
    }

    /// Verify the revocation entry signature.
    pub fn verify(&self, trust_group_key: &VerifyingKey) -> Result<()> {
        match self.sig_algo {
            SigAlgo::Classical => {
                trust_group_key.verify_strict(&self.data_bytes(), &self.signature)?;
            }
        }
        Ok(())
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> [u8; REVOCATION_LEN] {
        let mut out = [0u8; REVOCATION_LEN];
        out[..REVOCATION_DATA_LEN].copy_from_slice(&self.data_bytes());
        out[REVOCATION_DATA_LEN..].copy_from_slice(&self.signature.to_bytes());
        out
    }

    /// Parse from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != REVOCATION_LEN {
            return Err(Error::PayloadTooShort);
        }
        let sig_algo = SigAlgo::try_from(bytes[0])?;
        let mut device_public_key = [0u8; KEY_LEN];
        device_public_key.copy_from_slice(&bytes[1..1 + KEY_LEN]);
        let mut revoked_bytes = [0u8; 8];
        revoked_bytes.copy_from_slice(&bytes[1 + KEY_LEN..1 + KEY_LEN + 8]);
        let revoked_at = u64::from_be_bytes(revoked_bytes);
        let reason = RevocationReason::try_from(bytes[1 + KEY_LEN + 8])?;
        let mut sig_bytes = [0u8; SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&bytes[REVOCATION_DATA_LEN..]);
        let signature = Signature::from_bytes(&sig_bytes);
        Ok(RevocationEntry {
            sig_algo,
            device_public_key,
            revoked_at,
            reason,
            signature,
        })
    }

    fn data_bytes(&self) -> [u8; REVOCATION_DATA_LEN] {
        build_revocation_data(self.sig_algo, &self.device_public_key, self.revoked_at, self.reason)
    }
}

fn build_revocation_data(
    sig_algo: SigAlgo,
    device_public_key: &[u8; KEY_LEN],
    revoked_at: u64,
    reason: RevocationReason,
) -> [u8; REVOCATION_DATA_LEN] {
    let mut out = [0u8; REVOCATION_DATA_LEN];
    out[0] = sig_algo.into();
    out[1..1 + KEY_LEN].copy_from_slice(device_public_key);
    out[1 + KEY_LEN..1 + KEY_LEN + 8].copy_from_slice(&revoked_at.to_be_bytes());
    out[1 + KEY_LEN + 8] = reason.into();
    out
}

/// Grow-only set of revocations (G-Set CRDT).
#[derive(Clone, Debug, Default)]
pub struct RevocationSet {
    entries: Vec<RevocationEntry>,
}

impl RevocationSet {
    /// Create an empty revocation set.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Seed from an existing vector.
    pub fn from_entries(entries: Vec<RevocationEntry>) -> Self {
        Self { entries }
    }

    /// Add an entry if it is new (device key not already revoked).
    pub fn add(&mut self, entry: RevocationEntry) {
        if self.contains(&entry.device_public_key) {
            return;
        }
        self.entries.push(entry);
    }

    /// Check if a device key has been revoked.
    pub fn contains(&self, device_public_key: &[u8; KEY_LEN]) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.device_public_key == *device_public_key)
    }

    /// Iterate over entries.
    pub fn iter(&self) -> impl Iterator<Item = &RevocationEntry> {
        self.entries.iter()
    }
}
