//! Trust group lifecycle orchestration (R2-TRUST §4–§6).
//!
//! This module wires together the low-level cryptographic primitives
//! ([`DeviceCertificate`], [`JoinCode`], [`DerivedGroupKeys`], etc.)
//! into a coherent ceremony API that mirrors the trust lifecycle
//! described in the R2-TRUST specification.
//!
//! ## Design
//!
//! - **`no_std`-first**: uses `alloc` only, no filesystem or network.
//! - **Pure logic**: callers handle persistence (serialize / deserialize).
//! - **Two roles**: [`TrustGroup`] (key holder) and [`MemberState`] (device).
//!
//! ## Typical flow
//!
//! ```text
//! key holder                          joining device
//! ──────────                          ──────────────
//! TrustGroup::create()
//! generate_join_code()  ──(OOB)──▶    (receives code)
//!                       ◀──────────   build JoinRequestPayload
//! process_join_request()──────────▶   MemberState::from_join_response()
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core::{CryptoRng, RngCore};

use crate::cert::{DeviceCertificate, DeviceRole};
use crate::error::{Error, Result};
use crate::hkdf::{derive_group_keys, DerivedGroupKeys};
use crate::join::{
    decrypt_join_response, encrypt_join_response, EncryptedJoinResponse, JoinCode,
    JoinResponseBundle,
};
use crate::revocation::{RevocationEntry, RevocationReason, RevocationSet};
use crate::types::{MinCryptoLevel, KEY_LEN};

/// Default certificate validity: 1 year in seconds.
pub const DEFAULT_CERT_TTL_SECS: u64 = 365 * 24 * 60 * 60;

/// Default join code validity: 5 minutes in seconds (mirrors Anthill).
pub const DEFAULT_JOIN_CODE_TTL_SECS: u64 = 300;

/// Metadata tracked alongside a member's certificate.
///
/// Device names come from R2-PROVISION `device_info`, not from the
/// certificate itself, so we store them as sidecar data.
#[derive(Clone, Debug)]
pub struct MemberInfo {
    /// The device's Ed25519 certificate.
    pub certificate: DeviceCertificate,
    /// Human-readable name (set at join time).
    pub name: String,
}

/// Key-holder side of a trust group.
///
/// Holds the trust group signing key, derived keys, member list,
/// active join codes, and revocation set. Mirrors the role that
/// Anthill's `ColonyTrust` plays, but with full R2-TRUST spec crypto.
pub struct TrustGroup {
    /// Trust group Ed25519 signing key (the root secret).
    signing_key: SigningKey,
    /// HKDF-derived DEK + HK.
    derived_keys: DerivedGroupKeys,
    /// Key holder's own certificate.
    self_cert: DeviceCertificate,
    /// Provisioned members (excludes the key holder).
    members: Vec<MemberInfo>,
    /// Active (unexpired, unused) join codes.
    join_codes: Vec<JoinCode>,
    /// Certificate revocation set (G-Set CRDT).
    revocations: RevocationSet,
    /// Monotonic sequence counter for GROUP_MGMT messages.
    sequence: u32,
    /// Minimum cryptographic level for this group.
    min_crypto_level: MinCryptoLevel,
}

impl TrustGroup {
    /// Create a brand-new trust group.
    ///
    /// Generates a fresh Ed25519 keypair, derives DEK/HK, and
    /// self-issues a key-holder certificate. This is the "creation
    /// ceremony" (R2-TRUST §4.2).
    pub fn create<R: RngCore + CryptoRng>(rng: &mut R, now: u64) -> Result<Self> {
        let signing_key = SigningKey::generate(rng);
        Self::from_signing_key(signing_key, now)
    }

    /// Restore a trust group from an existing signing key.
    ///
    /// Re-derives keys and re-issues the key-holder certificate.
    /// Use this when loading from persisted state.
    pub fn from_signing_key(signing_key: SigningKey, now: u64) -> Result<Self> {
        let derived_keys = derive_group_keys(&signing_key)?;
        let tg_id = *signing_key.verifying_key().as_bytes();

        let self_cert = DeviceCertificate::issue(
            &signing_key,
            tg_id, // key holder's public key IS the TG public key
            tg_id,
            DeviceRole::KeyHolder,
            now,
            now + DEFAULT_CERT_TTL_SECS,
        );

        Ok(TrustGroup {
            signing_key,
            derived_keys,
            self_cert,
            members: Vec::new(),
            join_codes: Vec::new(),
            revocations: RevocationSet::new(),
            sequence: 0,
            min_crypto_level: MinCryptoLevel::Classical,
        })
    }

    /// Restore full state (signing key + members + revocations + sequence).
    ///
    /// For loading from persistence where members were already provisioned.
    pub fn restore(
        signing_key: SigningKey,
        self_cert: DeviceCertificate,
        members: Vec<MemberInfo>,
        revocations: RevocationSet,
        sequence: u32,
        min_crypto_level: MinCryptoLevel,
    ) -> Result<Self> {
        let derived_keys = derive_group_keys(&signing_key)?;
        Ok(TrustGroup {
            signing_key,
            derived_keys,
            self_cert,
            members,
            join_codes: Vec::new(), // join codes are ephemeral
            revocations,
            sequence,
            min_crypto_level,
        })
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Trust group signing key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Trust group public key (also serves as the trust group ID).
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Trust group ID (SHA-256 of public key is just the raw public key bytes
    /// for classical Ed25519).
    pub fn trust_group_id(&self) -> [u8; KEY_LEN] {
        *self.signing_key.verifying_key().as_bytes()
    }

    /// HKDF-derived group keys (DEK + HK).
    pub fn derived_keys(&self) -> &DerivedGroupKeys {
        &self.derived_keys
    }

    /// Key holder's own certificate.
    pub fn self_certificate(&self) -> &DeviceCertificate {
        &self.self_cert
    }

    /// All provisioned members (not including key holder).
    pub fn members(&self) -> &[MemberInfo] {
        &self.members
    }

    /// Certificate revocation set.
    pub fn revocations(&self) -> &RevocationSet {
        &self.revocations
    }

    /// Current GROUP_MGMT sequence number.
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Whether no devices have joined yet (only key holder exists).
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Minimum cryptographic level for this group.
    pub fn min_crypto_level(&self) -> MinCryptoLevel {
        self.min_crypto_level
    }

    // ── Join code management ──────────────────────────────────────────

    /// Generate a new join code (R2-TRUST §5.2).
    ///
    /// Expired codes are cleaned up on every call, mirroring
    /// Anthill's `generate_join_code()` pattern.
    pub fn generate_join_code<R: RngCore + CryptoRng>(
        &mut self,
        rng: &mut R,
        now: u64,
        ttl_secs: u64,
    ) -> &JoinCode {
        // Clean expired codes first (Anthill pattern).
        self.cleanup_expired_codes(now);

        let code = JoinCode::generate(rng, now + ttl_secs);
        self.join_codes.push(code);
        self.join_codes.last().expect("just pushed")
    }

    /// Remove expired and used join codes.
    pub fn cleanup_expired_codes(&mut self, now: u64) {
        self.join_codes
            .retain(|c| c.expires_at() > now && c.validate(c.value(), now).is_ok());
    }

    /// Number of active (unexpired, unused) join codes.
    pub fn active_join_code_count(&self) -> usize {
        self.join_codes.len()
    }

    /// Inject an externally-created join code (e.g. restored from persistence).
    pub fn inject_join_code(&mut self, code: JoinCode) {
        self.join_codes.push(code);
    }

    /// Read-only access to active join codes (for persistence/snapshotting).
    pub fn join_codes(&self) -> &[JoinCode] {
        &self.join_codes
    }

    /// Check if a join code is valid without consuming it.
    pub fn validate_join_code(&self, candidate: &[u8; 16], now: u64) -> bool {
        self.join_codes.iter().any(|c| c.validate(candidate, now).is_ok())
    }

    // ── Join protocol ─────────────────────────────────────────────────

    /// Process a join request and produce an encrypted response bundle.
    ///
    /// This is the key holder's side of the join handshake (R2-TRUST §5.2):
    /// 1. Validate the join code (constant-time, single-use).
    /// 2. Issue a device certificate.
    /// 3. Encrypt the response bundle (cert + DEK + HK) using X25519.
    /// 4. Record the new member.
    ///
    /// Mirrors Anthill's `provision_device()` but with full spec crypto.
    pub fn process_join_request<R: RngCore + CryptoRng>(
        &mut self,
        rng: &mut R,
        now: u64,
        join_code_candidate: &[u8; 16],
        device_public_key: &VerifyingKey,
        device_name: String,
        cert_ttl_secs: u64,
    ) -> Result<EncryptedJoinResponse> {
        // 1. Validate and consume the join code.
        let code_idx = self
            .join_codes
            .iter()
            .position(|c| c.validate(join_code_candidate, now).is_ok())
            .ok_or(Error::InvalidJoinCode)?;
        self.join_codes[code_idx].mark_used();

        // Clean up expired/used codes.
        self.cleanup_expired_codes(now);

        // 2. Check device isn't already a member or revoked.
        let dpk = *device_public_key.as_bytes();
        if self.revocations.contains(&dpk) {
            return Err(Error::Revoked);
        }
        if self.members.iter().any(|m| m.certificate.device_public_key == dpk) {
            return Err(Error::DuplicateMember);
        }

        // 3. Issue device certificate.
        let cert = DeviceCertificate::issue(
            &self.signing_key,
            dpk,
            self.trust_group_id(),
            DeviceRole::Member,
            now,
            now + cert_ttl_secs,
        );

        // 4. Build and encrypt the response bundle.
        let bundle = JoinResponseBundle::new(
            cert.clone(),
            self.derived_keys.dek,
            self.derived_keys.hk,
            self.min_crypto_level,
        );
        let encrypted = encrypt_join_response(rng, &self.signing_key, device_public_key, &bundle)?;

        // 5. Record the member.
        self.members.push(MemberInfo {
            certificate: cert,
            name: device_name,
        });
        self.sequence += 1;

        Ok(encrypted)
    }

    // ── Revocation ────────────────────────────────────────────────────

    /// Revoke a device's certificate (R2-TRUST §6).
    ///
    /// Issues a signed revocation entry, adds it to the G-Set,
    /// and removes the device from the member list.
    /// Returns the revocation entry for distribution to other members.
    pub fn revoke_device(
        &mut self,
        now: u64,
        device_public_key: &[u8; KEY_LEN],
        reason: RevocationReason,
    ) -> Result<RevocationEntry> {
        // Check device is actually a member.
        let was_member = self
            .members
            .iter()
            .any(|m| &m.certificate.device_public_key == device_public_key);
        if !was_member {
            return Err(Error::MemberNotFound);
        }

        let entry =
            RevocationEntry::issue(&self.signing_key, *device_public_key, now, reason);
        self.revocations.add(entry.clone());

        // Remove from active members.
        self.members
            .retain(|m| &m.certificate.device_public_key != device_public_key);
        self.sequence += 1;

        Ok(entry)
    }

    /// Process a voluntary leave from a device.
    ///
    /// The device has signed a Leave GROUP_MGMT message; the key holder
    /// records the revocation and removes the member.
    pub fn process_leave(
        &mut self,
        now: u64,
        device_public_key: &[u8; KEY_LEN],
    ) -> Result<RevocationEntry> {
        self.revoke_device(now, device_public_key, RevocationReason::VoluntaryLeave)
    }

    /// Look up a member by public key.
    pub fn find_member(&self, device_public_key: &[u8; KEY_LEN]) -> Option<&MemberInfo> {
        self.members
            .iter()
            .find(|m| &m.certificate.device_public_key == device_public_key)
    }
}

// ── Device (member) side ──────────────────────────────────────────────────

/// Device-side trust group membership state.
///
/// Created when a device successfully completes the join handshake.
/// Holds the device's own signing key, its issued certificate,
/// and the trust group's derived keys.
pub struct MemberState {
    /// Device's own Ed25519 signing key.
    device_key: SigningKey,
    /// Trust group public key.
    trust_group_public: VerifyingKey,
    /// Device certificate issued by the key holder.
    certificate: DeviceCertificate,
    /// Trust group data encryption key.
    dek: [u8; 32],
    /// Trust group HMAC key.
    hk: [u8; 32],
    /// Minimum cryptographic level for the group.
    min_crypto_level: MinCryptoLevel,
}

impl MemberState {
    /// Complete the join handshake by decrypting the response bundle.
    ///
    /// The joining device calls this after receiving the encrypted
    /// join response from the key holder (R2-TRUST §5.2).
    pub fn from_join_response(
        device_key: SigningKey,
        trust_group_public: &VerifyingKey,
        encrypted: &EncryptedJoinResponse,
        now: u64,
    ) -> Result<Self> {
        let bundle = decrypt_join_response(&device_key, trust_group_public, encrypted)?;

        // Verify the certificate we received is valid and for us.
        bundle
            .certificate
            .verify(trust_group_public, now, None)?;

        let expected_dpk = device_key.verifying_key().to_bytes();
        if bundle.certificate.device_public_key != expected_dpk {
            return Err(Error::InvalidPublicKey);
        }

        Ok(MemberState {
            device_key,
            trust_group_public: *trust_group_public,
            certificate: bundle.certificate,
            dek: bundle.dek,
            hk: bundle.hk,
            min_crypto_level: bundle.min_crypto_level,
        })
    }

    /// Restore from previously persisted state.
    pub fn restore(
        device_key: SigningKey,
        trust_group_public: VerifyingKey,
        certificate: DeviceCertificate,
        dek: [u8; 32],
        hk: [u8; 32],
        min_crypto_level: MinCryptoLevel,
    ) -> Self {
        MemberState {
            device_key,
            trust_group_public,
            certificate,
            dek,
            hk,
            min_crypto_level,
        }
    }

    /// Device's signing key.
    pub fn device_key(&self) -> &SigningKey {
        &self.device_key
    }

    /// Trust group public key.
    pub fn trust_group_public(&self) -> &VerifyingKey {
        &self.trust_group_public
    }

    /// Device certificate.
    pub fn certificate(&self) -> &DeviceCertificate {
        &self.certificate
    }

    /// Data encryption key.
    pub fn dek(&self) -> &[u8; 32] {
        &self.dek
    }

    /// HMAC key.
    pub fn hk(&self) -> &[u8; 32] {
        &self.hk
    }

    /// Minimum cryptographic level.
    pub fn min_crypto_level(&self) -> MinCryptoLevel {
        self.min_crypto_level
    }

    /// Check if the certificate is still valid at the given time.
    pub fn is_valid(&self, now: u64) -> bool {
        self.certificate
            .verify(&self.trust_group_public, now, None)
            .is_ok()
    }
}
