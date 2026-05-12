//! Device identity — per-device Ed25519 keypair, persisted in NVS.
//!
//! Phase 5a / SPEC-R2-ROCKER-SENSOR §3:
//! On first boot the firmware generates a fresh keypair and stores the
//! 32-byte seed in NVS. On subsequent boots it reloads the same seed,
//! so `device_pk` is stable across reboots and OTA updates (unless a
//! factory reset clears NVS, in which case a new identity is minted).
//!
//! The TG public key is embedded at compile time (Phase 5a uses it for
//! reference / future TG-cert verification; Phase 5b/c will use it to
//! verify wifi_offer + frame HMACs). For now the dashboard uses TOFU
//! against `device_pk` directly.

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, SECRET_KEY_LENGTH};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys::esp_fill_random;
use log::info;

/// TG public key — same bytes the dashboard verifies against. Embedded
/// at compile time per SPEC-R2-ROCKER-SENSOR §3.2.
pub const TG_PUB_KEY: [u8; 32] = *include_bytes!("../../../../trust_keys/tg_pub.bin");

const NVS_NS: &str = "r2-rocker";
const NVS_KEY_DEVICE_PRIV: &str = "device_priv";
const NVS_KEY_RBID: &str = "rbid";

pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Load the device key from NVS, or generate + persist a fresh one.
    pub fn load_or_generate(nvs: EspDefaultNvsPartition) -> Result<Self> {
        let mut store = EspNvs::<NvsDefault>::new(nvs, NVS_NS, true)
            .context("EspNvs::new")?;

        let mut buf = [0u8; SECRET_KEY_LENGTH];
        let signing = match store.get_blob(NVS_KEY_DEVICE_PRIV, &mut buf) {
            Ok(Some(slice)) if slice.len() == SECRET_KEY_LENGTH => {
                let seed: [u8; 32] = slice.try_into()
                    .map_err(|_| anyhow!("device_priv NVS slice not 32 bytes"))?;
                let s = SigningKey::from_bytes(&seed);
                info!(
                    "identity: loaded device key from NVS, pk={}",
                    hex(s.verifying_key().to_bytes().as_slice())
                );
                s
            }
            _ => {
                // First boot — mint a fresh keypair and persist.
                let mut seed = [0u8; SECRET_KEY_LENGTH];
                // SAFETY: esp_fill_random writes `len` bytes into the buffer.
                unsafe {
                    esp_fill_random(seed.as_mut_ptr() as *mut _, seed.len());
                }
                let s = SigningKey::from_bytes(&seed);
                store
                    .set_blob(NVS_KEY_DEVICE_PRIV, &seed)
                    .context("set_blob device_priv")?;
                info!(
                    "identity: generated new device key, pk={}",
                    hex(s.verifying_key().to_bytes().as_slice())
                );
                s
            }
        };

        Ok(Self { signing })
    }

    /// Returns the 32-byte Ed25519 public key — the device's stable identity.
    pub fn device_pk(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Ed25519-sign a message. Used to sign the canonical announce body
    /// per SPEC-R2-ROCKER-WIRE §3.1.
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        let sig: Signature = self.signing.sign(msg);
        sig.to_bytes()
    }
}

/// Load-or-generate a stable 8-byte RBID for R2-BEACON, persisted to NVS.
///
/// Required so that the dashboard's bootstrap loop, which keys its
/// "wait for UDP presence" step on the RBID it observed during BLE
/// scanning, can match the *post-reboot* presence packet we broadcast
/// once WiFi comes up. Without persistence the post-reboot RBID is
/// regenerated and the wait times out at 60 s, leaving the loop stuck
/// at "Waiting for UDP presence" even though the sensor is fine.
///
/// Privacy trade-off (acceptable for the rocker rig): a fixed RBID is
/// linkable across BLE adverts and across reboots — i.e. an observer
/// can tell two adverts come from the same device. For a stationary
/// sensor on a private hotspot in a non-hostile RF environment this is
/// fine; for roaming or adversarial deployments, swap to
/// `RbidStrategy::Hmac` (R2-BEACON §6.1) once a TG session key exists.
pub fn load_or_generate_rbid(nvs: EspDefaultNvsPartition) -> Result<[u8; 8]> {
    let mut store = EspNvs::<NvsDefault>::new(nvs, NVS_NS, true)
        .context("EspNvs::new for rbid")?;
    let mut buf = [0u8; 8];
    if let Ok(Some(slice)) = store.get_blob(NVS_KEY_RBID, &mut buf) {
        if slice.len() == 8 {
            let mut out = [0u8; 8];
            out.copy_from_slice(slice);
            info!("rbid: loaded from NVS = {}", hex(&out));
            return Ok(out);
        }
    }
    let mut rbid = [0u8; 8];
    unsafe { esp_fill_random(rbid.as_mut_ptr() as *mut _, rbid.len()); }
    store.set_blob(NVS_KEY_RBID, &rbid).context("set_blob rbid")?;
    info!("rbid: minted new = {}", hex(&rbid));
    Ok(rbid)
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{:02x}", b);
    }
    out
}
