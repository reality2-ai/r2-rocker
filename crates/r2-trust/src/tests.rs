use alloc::vec::Vec;

use ed25519_dalek::SigningKey;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use alloc::string::String;

use crate::{
    cert::DeviceCertificate,
    derive_group_keys, encrypt_join_response,
    group_mgmt::{GroupMgmtMessage, GroupMgmtOpCode},
    hkdf::derive_peering_keys,
    join::{decrypt_join_response, JoinInvite, JoinRequestPayload, JoinResponseBundle},
    lifecycle::{MemberState, TrustGroup, DEFAULT_CERT_TTL_SECS, DEFAULT_JOIN_CODE_TTL_SECS},
    revocation::{RevocationEntry, RevocationReason, RevocationSet},
    types::{KemAlgo, MinCryptoLevel, SigAlgo, DEVICE_CERT_LEN, JOIN_INVITE_LEN},
    DeviceRole, Error, JoinCode,
};

const TG_SEED: [u8; 32] = [0x11; 32];
const DEV_SEED: [u8; 32] = [0x33; 32];
const DEV2_SEED: [u8; 32] = [0x44; 32];

fn trust_group_key() -> SigningKey {
    SigningKey::from_bytes(&TG_SEED)
}

fn device_key() -> SigningKey {
    SigningKey::from_bytes(&DEV_SEED)
}

fn other_device_key() -> SigningKey {
    SigningKey::from_bytes(&DEV2_SEED)
}

fn generate_signing_key(rng: &mut impl rand_core::RngCore) -> SigningKey {
    let mut seed = [0u8; 32];
    rng.fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

#[test]
fn certificate_roundtrip_and_validation() {
    let tg = trust_group_key();
    let device = device_key();
    let now = 1_739_600_000;
    let cert = DeviceCertificate::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        *tg.verifying_key().as_bytes(),
        DeviceRole::Member,
        now,
        now + 86_400,
    );

    assert_eq!(cert.sig_algo, SigAlgo::Classical);

    let encoded = cert.to_bytes();
    assert_eq!(encoded.len(), DEVICE_CERT_LEN);

    let parsed = DeviceCertificate::from_bytes(&encoded).expect("parse cert");
    assert_eq!(parsed.sig_algo, SigAlgo::Classical);

    let revocations = RevocationSet::new();
    parsed
        .verify(&tg.verifying_key(), now + 60, Some(&revocations))
        .expect("valid certificate");
}

#[test]
fn certificate_revocation_blocks_usage() {
    let tg = trust_group_key();
    let device = device_key();
    let now = 1_739_600_000;
    let cert = DeviceCertificate::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        *tg.verifying_key().as_bytes(),
        DeviceRole::Member,
        now,
        now + 86_400,
    );

    let mut set = RevocationSet::new();
    let entry = RevocationEntry::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        now + 1,
        RevocationReason::KeyCompromise,
    );
    set.add(entry);

    let err = cert
        .verify(&tg.verifying_key(), now + 10, Some(&set))
        .expect_err("revoked");
    assert_eq!(err, Error::Revoked);
}

#[test]
fn hkdf_vectors_match_spec() {
    let tg = trust_group_key();
    let keys = derive_group_keys(&tg).expect("hkdf");

    let expected_dek = [
        0xdd, 0xe5, 0x95, 0xc4, 0xf9, 0xd6, 0xad, 0xc7, 0x37, 0x07, 0x78, 0x58, 0x6a, 0xdd, 0xe5,
        0xe1, 0x21, 0xe7, 0xbf, 0xa1, 0x2e, 0xe0, 0x4d, 0x4b, 0x34, 0xa1, 0xa4, 0x73, 0x99, 0x76,
        0x7c, 0x98,
    ];
    let expected_hk = [
        0x20, 0x1d, 0x66, 0xf8, 0xef, 0x16, 0x67, 0xbd, 0xfc, 0x1c, 0x81, 0x8e, 0x62, 0xe4, 0x6b,
        0x09, 0x1d, 0x25, 0x6c, 0x04, 0x96, 0x7e, 0xd6, 0x6b, 0x16, 0x5e, 0x64, 0xc1, 0x2b, 0x52,
        0xa2, 0x19,
    ];

    assert_eq!(keys.dek, expected_dek);
    assert_eq!(keys.hk, expected_hk);
}

#[test]
fn join_response_encrypts_and_decrypts() {
    let tg = trust_group_key();
    let device = other_device_key();
    let keys = derive_group_keys(&tg).expect("hkdf");
    let now = 1_739_600_000;
    let cert = DeviceCertificate::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        *tg.verifying_key().as_bytes(),
        DeviceRole::Member,
        now,
        now + 86_400,
    );
    let bundle = JoinResponseBundle::new(cert, keys.dek, keys.hk, MinCryptoLevel::Classical);

    let mut rng = ChaCha20Rng::from_seed([0u8; 32]);
    let encrypted =
        encrypt_join_response(&mut rng, &tg, &device.verifying_key(), &bundle).expect("encrypt");
    let decrypted =
        decrypt_join_response(&device, &tg.verifying_key(), &encrypted).expect("decrypt");
    assert_eq!(
        decrypted.certificate.to_bytes(),
        bundle.certificate.to_bytes()
    );
    assert_eq!(decrypted.dek, bundle.dek);
    assert_eq!(decrypted.hk, bundle.hk);
}

#[test]
fn join_code_validation_rules() {
    let mut rng = ChaCha20Rng::from_seed([1u8; 32]);
    let expires = 1_739_600_000;
    let mut code = JoinCode::generate(&mut rng, expires);
    let candidate = *code.value();
    code.validate(&candidate, expires - 60).expect("valid");
    code.mark_used();
    let err = code.validate(&candidate, expires - 30).expect_err("used");
    assert_eq!(err, Error::InvalidJoinCode);
}

#[test]
fn group_mgmt_message_roundtrip() {
    let tg = trust_group_key();
    let dev = device_key();
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0u8; 16]);
    let mut msg = GroupMgmtMessage::new(
        GroupMgmtOpCode::JoinRequest,
        *tg.verifying_key().as_bytes(),
        *dev.verifying_key().as_bytes(),
        42,
        1_739_600_100,
        payload,
    );
    assert_eq!(msg.sig_algo, SigAlgo::Classical);
    msg.sign(&dev);
    let bytes = msg.encode().expect("encode");
    let decoded = GroupMgmtMessage::decode(&bytes).expect("decode");
    decoded.verify(&dev.verifying_key()).expect("verify");
    assert_eq!(decoded.sig_algo, SigAlgo::Classical);
    assert_eq!(decoded.opcode, GroupMgmtOpCode::JoinRequest);
    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.payload.len(), 16);
}

// ---------------------------------------------------------------------------
// Algorithm agility tests
// ---------------------------------------------------------------------------

#[test]
fn unknown_sig_algo_rejected_in_cert() {
    let tg = trust_group_key();
    let device = device_key();
    let now = 1_739_600_000;
    let cert = DeviceCertificate::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        *tg.verifying_key().as_bytes(),
        DeviceRole::Member,
        now,
        now + 86_400,
    );
    let mut bytes = cert.to_bytes();
    // Corrupt the sig_algo byte (offset 1) to an unknown value
    bytes[1] = 0xFF;
    let err = DeviceCertificate::from_bytes(&bytes).expect_err("unknown sig_algo");
    assert_eq!(err, Error::UnsupportedAlgorithm(0xFF));
}

#[test]
fn unknown_sig_algo_rejected_in_group_mgmt() {
    let tg = trust_group_key();
    let dev = device_key();
    let mut msg = GroupMgmtMessage::new(
        GroupMgmtOpCode::Ack,
        *tg.verifying_key().as_bytes(),
        *dev.verifying_key().as_bytes(),
        1,
        1_739_600_000,
        Vec::new(),
    );
    msg.sign(&dev);
    let mut bytes = msg.encode().expect("encode");
    // Corrupt sig_algo byte (offset 1)
    bytes[1] = 0xFE;
    let err = GroupMgmtMessage::decode(&bytes).expect_err("unknown sig_algo");
    assert_eq!(err, Error::UnsupportedAlgorithm(0xFE));
}

#[test]
fn unknown_kem_algo_rejected_in_join_request() {
    let payload = JoinRequestPayload::new([0xAA; 16], [0xBB; 32]);
    let mut bytes = payload.encode().to_vec();
    // Corrupt kem_algo byte (offset 0)
    bytes[0] = 0xFD;
    let err = JoinRequestPayload::decode(&bytes).expect_err("unknown kem_algo");
    assert_eq!(err, Error::UnsupportedAlgorithm(0xFD));
}

#[test]
fn cert_v2_roundtrip_with_sig_algo() {
    let tg = trust_group_key();
    let device = device_key();
    let now = 1_739_600_000;
    let cert = DeviceCertificate::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        *tg.verifying_key().as_bytes(),
        DeviceRole::KeyHolder,
        now,
        now + 86_400 * 365,
    );

    // Verify version 0x02 and sig_algo Classical
    assert_eq!(cert.version, 0x02);
    assert_eq!(cert.sig_algo, SigAlgo::Classical);

    // Roundtrip
    let bytes = cert.to_bytes();
    assert_eq!(bytes[0], 0x02); // version
    assert_eq!(bytes[1], 0x01); // sig_algo = Classical

    let parsed = DeviceCertificate::from_bytes(&bytes).expect("parse");
    assert_eq!(parsed.version, cert.version);
    assert_eq!(parsed.sig_algo, cert.sig_algo);
    assert_eq!(parsed.device_public_key, cert.device_public_key);
    assert_eq!(parsed.trust_group_id, cert.trust_group_id);
    assert_eq!(parsed.role, cert.role);
    assert_eq!(parsed.issued_at, cert.issued_at);
    assert_eq!(parsed.expires_at, cert.expires_at);

    // Verify signature
    parsed
        .verify(&tg.verifying_key(), now + 60, None)
        .expect("valid");
}

#[test]
fn revocation_entry_roundtrip_with_sig_algo() {
    let tg = trust_group_key();
    let device = device_key();
    let now = 1_739_600_000;
    let entry = RevocationEntry::issue(
        &tg,
        *device.verifying_key().as_bytes(),
        now,
        RevocationReason::ForcedRemoval,
    );
    assert_eq!(entry.sig_algo, SigAlgo::Classical);

    let bytes = entry.to_bytes();
    let parsed = RevocationEntry::from_bytes(&bytes).expect("parse");
    assert_eq!(parsed.sig_algo, SigAlgo::Classical);
    assert_eq!(parsed.device_public_key, entry.device_public_key);
    assert_eq!(parsed.revoked_at, entry.revoked_at);
    assert_eq!(parsed.reason, entry.reason);
    parsed.verify(&tg.verifying_key()).expect("valid");
}

#[test]
fn join_request_payload_roundtrip_with_kem_algo() {
    let payload = JoinRequestPayload::new([0x42; 16], [0x99; 32]);
    assert_eq!(payload.kem_algo, KemAlgo::Classical);

    let bytes = payload.encode();
    assert_eq!(bytes[0], 0x01); // kem_algo = Classical

    let parsed = JoinRequestPayload::decode(&bytes).expect("parse");
    assert_eq!(parsed.kem_algo, KemAlgo::Classical);
    assert_eq!(parsed.join_code, payload.join_code);
    assert_eq!(parsed.nonce, payload.nonce);
}

// ── JSON conformance vector tests ─────────────────────────────────────────────
// Vectors live in r2-specifications; path is relative to this crate's Cargo.toml.

#[cfg(test)]
const TRUST_VECTORS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../r2-specifications/testing/test-vectors/r2-trust-vectors.json"
));

fn hex_to_bytes(s: &str) -> alloc::vec::Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex_to_32(s: &str) -> [u8; 32] {
    let v = hex_to_bytes(s);
    assert_eq!(v.len(), 32, "expected 32-byte hex, got {}", v.len());
    v.try_into().unwrap()
}

#[test]
fn json_conformance_hkdf_group_keys() {
    let data: serde_json::Value = serde_json::from_str(TRUST_VECTORS_JSON)
        .expect("parse r2-trust-vectors.json");
    let vectors = data["hkdf_group_keys"].as_array().expect("hkdf_group_keys array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let tg_seed = hex_to_32(v["input"]["tg_sk_seed_hex"].as_str().unwrap());
        let tg_key = SigningKey::from_bytes(&tg_seed);
        let keys = crate::derive_group_keys(&tg_key)
            .unwrap_or_else(|e| panic!("TRUST hkdf {id}: derive_group_keys failed: {e:?}"));
        let expected_dek = hex_to_32(v["expected"]["dek_hex"].as_str().unwrap());
        let expected_hk  = hex_to_32(v["expected"]["hk_hex"].as_str().unwrap());
        assert_eq!(keys.dek, expected_dek, "TRUST hkdf {id}: DEK mismatch");
        assert_eq!(keys.hk,  expected_hk,  "TRUST hkdf {id}: HK mismatch");
    }
}

#[test]
fn json_conformance_device_certificates() {
    let data: serde_json::Value = serde_json::from_str(TRUST_VECTORS_JSON)
        .expect("parse r2-trust-vectors.json");
    let vectors = data["device_certificates"].as_array().expect("device_certificates array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let tg_seed  = hex_to_32(v["input"]["tg_sk_seed_hex"].as_str().unwrap());
        let dev_seed = hex_to_32(v["input"]["dev_sk_seed_hex"].as_str().unwrap());
        let tg_key   = SigningKey::from_bytes(&tg_seed);
        let dev_key  = SigningKey::from_bytes(&dev_seed);

        let issued_at  = v["input"]["issued_at"].as_u64().unwrap();
        let expires_at = v["input"]["expires_at"].as_u64().unwrap();

        let cert = crate::cert::DeviceCertificate::issue(
            &tg_key,
            *dev_key.verifying_key().as_bytes(),
            *tg_key.verifying_key().as_bytes(),
            crate::DeviceRole::Member,
            issued_at,
            expires_at,
        );
        let encoded = cert.to_bytes();
        let expected = hex_to_bytes(v["expected"]["encoded_hex"].as_str().unwrap());
        assert_eq!(encoded.as_ref(), expected.as_slice(),
            "TRUST cert {id}: encoded bytes mismatch");

        // Round-trip: parse back and verify
        let parsed = crate::cert::DeviceCertificate::from_bytes(&encoded)
            .unwrap_or_else(|e| panic!("TRUST cert {id}: parse failed: {e:?}"));
        let revocations = crate::revocation::RevocationSet::new();
        parsed.verify(&tg_key.verifying_key(), issued_at + 60, Some(&revocations))
            .unwrap_or_else(|e| panic!("TRUST cert {id}: verify failed: {e:?}"));
    }
}

#[test]
fn json_conformance_join_response_encryption() {
    let data: serde_json::Value = serde_json::from_str(TRUST_VECTORS_JSON)
        .expect("parse r2-trust-vectors.json");
    let vectors = data["join_response_encryption"].as_array().expect("join_response_encryption array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let tg_seed  = hex_to_32(v["input"]["tg_sk_seed_hex"].as_str().unwrap());
        let dev_seed = hex_to_32(v["input"]["dev_sk_seed_hex"].as_str().unwrap());
        let rng_seed = hex_to_32(v["input"]["rng_seed_hex"].as_str().unwrap());
        let tg_key   = SigningKey::from_bytes(&tg_seed);
        let dev_key  = SigningKey::from_bytes(&dev_seed);

        // Rebuild bundle from vector inputs
        let cert_bytes = hex_to_bytes(v["input"]["bundle"]["cert_encoded_hex"].as_str().unwrap());
        let cert = crate::cert::DeviceCertificate::from_bytes(&cert_bytes)
            .unwrap_or_else(|e| panic!("TRUST join {id}: parse cert: {e:?}"));
        let dek = hex_to_32(v["input"]["bundle"]["dek_hex"].as_str().unwrap());
        let hk  = hex_to_32(v["input"]["bundle"]["hk_hex"].as_str().unwrap());
        let bundle = crate::join::JoinResponseBundle::new(cert, dek, hk, crate::types::MinCryptoLevel::Classical);

        // Encrypt with seeded RNG
        let mut rng = ChaCha20Rng::from_seed(rng_seed);
        let encrypted = crate::join::encrypt_join_response(&mut rng, &tg_key, &dev_key.verifying_key(), &bundle)
            .unwrap_or_else(|e| panic!("TRUST join {id}: encrypt failed: {e:?}"));

        let expected_nonce  = hex_to_bytes(v["expected"]["nonce_hex"].as_str().unwrap());
        let expected_cipher = hex_to_bytes(v["expected"]["ciphertext_hex"].as_str().unwrap());
        assert_eq!(encrypted.nonce.as_ref(), expected_nonce.as_slice(),
            "TRUST join {id}: nonce mismatch");
        assert_eq!(encrypted.ciphertext, expected_cipher,
            "TRUST join {id}: ciphertext mismatch");

        // Decrypt and verify round-trip
        let decrypted = crate::join::decrypt_join_response(&dev_key, &tg_key.verifying_key(), &encrypted)
            .unwrap_or_else(|e| panic!("TRUST join {id}: decrypt failed: {e:?}"));
        assert_eq!(decrypted.dek, dek, "TRUST join {id}: DEK round-trip mismatch");
        assert_eq!(decrypted.hk,  hk,  "TRUST join {id}: HK round-trip mismatch");
        assert_eq!(decrypted.min_crypto_level, crate::types::MinCryptoLevel::Classical,
            "TRUST join {id}: min_crypto_level round-trip mismatch");
    }
}

#[test]
fn json_conformance_peering_keys() {
    let data: serde_json::Value = serde_json::from_str(TRUST_VECTORS_JSON)
        .expect("parse r2-trust-vectors.json");
    let vectors = data["peering_keys"].as_array().expect("peering_keys array");

    for v in vectors {
        let id = v["id"].as_str().unwrap_or("?");
        let shared_secret = hex_to_32(v["input"]["shared_secret_hex"].as_str().unwrap());
        let tg_pk_a = hex_to_32(v["input"]["tg_pk_a_hex"].as_str().unwrap());
        let tg_pk_b = hex_to_32(v["input"]["tg_pk_b_hex"].as_str().unwrap());

        let keys = derive_peering_keys(&shared_secret, &tg_pk_a, &tg_pk_b)
            .unwrap_or_else(|e| panic!("TRUST peering {id}: derive failed: {e:?}"));

        let expected_hmac = hex_to_32(v["expected"]["peer_hmac_key_hex"].as_str().unwrap());
        let expected_enc = hex_to_32(v["expected"]["peer_enc_key_hex"].as_str().unwrap());
        assert_eq!(keys.hmac, expected_hmac, "TRUST peering {id}: HMAC key mismatch");
        assert_eq!(keys.enc, expected_enc, "TRUST peering {id}: ENC key mismatch");

        // Verify commutativity: swapping argument order produces the same keys.
        let keys_swapped = derive_peering_keys(&shared_secret, &tg_pk_b, &tg_pk_a)
            .unwrap_or_else(|e| panic!("TRUST peering {id}: derive (swapped) failed: {e:?}"));
        assert_eq!(keys.hmac, keys_swapped.hmac, "TRUST peering {id}: HMAC not commutative");
        assert_eq!(keys.enc, keys_swapped.enc, "TRUST peering {id}: ENC not commutative");
    }
}

// ---------------------------------------------------------------------------
// Lifecycle tests — mirrors Anthill's colony_trust_lifecycle test
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_create_and_join() {
    let mut rng = ChaCha20Rng::from_seed([0x55; 32]);
    let now = 1_739_600_000;

    // 1. Key holder creates trust group.
    let mut tg = TrustGroup::create(&mut rng, now).expect("create TG");
    assert!(tg.is_empty());
    assert_eq!(tg.self_certificate().role, DeviceRole::KeyHolder);

    // 2. Generate a join code.
    let code_value = {
        let code = tg.generate_join_code(&mut rng, now, DEFAULT_JOIN_CODE_TTL_SECS);
        *code.value()
    };
    assert_eq!(tg.active_join_code_count(), 1);

    // 3. Device generates its own keypair and joins.
    let device_key = generate_signing_key(&mut rng);
    let encrypted = tg
        .process_join_request(
            &mut rng,
            now + 10,
            &code_value,
            &device_key.verifying_key(),
            String::from("My Laptop"),
            DEFAULT_CERT_TTL_SECS,
        )
        .expect("join");

    assert!(!tg.is_empty());
    assert_eq!(tg.members().len(), 1);
    assert_eq!(tg.members()[0].name, "My Laptop");

    // 4. Device decrypts the response and becomes a member.
    let member = MemberState::from_join_response(
        device_key,
        &tg.verifying_key(),
        &encrypted,
        now + 10,
    )
    .expect("member state");

    assert_eq!(member.dek(), tg.derived_keys().dek.as_ref());
    assert_eq!(member.hk(), tg.derived_keys().hk.as_ref());
    assert!(member.is_valid(now + 100));
    assert_eq!(member.certificate().role, DeviceRole::Member);
}

#[test]
fn lifecycle_join_code_single_use() {
    let mut rng = ChaCha20Rng::from_seed([0x66; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    let code_value = *tg.generate_join_code(&mut rng, now, 300).value();

    // First use succeeds.
    let dev1 = generate_signing_key(&mut rng);
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code_value,
        &dev1.verifying_key(),
        String::from("Device 1"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("first join");

    // Second use of the same code fails.
    let dev2 = generate_signing_key(&mut rng);
    let err = tg
        .process_join_request(
            &mut rng,
            now + 10,
            &code_value,
            &dev2.verifying_key(),
            String::from("Device 2"),
            DEFAULT_CERT_TTL_SECS,
        )
        .expect_err("code already used");
    assert_eq!(err, Error::InvalidJoinCode);
}

#[test]
fn lifecycle_join_code_expiry() {
    let mut rng = ChaCha20Rng::from_seed([0x77; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    let code_value = *tg.generate_join_code(&mut rng, now, 60).value(); // 60s TTL

    // Try to use it after expiry.
    let dev = generate_signing_key(&mut rng);
    let err = tg
        .process_join_request(
            &mut rng,
            now + 120, // well past expiry
            &code_value,
            &dev.verifying_key(),
            String::from("Late Device"),
            DEFAULT_CERT_TTL_SECS,
        )
        .expect_err("expired");
    assert_eq!(err, Error::InvalidJoinCode);
}

#[test]
fn lifecycle_revoke_device() {
    let mut rng = ChaCha20Rng::from_seed([0x88; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    // Join a device.
    let code_value = *tg.generate_join_code(&mut rng, now, 300).value();
    let dev = generate_signing_key(&mut rng);
    let dpk = *dev.verifying_key().as_bytes();
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code_value,
        &dev.verifying_key(),
        String::from("Revokable Device"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("join");
    assert_eq!(tg.members().len(), 1);

    // Revoke the device.
    let entry = tg
        .revoke_device(now + 100, &dpk, RevocationReason::ForcedRemoval)
        .expect("revoke");
    assert_eq!(entry.reason, RevocationReason::ForcedRemoval);
    assert!(tg.is_empty());
    assert!(tg.revocations().contains(&dpk));

    // Revoking again fails (not a member).
    let err = tg
        .revoke_device(now + 200, &dpk, RevocationReason::KeyCompromise)
        .expect_err("already revoked");
    assert_eq!(err, Error::MemberNotFound);
}

#[test]
fn lifecycle_revoked_device_cannot_rejoin() {
    let mut rng = ChaCha20Rng::from_seed([0x99; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    // Join and revoke.
    let code1 = *tg.generate_join_code(&mut rng, now, 300).value();
    let dev = generate_signing_key(&mut rng);
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code1,
        &dev.verifying_key(),
        String::from("Bad Device"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("join");
    tg.revoke_device(now + 100, dev.verifying_key().as_bytes(), RevocationReason::KeyCompromise)
        .expect("revoke");

    // Try to rejoin with a new code — should be blocked.
    let code2 = *tg.generate_join_code(&mut rng, now + 200, 300).value();
    let err = tg
        .process_join_request(
            &mut rng,
            now + 210,
            &code2,
            &dev.verifying_key(),
            String::from("Bad Device Again"),
            DEFAULT_CERT_TTL_SECS,
        )
        .expect_err("revoked");
    assert_eq!(err, Error::Revoked);
}

#[test]
fn lifecycle_duplicate_member_rejected() {
    let mut rng = ChaCha20Rng::from_seed([0xAA; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    let code1 = *tg.generate_join_code(&mut rng, now, 300).value();
    let dev = generate_signing_key(&mut rng);
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code1,
        &dev.verifying_key(),
        String::from("Device"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("first join");

    // Try to join the same device again.
    let code2 = *tg.generate_join_code(&mut rng, now + 10, 300).value();
    let err = tg
        .process_join_request(
            &mut rng,
            now + 15,
            &code2,
            &dev.verifying_key(),
            String::from("Device Dup"),
            DEFAULT_CERT_TTL_SECS,
        )
        .expect_err("duplicate");
    assert_eq!(err, Error::DuplicateMember);
}

#[test]
fn lifecycle_voluntary_leave() {
    let mut rng = ChaCha20Rng::from_seed([0xBB; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    let code = *tg.generate_join_code(&mut rng, now, 300).value();
    let dev = generate_signing_key(&mut rng);
    let dpk = *dev.verifying_key().as_bytes();
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code,
        &dev.verifying_key(),
        String::from("Leaving Device"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("join");

    let entry = tg.process_leave(now + 1000, &dpk).expect("leave");
    assert_eq!(entry.reason, RevocationReason::VoluntaryLeave);
    assert!(tg.is_empty());
    assert!(tg.revocations().contains(&dpk));
}

#[test]
fn lifecycle_find_member() {
    let mut rng = ChaCha20Rng::from_seed([0xCC; 32]);
    let now = 1_739_600_000;
    let mut tg = TrustGroup::create(&mut rng, now).expect("create");

    let code = *tg.generate_join_code(&mut rng, now, 300).value();
    let dev = generate_signing_key(&mut rng);
    let dpk = *dev.verifying_key().as_bytes();
    tg.process_join_request(
        &mut rng,
        now + 5,
        &code,
        &dev.verifying_key(),
        String::from("Findable"),
        DEFAULT_CERT_TTL_SECS,
    )
    .expect("join");

    let found = tg.find_member(&dpk).expect("find member");
    assert_eq!(found.name, "Findable");

    let missing = [0xFFu8; 32];
    assert!(tg.find_member(&missing).is_none());
}

#[test]
fn join_invite_roundtrip_and_signature() {
    let issuer = device_key();
    let issuer_pk_bytes = *issuer.verifying_key().as_bytes();
    let invite_code = [0xAB; 16];
    let trust_group_id = [0xCD; 32];
    let created_at = 1_700_000_000u64;
    let expires_at = created_at + 900;

    let invite = JoinInvite::new_signed(
        invite_code,
        trust_group_id,
        &issuer,
        created_at,
        expires_at,
        1,
    );

    // Wire shape matches the spec.
    let bytes = invite.to_bytes();
    assert_eq!(bytes.len(), JOIN_INVITE_LEN);
    assert_eq!(bytes.len(), 161);
    assert_eq!(&bytes[0..16], &invite_code);
    assert_eq!(&bytes[16..48], &trust_group_id);
    assert_eq!(&bytes[48..80], &issuer_pk_bytes);

    // Round-trip preserves all fields.
    let decoded = JoinInvite::from_bytes(&bytes).expect("decode");
    assert_eq!(decoded, invite);

    // Signature verifies before expiry.
    assert!(decoded.verify(created_at + 1).is_ok());

    // Expiry rejects.
    assert!(matches!(
        decoded.verify(expires_at + 1),
        Err(Error::JoinCodeExpired)
    ));

    // Tampered invite fails signature verification.
    let mut tampered_bytes = bytes;
    tampered_bytes[0] ^= 0x01; // flip a bit in invite_code
    let tampered = JoinInvite::from_bytes(&tampered_bytes).expect("decode tampered");
    assert!(matches!(
        tampered.verify(created_at + 1),
        Err(Error::Signature)
    ));

    // max_uses == 0 is invalid even with a fresh signature.
    let zero_uses = JoinInvite::new_signed(
        invite_code,
        trust_group_id,
        &issuer,
        created_at,
        expires_at,
        0,
    );
    assert!(matches!(
        zero_uses.verify(created_at + 1),
        Err(Error::InvalidJoinCode)
    ));

    // Truncated input rejected.
    assert!(matches!(
        JoinInvite::from_bytes(&bytes[..160]),
        Err(Error::PayloadTooShort)
    ));
}

#[test]
fn join_invite_opcode_round_trips_in_group_mgmt() {
    let issuer = device_key();
    let invite = JoinInvite::new_signed(
        [0xAA; 16],
        [0xBB; 32],
        &issuer,
        1_700_000_000,
        1_700_000_900,
        1,
    );
    let trust_group_id = invite.trust_group_id;
    let issuer_pk_bytes = invite.issuer_pk;

    let mut msg = GroupMgmtMessage::new(
        GroupMgmtOpCode::JoinInvite,
        trust_group_id,
        issuer_pk_bytes,
        0,
        1_700_000_000,
        invite.to_bytes().to_vec(),
    );
    msg.sign(&issuer);
    let encoded = msg.encode().expect("encode group_mgmt");

    let decoded = GroupMgmtMessage::decode(&encoded).expect("decode group_mgmt");
    assert_eq!(decoded.opcode, GroupMgmtOpCode::JoinInvite);
    decoded.verify(&issuer.verifying_key()).expect("verify outer signature");

    // Inner JoinInvite parses cleanly from the GROUP_MGMT payload.
    let inner = JoinInvite::from_bytes(&decoded.payload).expect("decode inner invite");
    assert!(inner.verify(1_700_000_001).is_ok());
}

