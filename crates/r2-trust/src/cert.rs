use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::error::{Error, Result};
use crate::revocation::RevocationSet;
use crate::types::{SigAlgo, DEVICE_CERT_LEN, DEVICE_CERT_VERSION, KEY_LEN, SIGNATURE_LEN};

const CERT_DATA_LEN: usize = DEVICE_CERT_LEN - SIGNATURE_LEN;

/// Device role encoded in certificates (R2-TRUST §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceRole {
    /// Key holder (`0x01`).
    KeyHolder = 0x01,
    /// Standard member (`0x02`).
    Member = 0x02,
}

impl TryFrom<u8> for DeviceRole {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(DeviceRole::KeyHolder),
            0x02 => Ok(DeviceRole::Member),
            other => Err(Error::InvalidRole(other)),
        }
    }
}

impl From<DeviceRole> for u8 {
    fn from(value: DeviceRole) -> Self {
        value as u8
    }
}

/// Device certificate format defined in R2-TRUST §4.1.
///
/// v2 adds `sig_algo` to the wire format for algorithm agility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCertificate {
    /// Certificate format version (must be [`DEVICE_CERT_VERSION`]).
    pub version: u8,
    /// Signature algorithm used.
    pub sig_algo: SigAlgo,
    /// Device's Ed25519 public key (32 bytes).
    pub device_public_key: [u8; KEY_LEN],
    /// Trust group public key hash (32 bytes).
    pub trust_group_id: [u8; KEY_LEN],
    /// Device role within the trust group.
    pub role: DeviceRole,
    /// Issue timestamp (Unix seconds, big-endian on wire).
    pub issued_at: u64,
    /// Expiry timestamp (Unix seconds).
    pub expires_at: u64,
    /// Ed25519 signature over the data portion.
    pub signature: Signature,
}

impl DeviceCertificate {
    /// Issue and sign a new device certificate.
    pub fn issue(
        signer: &SigningKey,
        device_public_key: [u8; KEY_LEN],
        trust_group_id: [u8; KEY_LEN],
        role: DeviceRole,
        issued_at: u64,
        expires_at: u64,
    ) -> Self {
        let sig_algo = SigAlgo::Classical;
        let data = build_data(
            DEVICE_CERT_VERSION,
            sig_algo,
            &device_public_key,
            &trust_group_id,
            role,
            issued_at,
            expires_at,
        );
        let signature = signer.sign(&data);
        DeviceCertificate {
            version: DEVICE_CERT_VERSION,
            sig_algo,
            device_public_key,
            trust_group_id,
            role,
            issued_at,
            expires_at,
            signature,
        }
    }

    /// Verify cryptographic signature, validity window, and revocation state.
    pub fn verify(
        &self,
        trust_group_key: &VerifyingKey,
        now: u64,
        revocations: Option<&RevocationSet>,
    ) -> Result<()> {
        if self.version != DEVICE_CERT_VERSION {
            return Err(Error::InvalidVersion(self.version));
        }
        match self.sig_algo {
            SigAlgo::Classical => { /* supported */ }
            // Future variants would be dispatched here.
        }
        if now < self.issued_at {
            return Err(Error::NotYetValid);
        }
        if now >= self.expires_at {
            return Err(Error::Expired);
        }
        if let Some(set) = revocations {
            if set.contains(&self.device_public_key) {
                return Err(Error::Revoked);
            }
        }
        trust_group_key.verify_strict(&self.data_bytes(), &self.signature)?;
        Ok(())
    }

    /// Serialize into the canonical binary representation.
    pub fn to_bytes(&self) -> [u8; DEVICE_CERT_LEN] {
        let mut out = [0u8; DEVICE_CERT_LEN];
        out[..CERT_DATA_LEN].copy_from_slice(&self.data_bytes());
        out[CERT_DATA_LEN..].copy_from_slice(&self.signature.to_bytes());
        out
    }

    /// Parse a certificate from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != DEVICE_CERT_LEN {
            return Err(Error::PayloadTooShort);
        }
        let version = bytes[0];
        let sig_algo = SigAlgo::try_from(bytes[1])?;
        let mut device_public_key = [0u8; KEY_LEN];
        device_public_key.copy_from_slice(&bytes[2..34]);
        let mut trust_group_id = [0u8; KEY_LEN];
        trust_group_id.copy_from_slice(&bytes[34..66]);
        let role = DeviceRole::try_from(bytes[66])?;
        let mut issued_bytes = [0u8; 8];
        issued_bytes.copy_from_slice(&bytes[67..75]);
        let issued_at = u64::from_be_bytes(issued_bytes);
        let mut expires_bytes = [0u8; 8];
        expires_bytes.copy_from_slice(&bytes[75..83]);
        let expires_at = u64::from_be_bytes(expires_bytes);
        let mut sig_bytes = [0u8; SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&bytes[83..]);
        let signature = Signature::from_bytes(&sig_bytes);
        Ok(DeviceCertificate {
            version,
            sig_algo,
            device_public_key,
            trust_group_id,
            role,
            issued_at,
            expires_at,
            signature,
        })
    }

    fn data_bytes(&self) -> [u8; CERT_DATA_LEN] {
        build_data(
            self.version,
            self.sig_algo,
            &self.device_public_key,
            &self.trust_group_id,
            self.role,
            self.issued_at,
            self.expires_at,
        )
    }
}

fn build_data(
    version: u8,
    sig_algo: SigAlgo,
    device_public_key: &[u8; KEY_LEN],
    trust_group_id: &[u8; KEY_LEN],
    role: DeviceRole,
    issued_at: u64,
    expires_at: u64,
) -> [u8; CERT_DATA_LEN] {
    let mut out = [0u8; CERT_DATA_LEN];
    out[0] = version;
    out[1] = sig_algo.into();
    out[2..34].copy_from_slice(device_public_key);
    out[34..66].copy_from_slice(trust_group_id);
    out[66] = role.into();
    out[67..75].copy_from_slice(&issued_at.to_be_bytes());
    out[75..83].copy_from_slice(&expires_at.to_be_bytes());
    out
}
