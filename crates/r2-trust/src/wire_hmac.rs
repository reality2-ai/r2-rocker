//! Concrete [`HmacProvider`] implementation using HKDF-derived HMAC keys.
//!
//! This module bridges R2-TRUST key derivation with R2-WIRE frame
//! authentication, implementing the HMAC envelope described in
//! R2-TRUST §6.2 and R2-WIRE §10.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use r2_trust::wire_hmac::GroupHmac;
//! use r2_wire::{sign_compact, verify_compact, classify_compact};
//!
//! let hmac = GroupHmac::new(derived_keys.hk);
//! let (flags, tag) = sign_compact(&msg, &hmac);
//! // ... encode and send ...
//!
//! // On receive:
//! let class = classify_compact(&received, Some(&hmac));
//! ```

use hmac::{Hmac, Mac};
use sha2::Sha256;

use r2_wire::hmac::{COMPACT_TAG_LEN, EXTENDED_TAG_LEN, HmacProvider};

type HmacSha256 = Hmac<Sha256>;

/// HMAC provider for intra-trust-group authentication.
///
/// Wraps a 32-byte HMAC key (`HK`) derived from the trust group
/// secret via HKDF (R2-TRUST §3.1).
#[derive(Clone)]
pub struct GroupHmac {
    hk: [u8; 32],
}

impl GroupHmac {
    /// Create a new provider from the HKDF-derived HMAC key.
    pub fn new(hk: [u8; 32]) -> Self {
        Self { hk }
    }

    /// Access the raw key (for diagnostics / test vectors only).
    pub fn key(&self) -> &[u8; 32] {
        &self.hk
    }
}

impl HmacProvider for GroupHmac {
    fn mac_compact(&self, authenticated_bytes: &[u8]) -> [u8; COMPACT_TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.hk)
            .expect("HMAC key length is always valid");
        mac.update(authenticated_bytes);
        let result = mac.finalize().into_bytes();
        let mut tag = [0u8; COMPACT_TAG_LEN];
        tag.copy_from_slice(&result[..COMPACT_TAG_LEN]);
        tag
    }

    fn mac_extended(&self, authenticated_bytes: &[u8]) -> [u8; EXTENDED_TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.hk)
            .expect("HMAC key length is always valid");
        mac.update(authenticated_bytes);
        let result = mac.finalize().into_bytes();
        let mut tag = [0u8; EXTENDED_TAG_LEN];
        tag.copy_from_slice(&result);
        tag
    }
}

/// HMAC provider for bilateral peering authentication.
///
/// Wraps a 32-byte peering HMAC key derived from X25519 shared
/// secret via HKDF (R2-TRUST §7.5.3).
#[derive(Clone)]
pub struct PeeringHmac {
    key: [u8; 32],
}

impl PeeringHmac {
    /// Create from a peering HMAC key.
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }
}

impl HmacProvider for PeeringHmac {
    fn mac_compact(&self, authenticated_bytes: &[u8]) -> [u8; COMPACT_TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length is always valid");
        mac.update(authenticated_bytes);
        let result = mac.finalize().into_bytes();
        let mut tag = [0u8; COMPACT_TAG_LEN];
        tag.copy_from_slice(&result[..COMPACT_TAG_LEN]);
        tag
    }

    fn mac_extended(&self, authenticated_bytes: &[u8]) -> [u8; EXTENDED_TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length is always valid");
        mac.update(authenticated_bytes);
        let result = mac.finalize().into_bytes();
        let mut tag = [0u8; EXTENDED_TAG_LEN];
        tag.copy_from_slice(&result);
        tag
    }
}
