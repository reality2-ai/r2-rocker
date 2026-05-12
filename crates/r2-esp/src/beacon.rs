//! R2-BEACON BLE advertise + scan + peer-table on ESP-IDF (NimBLE).
//!
//! Wraps the protocol primitives from `r2_core::beacon` (build/parse,
//! `BeaconFlags`, `compute_rbid`) and the canonical event-name hash from
//! `r2_fnv::r2_hash` with the ESP-IDF NimBLE plumbing required to actually
//! emit and observe legacy R2-BEACON adverts on the BLE radio. Both the
//! DFR1195 dongle (Tier 1 USB-attached peer) and the LilyGo standalone
//! (Tier 1 R2 node) use this same surface; per-board pin maps and entry
//! points stay in `platforms/esp32-s3/`.
//!
//! ## What this module owns
//!
//! * `BLEDevice::take()` — the chip's singleton NimBLE radio.
//! * Advertisement publishing (28-byte R2-BEACON AD wrapped with the
//!   3-byte BLE Flags AD per R2-BEACON §7).
//! * A continuous scan loop (`block_on(async ...)` on a dedicated thread)
//!   that decodes incoming legacy beacons and feeds a peer table.
//! * RBID rotation on a configurable interval (R2-BEACON §6.1). Until a
//!   trust-group session key is available, RBID is freshly random per
//!   rotation; once a TG key lands (M6+) the caller can swap to
//!   `compute_rbid(session_key, epoch)` by passing a different
//!   `RbidStrategy` in `BeaconConfig`.
//! * Echo suppression — the radio hears its own adverts; this module
//!   filters them out using a small history of recent self-RBIDs.
//!
//! ## What this module deliberately does NOT own
//!
//! * L2CAP CoC server and any post-connection bytes — those live in
//!   `r2_esp::l2cap`. The two modules coexist on the same `BLEDevice`.
//! * Trust-group provisioning state machines (R2-PROVISION) — those live
//!   in board-specific code and act on observations surfaced via the
//!   `on_peer_observed` callback.
//! * R2-WIRE frame transport over BLE — beacons are below the trust
//!   boundary and carry only class + RBID + flags. Frame transport runs
//!   over L2CAP after a connection is established.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use esp32_nimble::{BLEDevice, BLEScan};
use esp_idf_svc::hal::task::block_on;
use esp_idf_svc::sys::esp_random;
use log::{info, warn};

use r2_core::beacon::{
    self, build_legacy_beacon, parse_legacy_beacon, BeaconFlags, LegacyBeacon, BEACON_VERSION,
};
use r2_core::fnv::r2_hash;

extern crate alloc;

/// RBID strategy. M0..M5 use `Random` so beacons rotate but carry no TG
/// linkage. From M6, a trust-group key is available and callers swap to
/// `Hmac { session_key, epoch_secs }` per R2-BEACON §6.1.
#[derive(Clone, Copy)]
pub enum RbidStrategy {
    /// Fresh 8 random bytes per rotation. No TG linkage.
    Random,
    /// HMAC-SHA256(session_key, epoch_counter)[..8]. Reserved for M6+.
    Hmac {
        session_key: [u8; 16],
        epoch_secs: u64,
    },
    /// Fixed 8 bytes used for every advert.
    ///
    /// Defeats the privacy goal of RBID rotation, but the BLE advert is
    /// no longer linkable across reboots — required when a peer (e.g.
    /// r2-rocker's bootstrap loop) needs to match the *same* RBID to a
    /// post-reboot UDP presence packet. Suitable for stationary devices
    /// in a private RF environment; do not use for roaming or hostile-
    /// RF scenarios where unlinkability matters.
    Fixed([u8; 8]),
}

/// Beacon configuration handed to [`start`]. All fields are required.
pub struct BeaconConfig {
    /// Reverse-DNS class string per R2-BEACON §4 / R2-CAP convention,
    /// e.g. `"nz.r2.dfr1195"`. Hashed via `r2_fnv::r2_hash` to produce
    /// the 32-bit `class_hash` carried on the wire.
    pub class_string: String,
    /// Nominal advertised TX power in dBm (R2-BEACON §7.3 byte 19).
    /// Receivers compute path loss against this; it does not configure
    /// the radio's actual PA setting.
    pub tx_power_dbm: i8,
    /// `provisioning` flag at start. Callers can update this through
    /// the returned [`BeaconHandle`] when the trust state changes.
    pub provisioning: bool,
    /// `mcu_mode` flag — set true on dual-processor designs when the
    /// SBC half is sleeping. Always false for always-on hosts.
    pub mcu_mode: bool,
    /// Mobile motion flag. Always false for stationary devices.
    pub mobile: bool,
    /// RBID rotation interval. R2-BEACON §6.1 recommends ≥30 s.
    pub rotate_interval: Duration,
    /// Per-cycle BLE scan duration. The scan task alternates between
    /// scanning for this long and yielding to handle rotation.
    pub scan_cycle: Duration,
    /// Peer table capacity. 32 is plenty for dev rigs.
    pub peer_table_size: usize,
    /// RBID strategy; see [`RbidStrategy`].
    pub rbid_strategy: RbidStrategy,
}

impl BeaconConfig {
    /// Sensible defaults: 60 s RBID rotation, 1 s scan cycles, 32-peer
    /// table, random RBIDs, mcu_mode = false, mobile = false. Caller
    /// supplies the class string and provisioning flag.
    pub fn for_class(class_string: impl Into<String>, provisioning: bool) -> Self {
        Self {
            class_string: class_string.into(),
            tx_power_dbm: 0,
            provisioning,
            mcu_mode: false,
            mobile: false,
            rotate_interval: Duration::from_secs(60),
            scan_cycle: Duration::from_secs(1),
            peer_table_size: 32,
            rbid_strategy: RbidStrategy::Random,
        }
    }
}

/// A peer observation snapshot. Surfaced to the caller via the
/// `on_peer_observed` callback registered at [`start`] time, and
/// queryable from the [`BeaconHandle::peers`] snapshot.
#[derive(Debug, Clone, Copy)]
pub struct PeerObservation {
    pub rbid: [u8; 8],
    pub class_hash: u32,
    pub flags: BeaconFlags,
    pub tx_power_dbm: i8,
    pub last_rssi: i32,
    pub sightings: u32,
    pub last_seen: Instant,
}

/// Read-only handle to the running beacon task. Drop has no effect — the
/// task lives for the lifetime of the firmware.
#[derive(Clone)]
pub struct BeaconHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    peers: Arc<Mutex<PeerTable>>,
    class_hash_u32: u32,
}

impl BeaconHandle {
    /// Snapshot of the current peer table.
    pub fn peers(&self) -> Vec<PeerObservation> {
        let g = self.inner.peers.lock().expect("peer table mutex");
        g.snapshot()
    }

    /// 32-bit FNV-1a of the canonicalised class string this beacon
    /// emits. Useful for cross-referencing the wire-level class_hash.
    pub fn class_hash(&self) -> u32 {
        self.inner.class_hash_u32
    }
}

/// Spawn the beacon task and return immediately.
///
/// On success the radio is initialised, the first advert is published,
/// and a dedicated thread runs the scan + rotation loop forever. The
/// `on_peer_observed` callback fires for each *new* RBID surfaced into
/// the peer table (existing RBIDs already in the table are updated
/// silently; only first sightings invoke the callback).
pub fn start(
    config: BeaconConfig,
    on_peer_observed: impl Fn(PeerObservation) + Send + 'static,
) -> Result<BeaconHandle> {
    let class_hash_u32 = r2_hash(&config.class_string)
        .map_err(|e| anyhow!("class_string hash: {e:?}"))?;

    let peers = Arc::new(Mutex::new(PeerTable::new(config.peer_table_size)));
    let inner = Arc::new(HandleInner {
        peers: peers.clone(),
        class_hash_u32,
    });

    let task_peers = peers;
    let task_config = TaskConfig {
        class_hash_u32,
        tx_power_dbm: config.tx_power_dbm,
        provisioning: config.provisioning,
        mcu_mode: config.mcu_mode,
        mobile: config.mobile,
        rotate_interval: config.rotate_interval,
        scan_cycle: config.scan_cycle,
        rbid_strategy: config.rbid_strategy,
    };
    let class_string_for_log = config.class_string.clone();
    let cb: Box<dyn Fn(PeerObservation) + Send + 'static> = Box::new(on_peer_observed);

    // Take the BLEDevice singleton SYNCHRONOUSLY here, before spawning
    // the thread, so NimBLE is fully initialised by the time `start()`
    // returns. Callers that follow `beacon::start` with `l2cap::init`
    // (or any other NimBLE-touching call) depend on this — without it,
    // `ble_l2cap_create_server` can dereference an uninitialised host
    // struct and the firmware crashes with a Guru Meditation
    // LoadProhibited at EXCVADDR≈0xa0. Observed on Seeed XIAO ESP32-S3
    // (see r2-rocker ADR-001), where the race always loses; on the
    // ESP32-S3-DevKitC-1 it happened to win. `BLEDevice::take()` is
    // idempotent — the spawned thread re-takes to get the same
    // `&'static mut` singleton, no double-init.
    {
        let ble_device = BLEDevice::take();
        ble_device.set_preferred_mtu(251).ok();
    }

    std::thread::Builder::new()
        .stack_size(8192)
        .name("beacon".into())
        .spawn(move || {
            let ble_device = BLEDevice::take();
            ble_device.set_preferred_mtu(251).ok();

            let initial_rbid = make_rbid(&task_config.rbid_strategy);
            let initial_beacon = LegacyBeacon {
                version: BEACON_VERSION,
                flags: BeaconFlags {
                    profile: 0,
                    has_bloom: false,
                    provisioning: task_config.provisioning,
                    mcu_mode: task_config.mcu_mode,
                    mobile: task_config.mobile,
                },
                rbid: initial_rbid,
                class_hash: task_config.class_hash_u32.to_be_bytes(),
                tx_power: task_config.tx_power_dbm,
                anti_collision: random_u16(),
            };
            if let Err(e) = set_advertising_payload(ble_device, &initial_beacon) {
                warn!("[BEACON] initial advert publish failed: {e:?}");
                return;
            }
            info!(
                "[BEACON] advertising as {} class=0x{:08x} prov={}",
                class_string_for_log, task_config.class_hash_u32, task_config.provisioning
            );
            run_loop(ble_device, task_config, task_peers, cb, initial_rbid)
        })
        .context("spawn beacon thread")?;

    Ok(BeaconHandle { inner })
}

// ── internals ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct TaskConfig {
    class_hash_u32: u32,
    tx_power_dbm: i8,
    provisioning: bool,
    mcu_mode: bool,
    mobile: bool,
    rotate_interval: Duration,
    scan_cycle: Duration,
    rbid_strategy: RbidStrategy,
}

fn run_loop(
    ble_device: &'static BLEDevice,
    cfg: TaskConfig,
    peers: Arc<Mutex<PeerTable>>,
    on_peer_observed: Box<dyn Fn(PeerObservation) + Send + 'static>,
    initial_rbid: [u8; 8],
) -> ! {
    let mut current_rbid = initial_rbid;
    let mut self_rbid_history: Vec<[u8; 8]> = Vec::with_capacity(8);
    let mut next_rotate = Instant::now() + cfg.rotate_interval;
    let scan_ms = cfg.scan_cycle.as_millis() as i32;

    block_on(async {
        loop {
            let mut ble_scan = BLEScan::new();
            let _ = ble_scan
                .active_scan(true)
                .filter_duplicates(false)
                .interval(100)
                .window(50)
                .start(ble_device, scan_ms, |device, data| -> Option<()> {
                    let rssi = device.rssi() as i32;
                    let Some(mfr) = data.manufacture_data() else {
                        return None;
                    };
                    if mfr.company_identifier != beacon::COMPANY_ID {
                        return None;
                    }
                    let payload = mfr.payload;
                    if payload.is_empty() || payload[0] != beacon::R2_BEACON_MAGIC {
                        return None;
                    }
                    // NimBLE strips the AD-Length, AD-Type, and Company ID
                    // bytes — reconstruct the full 28-byte AD before
                    // handing to parse_legacy_beacon.
                    let mut full_ad = [0u8; 28];
                    full_ad[0] = 0x1B;
                    full_ad[1] = 0xFF;
                    full_ad[2] = (beacon::COMPANY_ID & 0xFF) as u8;
                    full_ad[3] = (beacon::COMPANY_ID >> 8) as u8;
                    let copy_len = payload.len().min(24);
                    full_ad[4..4 + copy_len].copy_from_slice(&payload[..copy_len]);

                    if let Ok(b) = parse_legacy_beacon(&full_ad) {
                        if b.rbid == current_rbid
                            || self_rbid_history.iter().any(|h| *h == b.rbid)
                        {
                            return None; // our own echo
                        }
                        let cls = u32::from_be_bytes(b.class_hash);
                        let was_new = {
                            let mut g = peers.lock().expect("peer table");
                            g.observe(b.rbid, cls, b.flags, b.tx_power, rssi)
                        };
                        if was_new {
                            on_peer_observed(PeerObservation {
                                rbid: b.rbid,
                                class_hash: cls,
                                flags: b.flags,
                                tx_power_dbm: b.tx_power,
                                last_rssi: rssi,
                                sightings: 1,
                                last_seen: Instant::now(),
                            });
                        }
                    }
                    None
                })
                .await;

            if Instant::now() >= next_rotate {
                let new_rbid = make_rbid(&cfg.rbid_strategy);
                if self_rbid_history.len() == self_rbid_history.capacity() {
                    self_rbid_history.remove(0);
                }
                self_rbid_history.push(current_rbid);
                current_rbid = new_rbid;

                let beacon = LegacyBeacon {
                    version: BEACON_VERSION,
                    flags: BeaconFlags {
                        profile: 0,
                        has_bloom: false,
                        provisioning: cfg.provisioning,
                        mcu_mode: cfg.mcu_mode,
                        mobile: cfg.mobile,
                    },
                    rbid: current_rbid,
                    class_hash: cfg.class_hash_u32.to_be_bytes(),
                    tx_power: cfg.tx_power_dbm,
                    anti_collision: random_u16(),
                };
                if let Err(e) = set_advertising_payload(ble_device, &beacon) {
                    warn!("[BEACON] re-publish after rotate failed: {e:?}");
                }
                next_rotate = Instant::now() + cfg.rotate_interval;
            }
        }
    })
}

fn set_advertising_payload(
    ble_device: &'static BLEDevice,
    beacon: &LegacyBeacon,
) -> Result<()> {
    let beacon_ad = build_legacy_beacon(beacon);
    let mut adv_raw: Vec<u8> = Vec::with_capacity(31);
    // BLE Flags AD: general discoverable, BR/EDR not supported.
    adv_raw.push(0x02);
    adv_raw.push(0x01);
    adv_raw.push(0x06);
    adv_raw.extend_from_slice(&beacon_ad);

    let advertising = ble_device.get_advertising();
    {
        let mut adv = advertising.lock();
        let _ = adv.stop();
        adv.set_raw_data(&adv_raw)
            .map_err(|e| anyhow!("BLE set_raw_data: {:?}", e))?;
        adv.start()
            .map_err(|e| anyhow!("BLE adv start: {:?}", e))?;
    }
    Ok(())
}

fn make_rbid(strategy: &RbidStrategy) -> [u8; 8] {
    match strategy {
        RbidStrategy::Random => random_rbid(),
        RbidStrategy::Hmac {
            session_key,
            epoch_secs,
        } => beacon::compute_rbid(session_key, *epoch_secs),
        RbidStrategy::Fixed(rbid) => *rbid,
    }
}

fn random_rbid() -> [u8; 8] {
    let mut out = [0u8; 8];
    let r1 = unsafe { esp_random() }.to_be_bytes();
    let r2 = unsafe { esp_random() }.to_be_bytes();
    out[..4].copy_from_slice(&r1);
    out[4..].copy_from_slice(&r2);
    out
}

fn random_u16() -> u16 {
    (unsafe { esp_random() } & 0xFFFF) as u16
}

// ── Peer table ──────────────────────────────────────────────────────────

struct PeerEntry {
    rbid: [u8; 8],
    class_hash: u32,
    flags: BeaconFlags,
    tx_power_dbm: i8,
    last_rssi: i32,
    last_seen: Instant,
    sightings: u32,
}

struct PeerTable {
    capacity: usize,
    entries: Vec<Option<PeerEntry>>,
}

impl PeerTable {
    fn new(capacity: usize) -> Self {
        let mut entries = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            entries.push(None);
        }
        Self { capacity, entries }
    }

    /// Returns true if this is a new RBID (caller fires `on_peer_observed`).
    fn observe(
        &mut self,
        rbid: [u8; 8],
        class_hash: u32,
        flags: BeaconFlags,
        tx_power_dbm: i8,
        rssi: i32,
    ) -> bool {
        let now = Instant::now();
        for slot in self.entries.iter_mut() {
            if let Some(p) = slot {
                if p.rbid == rbid {
                    p.flags = flags;
                    p.tx_power_dbm = tx_power_dbm;
                    p.last_rssi = rssi;
                    p.last_seen = now;
                    p.sightings = p.sightings.saturating_add(1);
                    return false;
                }
            }
        }
        for slot in self.entries.iter_mut() {
            if slot.is_none() {
                *slot = Some(PeerEntry {
                    rbid,
                    class_hash,
                    flags,
                    tx_power_dbm,
                    last_rssi: rssi,
                    last_seen: now,
                    sightings: 1,
                });
                return true;
            }
        }
        // Full — evict oldest.
        let oldest_idx = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.as_ref().map(|e| (i, e.last_seen)))
            .min_by_key(|(_, t)| *t)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.entries[oldest_idx] = Some(PeerEntry {
            rbid,
            class_hash,
            flags,
            tx_power_dbm,
            last_rssi: rssi,
            last_seen: now,
            sightings: 1,
        });
        true
    }

    fn snapshot(&self) -> Vec<PeerObservation> {
        let mut v = Vec::new();
        for slot in &self.entries {
            if let Some(p) = slot {
                v.push(PeerObservation {
                    rbid: p.rbid,
                    class_hash: p.class_hash,
                    flags: p.flags,
                    tx_power_dbm: p.tx_power_dbm,
                    last_rssi: p.last_rssi,
                    sightings: p.sightings,
                    last_seen: p.last_seen,
                });
            }
        }
        v
    }
}

// Suppress unused-field warning on `capacity`; it documents intent and
// matches the constructor argument symmetrically.
const _: usize = core::mem::size_of::<PeerTable>();
