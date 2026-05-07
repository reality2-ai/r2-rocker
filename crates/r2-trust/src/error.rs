use displaydoc::Display;

/// Result type for r2-trust operations.
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Errors emitted by the r2-trust crate.
#[derive(Debug, Clone, PartialEq, Eq, Display)]
pub enum Error {
    /// Invalid version byte {0}
    InvalidVersion(u8),
    /// Unknown opcode value {0}
    InvalidOpcode(u8),
    /// Unknown role value {0}
    InvalidRole(u8),
    /// Unknown reason value {0}
    InvalidReason(u8),
    /// Signature verification failed
    Signature,
    /// Certificate expired
    Expired,
    /// Certificate not yet valid
    NotYetValid,
    /// Certificate revoked
    Revoked,
    /// Payload too short
    PayloadTooShort,
    /// Payload too large
    PayloadTooLarge,
    /// Invalid join code
    InvalidJoinCode,
    /// Join code expired
    JoinCodeExpired,
    /// Nonce length invalid
    InvalidNonce,
    /// Invalid public key encoding
    InvalidPublicKey,
    /// Encryption failure
    Encryption,
    /// Decryption failure
    Decryption,
    /// HKDF expansion failure
    Hkdf,
    /// Unsupported algorithm identifier {0}
    UnsupportedAlgorithm(u8),
    /// Device already a member of this trust group
    DuplicateMember,
    /// Device not found in trust group membership
    MemberNotFound,
}

impl From<ed25519_dalek::SignatureError> for Error {
    fn from(_: ed25519_dalek::SignatureError) -> Self {
        Error::Signature
    }
}

impl From<chacha20poly1305::aead::Error> for Error {
    fn from(_: chacha20poly1305::aead::Error) -> Self {
        Error::Decryption
    }
}

impl From<hkdf::InvalidLength> for Error {
    fn from(_: hkdf::InvalidLength) -> Self {
        Error::Hkdf
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}
