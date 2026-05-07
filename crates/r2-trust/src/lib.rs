//! # r2-trust
//!
//! Trust group security for [Reality2](https://github.com/reality2-ai),
//! implementing the R2-TRUST specification (see [`SPEC.md`](../SPEC.md) in this crate).
//!
//! Provides device certificates, trust group key derivation (HKDF),
//! join protocol encryption (X25519 + XChaCha20-Poly1305), group management
//! messages, and certificate revocation.
//!
//! ## Key Types
//!
//! - [`DeviceCertificate`] — Ed25519-signed device identity (R2-TRUST §4.1)
//! - [`DerivedGroupKeys`] — DEK + HK derived from trust group secret (§3.1)
//! - [`JoinCode`] — 128-bit code for join protocol handshake (§5)
//! - [`GroupMgmtMessage`] — signed management operations (§9)
//! - [`RevocationSet`] — certificate revocation tracking (§8)
//!
//! ## Algorithm Agility
//!
//! All wire formats include algorithm identifiers ([`SigAlgo`], [`KemAlgo`])
//! for future post-quantum hybrid support. Currently only classical
//! algorithms are implemented (Ed25519 / X25519).

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

/// Device certificates — issue, serialize, verify (SPEC.md §2).
pub mod cert;
/// Error types.
pub mod error;
/// GROUP_MGMT wire messages (SPEC.md §5).
pub mod group_mgmt;
/// HKDF key derivation for trust groups and peering (SPEC.md §3).
pub mod hkdf;
/// Join protocol — X25519 key exchange + encrypted response (SPEC.md §4).
pub mod join;
/// Certificate revocation (SPEC.md §6).
pub mod revocation;
/// Algorithm identifiers and wire-format constants (SPEC.md §1).
pub mod types;
/// Trust group lifecycle orchestration (R2-TRUST §4–§6).
pub mod lifecycle;
/// Concrete HMAC providers for R2-WIRE frame authentication (R2-TRUST §6).
pub mod wire_hmac;
/// Platform-agnostic persistence for trust group membership state.
pub mod persist;

pub use cert::{DeviceCertificate, DeviceRole};
pub use error::{Error, Result};
pub use group_mgmt::{GroupMgmtMessage, GroupMgmtOpCode};
pub use hkdf::{derive_group_keys, derive_peering_keys, DerivedGroupKeys, PeeringKeys};
pub use join::{
    decrypt_join_response, encrypt_join_response, EncryptedJoinResponse, JoinCode,
    JoinInvite, JoinRequestPayload, JoinResponseBundle,
};
pub use revocation::{RevocationEntry, RevocationReason, RevocationSet};
pub use types::{KemAlgo, MinCryptoLevel, SigAlgo};
pub use lifecycle::{MemberInfo, MemberState, TrustGroup};
pub use wire_hmac::{GroupHmac, PeeringHmac};

#[cfg(test)]
mod tests;
