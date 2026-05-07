//! R2-BEACON: Device Discovery (BLE Profile)
//!
//! Implements Legacy (31-byte) and Extended beacon formats, RBID computation,
//! and capability bloom filters.
//!
//! Beacons operate below the trust boundary — they carry class, capabilities,
//! and provisioning state, but NO trust group identity or cryptographic material.
//! Trust group recognition happens after connection (R2-BLE + R2-TRUST).

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// R2 Beacon magic byte
pub const R2_BEACON_MAGIC: u8 = 0xB2;
/// Current beacon protocol version
pub const BEACON_VERSION: u8 = 0x01;
/// Company ID for development
pub const COMPANY_ID: u16 = 0xFFFF;

/// Beacon flags (R2-BEACON §7.2)
///
/// Bit layout:
///   7: profile      (0=Legacy, 1=Extended)
///   6: has_bloom     (bloom filter present, Extended only)
///   5: provisioning  (device in provisioning mode)
///   4: mcu_mode      (MCU-only beacon, SBC sleeping)
///   3: mobile        (device in motion)
///   2-0: reserved
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeaconFlags {
    pub profile: u8,        // 0=Legacy, 1=Extended
    pub has_bloom: bool,    // Bloom filter present (Extended only)
    pub provisioning: bool, // Device in provisioning mode
    pub mcu_mode: bool,     // MCU-only beacon
    pub mobile: bool,       // Device in motion
}

impl BeaconFlags {
    pub fn encode(&self) -> u8 {
        (self.profile << 7)
            | ((self.has_bloom as u8) << 6)
            | ((self.provisioning as u8) << 5)
            | ((self.mcu_mode as u8) << 4)
            | ((self.mobile as u8) << 3)
    }

    pub fn decode(byte: u8) -> Self {
        BeaconFlags {
            profile: (byte >> 7) & 1,
            has_bloom: (byte >> 6) & 1 != 0,
            provisioning: (byte >> 5) & 1 != 0,
            mcu_mode: (byte >> 4) & 1 != 0,
            mobile: (byte >> 3) & 1 != 0,
        }
    }
}

/// Legacy beacon data (R2-BEACON §7.3)
///
/// 28-byte AD structure (+ 3 bytes BLE Flags = 31 bytes total PDU)
///
/// Layout: Len(1) + 0xFF(1) + CID(2) + B2(1) + Ver(1) + Flags(1) +
///         RBID(8) + ClassHash(4) + TXPower(1) + AntiCollision(2) + Reserved(6)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBeacon {
    pub version: u8,
    pub flags: BeaconFlags,
    pub rbid: [u8; 8],
    pub class_hash: [u8; 4],
    pub tx_power: i8,
    pub anti_collision: u16,
}

/// Extended beacon data (R2-BEACON §7.4)
///
/// Variable length, up to 254 bytes. Adds bloom filter for capability discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedBeacon {
    pub version: u8,
    pub flags: BeaconFlags,
    pub rbid: [u8; 8],
    pub class_hash: [u8; 4],
    pub tx_power: i8,
    pub anti_collision: u16,
    pub bloom_k: u8,
    pub bloom: Vec<u8>,
    pub seq: u8,
}

/// Beacon errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeaconError {
    TooShort,
    NotR2Beacon,
    InvalidVersion(u8),
    InvalidBloomLen(u8),
}

// ---- RBID Computation ----

/// Compute RBID = HMAC-SHA256(session_key, epoch_counter_be64)[0:8]
///
/// session_key: 16-byte key
/// epoch: counter encoded as big-endian uint64
pub fn compute_rbid(session_key: &[u8; 16], epoch: u64) -> [u8; 8] {
    let mut mac = HmacSha256::new_from_slice(session_key)
        .expect("HMAC key length is always valid");
    mac.update(&epoch.to_be_bytes());
    let result = mac.finalize().into_bytes();
    let mut rbid = [0u8; 8];
    rbid.copy_from_slice(&result[..8]);
    rbid
}

// ---- Legacy Beacon Build/Parse ----

/// Build a legacy beacon AD structure (28 bytes).
///
/// Note: This does NOT include BLE Flags (02 01 06). The caller must
/// prepend BLE Flags for the full 31-byte ADV PDU, or let the BLE
/// stack handle it (see R2-BEACON §7.7.3 for platform API guidance).
pub fn build_legacy_beacon(beacon: &LegacyBeacon) -> [u8; 28] {
    let mut buf = [0u8; 28];
    buf[0] = 0x1B; // AD Length (27 = bytes after this)
    buf[1] = 0xFF; // AD Type: Manufacturer Specific
    buf[2] = (COMPANY_ID & 0xFF) as u8;
    buf[3] = (COMPANY_ID >> 8) as u8;
    buf[4] = R2_BEACON_MAGIC;
    buf[5] = beacon.version;
    buf[6] = beacon.flags.encode();
    buf[7..15].copy_from_slice(&beacon.rbid);
    buf[15..19].copy_from_slice(&beacon.class_hash);
    buf[19] = beacon.tx_power as u8;
    buf[20] = (beacon.anti_collision & 0xFF) as u8;
    buf[21] = (beacon.anti_collision >> 8) as u8;
    // bytes 22-27 are reserved (already zero)
    buf
}

/// Parse a legacy beacon from bytes (must be at least 28 bytes)
pub fn parse_legacy_beacon(data: &[u8]) -> Result<LegacyBeacon, BeaconError> {
    if data.len() < 28 {
        return Err(BeaconError::TooShort);
    }
    if data[4] != R2_BEACON_MAGIC {
        return Err(BeaconError::NotR2Beacon);
    }
    let version = data[5];
    if version != BEACON_VERSION {
        return Err(BeaconError::InvalidVersion(version));
    }
    let flags = BeaconFlags::decode(data[6]);
    let mut rbid = [0u8; 8];
    rbid.copy_from_slice(&data[7..15]);
    let mut class_hash = [0u8; 4];
    class_hash.copy_from_slice(&data[15..19]);
    let tx_power = data[19] as i8;
    let anti_collision = u16::from_le_bytes([data[20], data[21]]);

    Ok(LegacyBeacon {
        version,
        flags,
        rbid,
        class_hash,
        tx_power,
        anti_collision,
    })
}

// ---- Extended Beacon Build/Parse ----

/// Build an extended beacon AD structure (variable length)
pub fn build_extended_beacon(beacon: &ExtendedBeacon) -> Vec<u8> {
    let bloom_len = beacon.bloom.len();
    // AD_Length covers: AD_Type(1) + CID(2) + Magic(1) + Ver(1) + Flags(1)
    //   + RBID(8) + ClassHash(4) + TXPower(1) + AntiCollision(2)
    //   + BloomK(1) + BloomLen(1) + Bloom(N) + Seq(1)
    let after_ad_len = 1 + 2 + 1 + 1 + 1 + 8 + 4 + 1 + 2 + 1 + 1 + bloom_len + 1;
    let total = 1 + after_ad_len;

    let mut buf = Vec::with_capacity(total);
    buf.push(after_ad_len as u8);
    buf.push(0xFF);
    buf.push((COMPANY_ID & 0xFF) as u8);
    buf.push((COMPANY_ID >> 8) as u8);
    buf.push(R2_BEACON_MAGIC);
    buf.push(beacon.version);
    buf.push(beacon.flags.encode());
    buf.extend_from_slice(&beacon.rbid);
    buf.extend_from_slice(&beacon.class_hash);
    buf.push(beacon.tx_power as u8);
    buf.push((beacon.anti_collision & 0xFF) as u8);
    buf.push((beacon.anti_collision >> 8) as u8);
    buf.push(beacon.bloom_k);
    buf.push(bloom_len as u8);
    buf.extend_from_slice(&beacon.bloom);
    buf.push(beacon.seq);

    buf
}

/// Parse an extended beacon from bytes
pub fn parse_extended_beacon(data: &[u8]) -> Result<ExtendedBeacon, BeaconError> {
    if data.len() < 25 {
        return Err(BeaconError::TooShort);
    }
    if data[4] != R2_BEACON_MAGIC {
        return Err(BeaconError::NotR2Beacon);
    }
    let version = data[5];
    if version != BEACON_VERSION {
        return Err(BeaconError::InvalidVersion(version));
    }
    let flags = BeaconFlags::decode(data[6]);
    let mut rbid = [0u8; 8];
    rbid.copy_from_slice(&data[7..15]);
    let mut class_hash = [0u8; 4];
    class_hash.copy_from_slice(&data[15..19]);
    let tx_power = data[19] as i8;
    let anti_collision = u16::from_le_bytes([data[20], data[21]]);
    let bloom_k = data[22];
    let bloom_len = data[23] as usize;
    if bloom_len > 200 {
        return Err(BeaconError::InvalidBloomLen(bloom_len as u8));
    }
    let mut pos = 24;
    if pos + bloom_len > data.len() {
        return Err(BeaconError::TooShort);
    }
    let bloom = data[pos..pos + bloom_len].to_vec();
    pos += bloom_len;

    let seq = if pos < data.len() { data[pos] } else { 0 };

    Ok(ExtendedBeacon {
        version, flags, rbid, class_hash, tx_power, anti_collision,
        bloom_k, bloom, seq,
    })
}

// ---- Bloom Filter ----

/// Set bits in a bloom filter for a given event name.
/// Uses FNV-1a(event_bytes || byte(i)) mod (bloom_len*8) for i in 0..k
pub fn bloom_set(bloom: &mut [u8], event: &str, k: u8) {
    let event_bytes = event.as_bytes();
    let total_bits = bloom.len() * 8;
    if total_bits == 0 { return; }

    for i in 0..k {
        let mut input = Vec::from(event_bytes);
        input.push(i);
        let hash = crate::fnv::fnv1a_32(&input);
        let bit_index = (hash as usize) % total_bits;
        let byte_index = bit_index / 8;
        let bit_in_byte = bit_index % 8;
        bloom[byte_index] |= 1 << bit_in_byte;
    }
}

/// Check if all bloom filter bits are set for a given event
pub fn bloom_check(bloom: &[u8], event: &str, k: u8) -> bool {
    let event_bytes = event.as_bytes();
    let total_bits = bloom.len() * 8;
    if total_bits == 0 { return false; }

    for i in 0..k {
        let mut input = Vec::from(event_bytes);
        input.push(i);
        let hash = crate::fnv::fnv1a_32(&input);
        let bit_index = (hash as usize) % total_bits;
        let byte_index = bit_index / 8;
        let bit_in_byte = bit_index % 8;
        if bloom[byte_index] & (1 << bit_in_byte) == 0 {
            return false;
        }
    }
    true
}

// ---- Scanner identification ----

/// Check if raw BLE advertisement data is an R2 beacon
pub fn is_r2_beacon(data: &[u8]) -> bool {
    data.len() >= 5 && data[1] == 0xFF && data[4] == R2_BEACON_MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex: alloc::string::String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    // ---- RBID computation vectors ----

    #[test]
    fn test_rbid_1() {
        let key: [u8; 16] = [0x01; 16];
        let rbid = compute_rbid(&key, 0);
        assert_eq!(hex::encode(rbid), "3e9ab3976a1745b7",
            "RBID-1: key=01×16, epoch=0");
    }

    #[test]
    fn test_rbid_2() {
        let key: [u8; 16] = [0x01; 16];
        let rbid = compute_rbid(&key, 1);
        assert_eq!(hex::encode(rbid), "f51c0567a44afedc",
            "RBID-2: same key, epoch=1");
    }

    #[test]
    fn test_rbid_3() {
        let key = hex_to_bytes("aabbccddeeff11223344556677889900");
        let mut key_arr = [0u8; 16];
        key_arr.copy_from_slice(&key);
        let rbid = compute_rbid(&key_arr, 0);
        assert_eq!(hex::encode(rbid), "85b2fbead4460457",
            "RBID-3: different session key");
    }

    // ---- Flags byte vectors ----

    #[test]
    fn test_flags_encoding() {
        let vectors: Vec<(BeaconFlags, u8)> = alloc::vec![
            // All off
            (BeaconFlags { profile: 0, has_bloom: false, provisioning: false, mcu_mode: false, mobile: false }, 0x00),
            // Legacy, provisioning mode
            (BeaconFlags { profile: 0, has_bloom: false, provisioning: true, mcu_mode: false, mobile: false }, 0x20),
            // Extended, bloom, mobile
            (BeaconFlags { profile: 1, has_bloom: true, provisioning: false, mcu_mode: false, mobile: true }, 0xC8),
            // Everything on
            (BeaconFlags { profile: 1, has_bloom: true, provisioning: true, mcu_mode: true, mobile: true }, 0xF8),
            // MCU mode only
            (BeaconFlags { profile: 0, has_bloom: false, provisioning: false, mcu_mode: true, mobile: false }, 0x10),
            // Mobile only
            (BeaconFlags { profile: 0, has_bloom: false, provisioning: false, mcu_mode: false, mobile: true }, 0x08),
            // Extended only
            (BeaconFlags { profile: 1, has_bloom: false, provisioning: false, mcu_mode: false, mobile: false }, 0x80),
            // Extended, MCU, mobile
            (BeaconFlags { profile: 1, has_bloom: false, provisioning: false, mcu_mode: true, mobile: true }, 0x98),
        ];
        for (flags, expected) in &vectors {
            assert_eq!(flags.encode(), *expected, "Flags encode mismatch for {:?}", flags);
            let decoded = BeaconFlags::decode(*expected);
            assert_eq!(decoded.profile, flags.profile);
            assert_eq!(decoded.has_bloom, flags.has_bloom);
            assert_eq!(decoded.provisioning, flags.provisioning);
            assert_eq!(decoded.mcu_mode, flags.mcu_mode);
            assert_eq!(decoded.mobile, flags.mobile);
        }
    }

    // ---- Legacy beacon parse/build ----

    #[test]
    fn test_legacy_tv1_stationary_sensor() {
        // R2-BEACON §9.1: Stationary sensor, not provisioning
        let beacon = LegacyBeacon {
            version: BEACON_VERSION,
            flags: BeaconFlags {
                profile: 0, has_bloom: false, provisioning: false,
                mcu_mode: false, mobile: false,
            },
            rbid: [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF],
            class_hash: [0xA3, 0x7F, 0x12, 0xE8],
            tx_power: -59,
            anti_collision: 0x8F1E,
        };
        let built = build_legacy_beacon(&beacon);
        let expected = hex_to_bytes(
            "1B FF FF FF B2 01 00 01 23 45 67 89 AB CD EF A3 7F 12 E8 C5 1E 8F 00 00 00 00 00 00"
        );
        assert_eq!(&built[..], &expected[..], "TV1: stationary sensor");

        let parsed = parse_legacy_beacon(&built).unwrap();
        assert_eq!(parsed, beacon);
    }

    #[test]
    fn test_legacy_tv2_provisioning() {
        // R2-BEACON §9.2: Device in provisioning mode
        let beacon = LegacyBeacon {
            version: BEACON_VERSION,
            flags: BeaconFlags {
                profile: 0, has_bloom: false, provisioning: true,
                mcu_mode: false, mobile: false,
            },
            rbid: [0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10],
            class_hash: [0x5B, 0x91, 0xD4, 0x3A],
            tx_power: -41,
            anti_collision: 0x237A,
        };
        let built = build_legacy_beacon(&beacon);
        let expected = hex_to_bytes(
            "1B FF FF FF B2 01 20 FE DC BA 98 76 54 32 10 5B 91 D4 3A D7 7A 23 00 00 00 00 00 00"
        );
        assert_eq!(&built[..], &expected[..], "TV2: provisioning mode");

        let parsed = parse_legacy_beacon(&built).unwrap();
        assert_eq!(parsed, beacon);
    }

    // ---- Extended beacon ----

    #[test]
    fn test_extended_tv3_mobile_hub() {
        // R2-BEACON §9.3: Mobile hub with bloom filter
        let beacon = ExtendedBeacon {
            version: BEACON_VERSION,
            flags: BeaconFlags {
                profile: 1, has_bloom: true, provisioning: false,
                mcu_mode: false, mobile: true,
            },
            rbid: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22],
            class_hash: [0x22, 0x8C, 0xF1, 0x07],
            tx_power: -50,
            anti_collision: 0x0000,
            bloom_k: 3,
            bloom: hex_to_bytes("8421084210842108"),
            seq: 0x2A,
        };
        let built = build_extended_beacon(&beacon);
        let expected = hex_to_bytes(
            "20 FF FF FF B2 01 C8 AA BB CC DD EE FF 11 22 22 8C F1 07 CE 00 00 03 08 84 21 08 42 10 84 21 08 2A"
        );
        assert_eq!(&built[..], &expected[..], "TV3: mobile hub with bloom");

        let parsed = parse_extended_beacon(&built).unwrap();
        assert_eq!(parsed, beacon);
    }

    #[test]
    fn test_extended_roundtrip() {
        let beacon = ExtendedBeacon {
            version: BEACON_VERSION,
            flags: BeaconFlags {
                profile: 1, has_bloom: true, provisioning: true,
                mcu_mode: false, mobile: false,
            },
            rbid: [0x11; 8],
            class_hash: [0x22; 4],
            tx_power: -40,
            anti_collision: 0xABCD,
            bloom_k: 3,
            bloom: alloc::vec![0xFF; 32],
            seq: 42,
        };
        let built = build_extended_beacon(&beacon);
        let parsed = parse_extended_beacon(&built).unwrap();
        assert_eq!(parsed, beacon);
    }

    // ---- Bloom filter vectors ----

    #[test]
    fn test_bloom_temperature_reading() {
        let mut bloom = [0u8; 8];
        bloom_set(&mut bloom, "temperature.reading", 3);
        let expected = hex_to_bytes("0020000080000004");
        assert_eq!(&bloom[..], &expected[..], "BLOOM-1: temperature.reading");
    }

    #[test]
    fn test_bloom_motion_detected() {
        let mut bloom = [0u8; 8];
        bloom_set(&mut bloom, "motion.detected", 3);
        let expected = hex_to_bytes("0000002010008000");
        assert_eq!(&bloom[..], &expected[..], "BLOOM-2: motion.detected");
    }

    #[test]
    fn test_bloom_combined() {
        let mut bloom = [0u8; 8];
        bloom_set(&mut bloom, "temperature.reading", 3);
        bloom_set(&mut bloom, "motion.detected", 3);
        let expected = hex_to_bytes("0020002090008004");
        assert_eq!(&bloom[..], &expected[..], "BLOOM-3: combined");
    }

    #[test]
    fn test_bloom_check_present() {
        let bloom = hex_to_bytes("0020000080000004");
        assert!(bloom_check(&bloom, "temperature.reading", 3));
    }

    #[test]
    fn test_bloom_check_absent() {
        let bloom = hex_to_bytes("0020000080000004");
        assert!(!bloom_check(&bloom, "motion.detected", 3));
    }

    #[test]
    fn test_bloom_check_combined() {
        let bloom = hex_to_bytes("0020002090008004");
        assert!(bloom_check(&bloom, "temperature.reading", 3));
        assert!(bloom_check(&bloom, "motion.detected", 3));
    }

    // ---- Scanner identification ----

    #[test]
    fn test_is_r2_beacon() {
        let data = hex_to_bytes("1B FF FF FF B2 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        assert!(is_r2_beacon(&data));
    }

    #[test]
    fn test_is_not_r2_beacon() {
        let data = hex_to_bytes("1B FF FF FF AA 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        assert!(!is_r2_beacon(&data));
    }

    // ---- Edge cases ----

    #[test]
    fn test_invalid_version() {
        let data = hex_to_bytes("1B FF FF FF B2 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        let result = parse_legacy_beacon(&data);
        assert_eq!(result, Err(BeaconError::InvalidVersion(0x02)));
    }

    #[test]
    fn test_invalid_magic() {
        let data = hex_to_bytes("1B FF FF FF AA 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        assert_eq!(parse_legacy_beacon(&data), Err(BeaconError::NotR2Beacon));
    }

    #[test]
    fn test_too_short() {
        let data = hex_to_bytes("1B FF FF FF B2");
        assert_eq!(parse_legacy_beacon(&data), Err(BeaconError::TooShort));
    }

    // Helper: we don't have the hex crate in no_std, so we use this for tests
    mod hex {
        use alloc::string::String;
        pub fn encode(bytes: impl AsRef<[u8]>) -> String {
            bytes.as_ref().iter().map(|b| alloc::format!("{:02x}", b)).collect()
        }
    }
}
