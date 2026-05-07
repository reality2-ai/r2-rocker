use alloc::vec::Vec;

use chacha20poly1305::{aead::Aead, Key, KeyInit, XChaCha20Poly1305, XNonce};
use curve25519_dalek::edwards::CompressedEdwardsY;
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand_core::{CryptoRng, RngCore};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};

use crate::cert::DeviceCertificate;
use crate::error::{Error, Result};
use crate::hkdf::hkdf_expand;
use crate::types::{
    KemAlgo, MinCryptoLevel, DEVICE_CERT_LEN, JOIN_CODE_LEN, JOIN_INVITE_LEN,
    JOIN_INVITE_SIGNED_LEN, JOIN_NONCE_LEN, JOIN_RESPONSE_BUNDLE_LEN,
    JOIN_RESPONSE_NONCE_LEN, KEY_LEN, SIGNATURE_LEN,
};

/// Join code used during provisioning (R2-TRUST §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinCode {
    value: [u8; JOIN_CODE_LEN],
    expires_at: u64,
    used: bool,
}

impl JoinCode {
    /// Generate a new join code with a given expiration time.
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R, expires_at: u64) -> Self {
        let mut value = [0u8; JOIN_CODE_LEN];
        rng.fill_bytes(&mut value);
        JoinCode {
            value,
            expires_at,
            used: false,
        }
    }

    /// Reconstruct a join code from persisted bytes (e.g. loaded from disk).
    pub fn from_raw(value: [u8; JOIN_CODE_LEN], expires_at: u64) -> Self {
        JoinCode {
            value,
            expires_at,
            used: false,
        }
    }

    /// Validate a candidate join code.
    pub fn validate(&self, candidate: &[u8; JOIN_CODE_LEN], now: u64) -> Result<()> {
        if now >= self.expires_at {
            return Err(Error::JoinCodeExpired);
        }
        if self.used {
            return Err(Error::InvalidJoinCode);
        }
        if self.value.ct_eq(candidate).unwrap_u8() == 1 {
            Ok(())
        } else {
            Err(Error::InvalidJoinCode)
        }
    }

    /// Mark the join code as used.
    pub fn mark_used(&mut self) {
        self.used = true;
    }

    /// Access the raw value.
    pub fn value(&self) -> &[u8; JOIN_CODE_LEN] {
        &self.value
    }

    /// Expiration timestamp.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

/// Signed pre-trust invite delivered from a key holder to a target device that
/// is being asked to join a trust group (R2-PROVISION §3.1, GROUP_MGMT opcode
/// `0x07` per R2-TRUST §10.1).
///
/// Wire layout (161 bytes):
/// ```text
///   0..16    invite_code     (CSPRNG random)
///   16..48   trust_group_id  (TG_PK, Ed25519)
///   48..80   issuer_pk       (DEV_PK of the provisioner)
///   80..88   created_at      (u64 big-endian, Unix seconds)
///   88..96   expires_at      (u64 big-endian, Unix seconds)
///   96       max_uses        (u8, 1..=255 — 0 is invalid)
///   97..161  signature       (Ed25519 over bytes 0..97, signed by issuer_sk)
/// ```
///
/// The receiver has no prior credentials for the issuer — verification uses
/// the inline `issuer_pk` (trust-on-presentation, justified by proximity per
/// R2-PROVISION §7.1). A `JoinInvite` does not by itself confer trust-group
/// membership; it is consumed by a subsequent `join_request` / `join_response`
/// exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinInvite {
    /// 128-bit join code, copied into the matching `join_request` (R2-TRUST §5.2.1).
    pub invite_code: [u8; JOIN_CODE_LEN],
    /// TG_PK — also the trust group identifier (R2-TRUST §2.2).
    pub trust_group_id: [u8; KEY_LEN],
    /// Provisioner's DEV_PK; used by the receiver to verify the embedded signature.
    pub issuer_pk: [u8; KEY_LEN],
    /// Unix seconds at which the invite was generated.
    pub created_at: u64,
    /// Unix seconds at which the invite expires (R2-PROVISION §3.1: default 900 s).
    pub expires_at: u64,
    /// Maximum allowed uses. `1` for ordinary single-use invites; up to `255`
    /// for batch-provisioning invites (R2-PROVISION §3.1). `0` is invalid.
    pub max_uses: u8,
    /// Ed25519 signature by the issuer's `DEV_SK` over bytes 0..97 of the wire form.
    pub signature: [u8; SIGNATURE_LEN],
}

impl JoinInvite {
    /// Build and sign a fresh invite. Caller supplies an already-generated
    /// `invite_code` (typically from [`JoinCode::value`]) and the trust
    /// group's identity, so the same code can be tracked alongside the
    /// `JoinCode` book-keeping on the key-holder side.
    pub fn new_signed(
        invite_code: [u8; JOIN_CODE_LEN],
        trust_group_id: [u8; KEY_LEN],
        issuer_sk: &SigningKey,
        created_at: u64,
        expires_at: u64,
        max_uses: u8,
    ) -> Self {
        let issuer_pk = *issuer_sk.verifying_key().as_bytes();
        let mut tbs = [0u8; JOIN_INVITE_SIGNED_LEN];
        write_signed_region(
            &mut tbs,
            &invite_code,
            &trust_group_id,
            &issuer_pk,
            created_at,
            expires_at,
            max_uses,
        );
        let signature = issuer_sk.sign(&tbs).to_bytes();
        JoinInvite {
            invite_code,
            trust_group_id,
            issuer_pk,
            created_at,
            expires_at,
            max_uses,
            signature,
        }
    }

    /// Serialize to the canonical 161-byte wire form.
    pub fn to_bytes(&self) -> [u8; JOIN_INVITE_LEN] {
        let mut out = [0u8; JOIN_INVITE_LEN];
        write_signed_region(
            (&mut out[..JOIN_INVITE_SIGNED_LEN])
                .try_into()
                .expect("slice length checked"),
            &self.invite_code,
            &self.trust_group_id,
            &self.issuer_pk,
            self.created_at,
            self.expires_at,
            self.max_uses,
        );
        out[JOIN_INVITE_SIGNED_LEN..].copy_from_slice(&self.signature);
        out
    }

    /// Parse from the canonical wire form. Does **not** verify the signature
    /// or expiry — callers MUST follow with [`Self::verify`] before acting
    /// on the contents.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != JOIN_INVITE_LEN {
            return Err(Error::PayloadTooShort);
        }
        let mut invite_code = [0u8; JOIN_CODE_LEN];
        invite_code.copy_from_slice(&bytes[0..JOIN_CODE_LEN]);
        let mut trust_group_id = [0u8; KEY_LEN];
        trust_group_id
            .copy_from_slice(&bytes[JOIN_CODE_LEN..JOIN_CODE_LEN + KEY_LEN]);
        let mut issuer_pk = [0u8; KEY_LEN];
        issuer_pk.copy_from_slice(
            &bytes[JOIN_CODE_LEN + KEY_LEN..JOIN_CODE_LEN + 2 * KEY_LEN],
        );
        let off = JOIN_CODE_LEN + 2 * KEY_LEN;
        let mut ts8 = [0u8; 8];
        ts8.copy_from_slice(&bytes[off..off + 8]);
        let created_at = u64::from_be_bytes(ts8);
        ts8.copy_from_slice(&bytes[off + 8..off + 16]);
        let expires_at = u64::from_be_bytes(ts8);
        let max_uses = bytes[off + 16];
        let mut signature = [0u8; SIGNATURE_LEN];
        signature.copy_from_slice(&bytes[JOIN_INVITE_SIGNED_LEN..JOIN_INVITE_LEN]);
        Ok(JoinInvite {
            invite_code,
            trust_group_id,
            issuer_pk,
            created_at,
            expires_at,
            max_uses,
            signature,
        })
    }

    /// Verify the embedded signature against `issuer_pk`, check expiry, and
    /// reject structurally invalid invites (e.g. `max_uses == 0`). Returns
    /// `Ok(())` if the invite is acceptable for use as a join offer.
    ///
    /// This does not by itself authenticate the *issuer* to any prior trust
    /// anchor — it only confirms that the invite was signed by whoever owns
    /// `issuer_pk` and hasn't expired. Acceptance on a first-contact basis
    /// is justified at the protocol layer by physical proximity (R2-PROVISION
    /// §7.1).
    pub fn verify(&self, now: u64) -> Result<()> {
        if self.max_uses == 0 {
            return Err(Error::InvalidJoinCode);
        }
        if now >= self.expires_at {
            return Err(Error::JoinCodeExpired);
        }
        let issuer_vk = VerifyingKey::from_bytes(&self.issuer_pk)
            .map_err(|_| Error::InvalidPublicKey)?;
        let mut tbs = [0u8; JOIN_INVITE_SIGNED_LEN];
        write_signed_region(
            &mut tbs,
            &self.invite_code,
            &self.trust_group_id,
            &self.issuer_pk,
            self.created_at,
            self.expires_at,
            self.max_uses,
        );
        let sig = Signature::from_bytes(&self.signature);
        issuer_vk
            .verify_strict(&tbs, &sig)
            .map_err(|_| Error::Signature)?;
        Ok(())
    }
}

fn write_signed_region(
    out: &mut [u8; JOIN_INVITE_SIGNED_LEN],
    invite_code: &[u8; JOIN_CODE_LEN],
    trust_group_id: &[u8; KEY_LEN],
    issuer_pk: &[u8; KEY_LEN],
    created_at: u64,
    expires_at: u64,
    max_uses: u8,
) {
    out[0..JOIN_CODE_LEN].copy_from_slice(invite_code);
    out[JOIN_CODE_LEN..JOIN_CODE_LEN + KEY_LEN].copy_from_slice(trust_group_id);
    out[JOIN_CODE_LEN + KEY_LEN..JOIN_CODE_LEN + 2 * KEY_LEN].copy_from_slice(issuer_pk);
    let off = JOIN_CODE_LEN + 2 * KEY_LEN;
    out[off..off + 8].copy_from_slice(&created_at.to_be_bytes());
    out[off + 8..off + 16].copy_from_slice(&expires_at.to_be_bytes());
    out[off + 16] = max_uses;
}

/// Join request payload (kem_algo + join code + anti-replay nonce).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequestPayload {
    /// Key encapsulation mechanism to use.
    pub kem_algo: KemAlgo,
    /// The join code (must match key holder's code).
    pub join_code: [u8; JOIN_CODE_LEN],
    /// Anti-replay nonce from the joining device.
    pub nonce: [u8; JOIN_NONCE_LEN],
}

impl JoinRequestPayload {
    /// Create a new join request with classical KEM.
    pub fn new(join_code: [u8; JOIN_CODE_LEN], nonce: [u8; JOIN_NONCE_LEN]) -> Self {
        JoinRequestPayload {
            kem_algo: KemAlgo::Classical,
            join_code,
            nonce,
        }
    }

    /// Serialize to wire format.
    pub fn encode(&self) -> [u8; 1 + JOIN_CODE_LEN + JOIN_NONCE_LEN] {
        let mut out = [0u8; 1 + JOIN_CODE_LEN + JOIN_NONCE_LEN];
        out[0] = self.kem_algo.into();
        out[1..1 + JOIN_CODE_LEN].copy_from_slice(&self.join_code);
        out[1 + JOIN_CODE_LEN..].copy_from_slice(&self.nonce);
        out
    }

    /// Parse from wire format.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 1 + JOIN_CODE_LEN + JOIN_NONCE_LEN {
            return Err(Error::PayloadTooShort);
        }
        let kem_algo = KemAlgo::try_from(bytes[0])?;
        let mut join_code = [0u8; JOIN_CODE_LEN];
        join_code.copy_from_slice(&bytes[1..1 + JOIN_CODE_LEN]);
        let mut nonce = [0u8; JOIN_NONCE_LEN];
        nonce.copy_from_slice(&bytes[1 + JOIN_CODE_LEN..]);
        Ok(JoinRequestPayload {
            kem_algo,
            join_code,
            nonce,
        })
    }
}

/// Bundle delivered in a join response (certificate + DEK + HK + min_crypto_level).
///
/// Per R2-TRUST §5.2, the encrypted join response includes the trust group's
/// minimum cryptographic level so the joining device can enforce it during
/// entanglement negotiation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinResponseBundle {
    /// The issued device certificate.
    pub certificate: DeviceCertificate,
    /// Data encryption key for the trust group.
    pub dek: [u8; 32],
    /// HMAC key for the trust group.
    pub hk: [u8; 32],
    /// Minimum cryptographic level required by this trust group.
    pub min_crypto_level: MinCryptoLevel,
}

impl JoinResponseBundle {
    /// Create a new bundle.
    pub fn new(
        certificate: DeviceCertificate,
        dek: [u8; 32],
        hk: [u8; 32],
        min_crypto_level: MinCryptoLevel,
    ) -> Self {
        JoinResponseBundle {
            certificate,
            dek,
            hk,
            min_crypto_level,
        }
    }

    /// Serialize to wire format.
    pub fn to_bytes(&self) -> [u8; JOIN_RESPONSE_BUNDLE_LEN] {
        let mut out = [0u8; JOIN_RESPONSE_BUNDLE_LEN];
        out[..DEVICE_CERT_LEN].copy_from_slice(&self.certificate.to_bytes());
        out[DEVICE_CERT_LEN..DEVICE_CERT_LEN + 32].copy_from_slice(&self.dek);
        out[DEVICE_CERT_LEN + 32..DEVICE_CERT_LEN + 64].copy_from_slice(&self.hk);
        out[DEVICE_CERT_LEN + 64] = self.min_crypto_level.into();
        out
    }

    /// Parse from wire format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != JOIN_RESPONSE_BUNDLE_LEN {
            return Err(Error::PayloadTooShort);
        }
        let certificate = DeviceCertificate::from_bytes(&bytes[..DEVICE_CERT_LEN])?;
        let mut dek = [0u8; 32];
        dek.copy_from_slice(&bytes[DEVICE_CERT_LEN..DEVICE_CERT_LEN + 32]);
        let mut hk = [0u8; 32];
        hk.copy_from_slice(&bytes[DEVICE_CERT_LEN + 32..DEVICE_CERT_LEN + 64]);
        let min_crypto_level = MinCryptoLevel::try_from(bytes[DEVICE_CERT_LEN + 64])?;
        Ok(JoinResponseBundle {
            certificate,
            dek,
            hk,
            min_crypto_level,
        })
    }
}

/// Encrypted join response container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedJoinResponse {
    /// XChaCha20-Poly1305 nonce (24 bytes).
    pub nonce: [u8; JOIN_RESPONSE_NONCE_LEN],
    /// Encrypted bundle (certificate + DEK + HK) with authentication tag.
    pub ciphertext: Vec<u8>,
}

/// Encrypt the join response bundle for a joining device.
pub fn encrypt_join_response<R: RngCore + CryptoRng>(
    rng: &mut R,
    trust_group_key: &SigningKey,
    device_public: &VerifyingKey,
    bundle: &JoinResponseBundle,
) -> Result<EncryptedJoinResponse> {
    let shared = derive_shared_secret(trust_group_key, device_public)?;
    let key = derive_join_key(shared.as_bytes(), trust_group_key, device_public)?;
    let cipher = XChaCha20Poly1305::new(&key);

    let mut nonce = [0u8; JOIN_RESPONSE_NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    let plaintext = bundle.to_bytes();
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| Error::Encryption)?;

    Ok(EncryptedJoinResponse { nonce, ciphertext })
}

/// Decrypt a join response bundle using the joining device's key.
pub fn decrypt_join_response(
    device_secret: &SigningKey,
    trust_group_public: &VerifyingKey,
    encrypted: &EncryptedJoinResponse,
) -> Result<JoinResponseBundle> {
    let shared = derive_shared_secret_device(device_secret, trust_group_public)?;
    let key = derive_join_key(shared.as_bytes(), trust_group_public, device_secret)?;
    let cipher = XChaCha20Poly1305::new(&key);
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&encrypted.nonce),
            encrypted.ciphertext.as_ref(),
        )
        .map_err(|_| Error::Decryption)?;
    JoinResponseBundle::from_bytes(&plaintext)
}

fn derive_join_key(
    shared_secret: &[u8; 32],
    tg_key: &impl PublicKeyBytes,
    device_key: &impl PublicKeyBytes,
) -> Result<Key> {
    let tg_bytes = tg_key.public_bytes();
    let dev_bytes = device_key.public_bytes();
    let mut salt = Vec::with_capacity(KEY_LEN * 2);
    salt.extend_from_slice(&tg_bytes);
    salt.extend_from_slice(&dev_bytes);
    let okm = hkdf_expand(shared_secret, &salt, b"R2-TRUST-v0.1-JOIN")?;
    Ok(*Key::from_slice(&okm))
}

fn derive_shared_secret(
    trust_group_key: &SigningKey,
    device_public: &VerifyingKey,
) -> Result<SharedSecret> {
    let tg_secret = ed25519_secret_to_x25519(trust_group_key);
    let device_public = ed25519_public_to_x25519(device_public)?;
    Ok(tg_secret.diffie_hellman(&device_public))
}

fn derive_shared_secret_device(
    device_secret: &SigningKey,
    trust_group_public: &VerifyingKey,
) -> Result<SharedSecret> {
    let device_secret = ed25519_secret_to_x25519(device_secret);
    let trust_group_public = ed25519_public_to_x25519(trust_group_public)?;
    Ok(device_secret.diffie_hellman(&trust_group_public))
}

fn ed25519_secret_to_x25519(secret: &SigningKey) -> StaticSecret {
    use sha2::Digest;

    let hash = sha2::Sha512::digest(secret.to_bytes());
    let mut clamped = [0u8; 32];
    clamped.copy_from_slice(&hash[..32]);
    clamped[0] &= 248;
    clamped[31] &= 127;
    clamped[31] |= 64;
    StaticSecret::from(clamped)
}

fn ed25519_public_to_x25519(public: &VerifyingKey) -> Result<PublicKey> {
    let compressed = CompressedEdwardsY(*public.as_bytes());
    let edwards = compressed.decompress().ok_or(Error::InvalidPublicKey)?;
    let montgomery: MontgomeryPoint = edwards.to_montgomery();
    Ok(PublicKey::from(montgomery.to_bytes()))
}

trait PublicKeyBytes {
    fn public_bytes(&self) -> [u8; KEY_LEN];
}

impl PublicKeyBytes for SigningKey {
    fn public_bytes(&self) -> [u8; KEY_LEN] {
        self.verifying_key().to_bytes()
    }
}

impl PublicKeyBytes for VerifyingKey {
    fn public_bytes(&self) -> [u8; KEY_LEN] {
        self.to_bytes()
    }
}
