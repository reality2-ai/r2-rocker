use alloc::vec::Vec;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::error::{Error, Result};
use crate::types::{SigAlgo, GROUP_MGMT_VERSION, KEY_LEN, SIGNATURE_LEN};

/// Header: version(1) + sig_algo(1) + opcode(1) + tgid(32) + sender_pk(32) + seq(4) + ts(8) + payload_len(2)
const HEADER_LEN: usize = 1 + 1 + 1 + KEY_LEN + KEY_LEN + 4 + 8 + 2;

/// GROUP_MGMT opcodes (R2-TRUST §10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupMgmtOpCode {
    /// Device requests to join a trust group.
    JoinRequest = 0x01,
    /// Key holder responds with encrypted credentials.
    JoinResponse = 0x02,
    /// Device voluntarily leaves the group.
    Leave = 0x03,
    /// Key holder revokes a device's certificate.
    Revoke = 0x04,
    /// Group-wide key rotation.
    KeyRotation = 0x05,
    /// Acknowledgement. NOTE: opcode 0x06 is reserved at the spec layer
    /// (was `grk_rotation`, removed in v0.3); used in-crate for `Ack` —
    /// this is an implementation-internal value not yet pinned in the
    /// R2-TRUST §10.1 opcode table.
    Ack = 0x06,
    /// Key holder invites a target device to join (R2-TRUST §10.1, §10.4 +
    /// R2-PROVISION §3.1). Payload is a 161-byte [`JoinInvite`]; receiver
    /// has no prior credentials for the sender and verifies the invite's
    /// embedded signature against `issuer_pk` carried inline.
    JoinInvite = 0x07,
}

impl TryFrom<u8> for GroupMgmtOpCode {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(GroupMgmtOpCode::JoinRequest),
            0x02 => Ok(GroupMgmtOpCode::JoinResponse),
            0x03 => Ok(GroupMgmtOpCode::Leave),
            0x04 => Ok(GroupMgmtOpCode::Revoke),
            0x05 => Ok(GroupMgmtOpCode::KeyRotation),
            0x06 => Ok(GroupMgmtOpCode::Ack),
            0x07 => Ok(GroupMgmtOpCode::JoinInvite),
            other => Err(Error::InvalidOpcode(other)),
        }
    }
}

impl From<GroupMgmtOpCode> for u8 {
    fn from(value: GroupMgmtOpCode) -> Self {
        value as u8
    }
}

/// GROUP_MGMT message (R2-TRUST §9.2).
///
/// v2 adds `sig_algo` to the wire format for algorithm agility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMgmtMessage {
    /// Wire format version.
    pub version: u8,
    /// Signature algorithm.
    pub sig_algo: SigAlgo,
    /// Operation code.
    pub opcode: GroupMgmtOpCode,
    /// Trust group public key hash.
    pub trust_group_id: [u8; KEY_LEN],
    /// Sender's device public key.
    pub sender_pk: [u8; KEY_LEN],
    /// Monotonic sequence number (replay protection).
    pub sequence: u32,
    /// Unix timestamp (seconds).
    pub timestamp: u64,
    /// Opcode-specific payload.
    pub payload: Vec<u8>,
    /// Ed25519 signature over the message header + payload.
    pub signature: Signature,
}

impl GroupMgmtMessage {
    /// Create a new unsigned message.
    pub fn new(
        opcode: GroupMgmtOpCode,
        trust_group_id: [u8; KEY_LEN],
        sender_pk: [u8; KEY_LEN],
        sequence: u32,
        timestamp: u64,
        payload: Vec<u8>,
    ) -> Self {
        GroupMgmtMessage {
            version: GROUP_MGMT_VERSION,
            sig_algo: SigAlgo::Classical,
            opcode,
            trust_group_id,
            sender_pk,
            sequence,
            timestamp,
            payload,
            signature: empty_signature(),
        }
    }

    /// Sign the message with the sender's key.
    pub fn sign(&mut self, signer: &SigningKey) {
        match self.sig_algo {
            SigAlgo::Classical => {
                self.signature = signer.sign(&self.data_bytes());
            }
        }
    }

    /// Verify the signature with the sender's public key.
    pub fn verify(&self, sender_key: &VerifyingKey) -> Result<()> {
        match self.sig_algo {
            SigAlgo::Classical => {
                sender_key.verify_strict(&self.data_bytes(), &self.signature)?;
            }
        }
        Ok(())
    }

    /// Encode into bytes (header + payload + signature).
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.len() > u16::MAX as usize {
            return Err(Error::PayloadTooLarge);
        }
        let mut out = self.data_bytes();
        out.extend_from_slice(&self.signature.to_bytes());
        Ok(out)
    }

    /// Decode from bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN + SIGNATURE_LEN {
            return Err(Error::PayloadTooShort);
        }
        let version = bytes[0];
        let sig_algo = SigAlgo::try_from(bytes[1])?;
        let opcode = GroupMgmtOpCode::try_from(bytes[2])?;
        let mut trust_group_id = [0u8; KEY_LEN];
        trust_group_id.copy_from_slice(&bytes[3..35]);
        let mut sender_pk = [0u8; KEY_LEN];
        sender_pk.copy_from_slice(&bytes[35..67]);
        let mut seq_bytes = [0u8; 4];
        seq_bytes.copy_from_slice(&bytes[67..71]);
        let sequence = u32::from_be_bytes(seq_bytes);
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&bytes[71..79]);
        let timestamp = u64::from_be_bytes(ts_bytes);
        let mut len_bytes = [0u8; 2];
        len_bytes.copy_from_slice(&bytes[79..81]);
        let payload_len = u16::from_be_bytes(len_bytes) as usize;
        let expected_total = HEADER_LEN + payload_len + SIGNATURE_LEN;
        if bytes.len() != expected_total {
            return Err(Error::PayloadTooShort);
        }
        let payload_slice = &bytes[81..81 + payload_len];
        let payload = payload_slice.to_vec();
        let mut sig_bytes = [0u8; SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&bytes[expected_total - SIGNATURE_LEN..]);
        let signature = Signature::from_bytes(&sig_bytes);
        Ok(GroupMgmtMessage {
            version,
            sig_algo,
            opcode,
            trust_group_id,
            sender_pk,
            sequence,
            timestamp,
            payload,
            signature,
        })
    }

    fn data_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push(self.version);
        out.push(self.sig_algo.into());
        out.push(self.opcode.into());
        out.extend_from_slice(&self.trust_group_id);
        out.extend_from_slice(&self.sender_pk);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

fn empty_signature() -> Signature {
    Signature::from_bytes(&[0u8; SIGNATURE_LEN])
}
