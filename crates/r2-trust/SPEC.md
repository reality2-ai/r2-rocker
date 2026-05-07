# R2-TRUST: Trust Group Security for Reality2

**Version:** 0.1.0
**Status:** Active development

---

## 1. Purpose

R2-TRUST implements the security layer for Reality2 trust groups. A trust
group is a set of devices that share cryptographic material and can
authenticate each other's messages.

## 2. Device Certificates

Ed25519-signed certificates bind a device's public key to a trust group.

**v2 wire format** (147 bytes):

```
version(1) | sig_algo(1) | device_pk(32) | tg_id(32) | role(1)
| issued_at(8) | expires_at(8) | signature(64)
```

**Roles:** KeyHolder (0x01) — can issue certificates and manage the group.
Member (0x02) — standard participant.

**Verification:** signature check → validity window → revocation check.

## 3. Key Derivation (HKDF)

All group keys are derived from the trust group signing key via HKDF-SHA256.

### 3.1 Group Keys (DEK + HK)

```
DEK = HKDF(ikm=TG_SK, salt=TG_PK, info="R2-TRUST-v0.1-DEK" || TG_PK)
HK  = HKDF(ikm=TG_SK, salt=TG_PK, info="R2-TRUST-v0.1-HMAC" || TG_PK)
```

- **DEK** — Data Encryption Key (payload encryption within the group)
- **HK** — HMAC Key (message authentication, used by R2-WIRE)

### 3.2 Peering Keys

For cross-group (entanglement) communication:

```
shared = X25519(TG_A_SK, TG_B_PK)
HMAC_key = HKDF(ikm=shared, salt=TG_A_PK||TG_B_PK, info="R2-TRUST-v0.1-PEER-HMAC")
ENC_key  = HKDF(ikm=shared, salt=TG_A_PK||TG_B_PK, info="R2-TRUST-v0.1-PEER-ENC")
```

## 4. Join Protocol

Provisioning a new device into a trust group:

1. Key holder generates a **JoinCode** (128-bit random, time-limited)
2. Joining device sends **JoinRequest**: `kem_algo(1) | join_code(16) | nonce(32)`
3. Key holder validates code, issues certificate, encrypts response
4. **JoinResponse**: X25519 key agreement → HKDF → XChaCha20-Poly1305 encryption
   of `certificate(147) | DEK(32) | HK(32)` = 211 bytes plaintext

Ed25519 keys are converted to X25519 for the DH exchange (RFC 8032 → RFC 7748).

## 5. GROUP_MGMT Messages

Signed management operations for trust group lifecycle:

**v2 wire format:**

```
version(1) | sig_algo(1) | opcode(1) | tg_id(32) | sender_pk(32)
| sequence(4) | timestamp(8) | payload_len(2) | payload(N) | signature(64)
```

**Opcodes:**

| Code | Name | Description |
|------|------|-------------|
| 0x01 | JoinRequest | Device requests membership |
| 0x02 | JoinResponse | Key holder sends encrypted credentials |
| 0x03 | Leave | Voluntary departure |
| 0x04 | Revoke | Key holder revokes a device |
| 0x05 | KeyRotation | Group-wide key rotation |
| 0x06 | Ack | Acknowledgement |

## 6. Revocation

**Entry format:**

```
sig_algo(1) | device_pk(32) | revoked_at(8) | reason(1) | signature(64)
```

**Reasons:** VoluntaryLeave (0x01), ForcedRemoval (0x02), KeyCompromise (0x03).

Revocations are checked during certificate verification.

## 7. Algorithm Agility

All wire formats include `sig_algo` / `kem_algo` identifiers:

| ID | Algorithm | Status |
|----|-----------|--------|
| 0x01 | Ed25519 / X25519 (classical) | Implemented |
| 0x02 | ML-DSA-65 + Ed25519 / ML-KEM-768 + X25519 (PQ hybrid) | Reserved |

The `min_crypto_level` field in trust group configuration specifies the
minimum acceptable level (Classical or PqHybrid).

## 8. Constants

| Name | Value | Description |
|------|-------|-------------|
| `KEY_LEN` | 32 | Public/private key size |
| `SIGNATURE_LEN` | 64 | Ed25519 signature |
| `DEVICE_CERT_VERSION` | 0x02 | Current cert version |
| `DEVICE_CERT_LEN` | 147 | Total cert wire size |
| `JOIN_CODE_LEN` | 16 | Join code (128-bit) |
| `JOIN_NONCE_LEN` | 32 | Anti-replay nonce |
| `JOIN_RESPONSE_NONCE_LEN` | 24 | XChaCha20 nonce |
| `JOIN_RESPONSE_BUNDLE_LEN` | 212 | Cert + DEK + HK + min_crypto_level |
| `GROUP_MGMT_VERSION` | 0x02 | Current message version |

## 9. Implementation Status

This crate implements the **cryptographic primitives** from the full R2-TRUST
specification. The following maps crate coverage to the normative spec
(`r2-specifications/specs/r2-core/R2-TRUST.md`):

### Implemented

| Spec Section | Crate Module | Notes |
|-------------|-------------|-------|
| §3.1 Derived keys (DEK, HK) | `hkdf.rs` | HKDF-SHA256, deterministic test vectors |
| §4.1 Certificate format | `cert.rs` | v2 wire format (147 bytes), Ed25519 signing |
| §4.2 Certificate validity | `cert.rs` | Signature + time window + revocation check |
| §4.4 Revocation | `revocation.rs` | Signed entries, RevocationSet lookup |
| §5.2 Join flow | `join.rs` | JoinCode, X25519 DH → HKDF → XChaCha20-Poly1305 |
| §6 HMAC envelope | `wire_hmac.rs` | `GroupHmac` / `PeeringHmac` implement `r2_wire::HmacProvider` |
| §7.5 Bilateral peering keys | `hkdf.rs` | `derive_peering_keys()` — function exists |
| §10 GROUP_MGMT protocol | `group_mgmt.rs` | v2 wire format, 6 opcodes, sign/verify |
| Algorithm agility | `types.rs` | SigAlgo/KemAlgo enums, PQ hybrid reserved |

### Not Yet Implemented

| Spec Section | Description | Priority |
|-------------|-------------|----------|
| §5.1 Trust group creation | No creation ceremony / key generation flow | Medium |
| §5.3 Leave protocol | Opcode defined, no leave-side logic (cert deletion, notification) | Medium |
| §5.4 Dissolve | No dissolution protocol | Low |
| §5.5 Key holder transfer | No handover ceremony | Medium |
| §7 Entanglement (full) | Peering key derivation exists; no negotiation protocol, tiers, keep-alive, `@entangled` routing | Medium |
| §8 Membership tiers | Full/Leaf/Guest — no tier-aware certificate validation | Low |
| §9 Cloud nodes | Self-deactivation, constraints | Low |
| §10.3 Idempotent retransmission | No sequence tracking / replay rejection | Medium |
| §11 Key rotation | Opcode 0x05 defined, no rotation ceremony, grace period, or re-keying | **High** |
| §13.10 Replay protection | No nonce/sequence dedup in GROUP_MGMT processing | Medium |
| §13.11 Forward secrecy | No ephemeral key exchange | Low |
| §13.12 Post-quantum | ML-DSA-65 + ML-KEM-768 reserved, not implemented | Future |

### Integration Gaps

- **r2-wire HMAC**: `hkdf.rs` derives HK, but no code calls HMAC-SHA256 on
  wire frames. This is the critical path to authenticated messaging.
- **r2-route peering**: `derive_peering_keys()` exists but no cross-group
  routing integration.
- **Platform integration**: No `r2-demo` code uses r2-trust yet — all current
  mesh traffic is unauthenticated.

---

*This spec is self-contained. For HMAC usage in wire framing, see r2-wire SPEC.md.
For the full normative specification, see R2-TRUST.md in r2-specifications.*
