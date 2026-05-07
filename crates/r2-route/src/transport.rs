//! Transport types, quality mapping, and transport sets (SPEC.md §4).

use crate::constants::*;

/// Transport families understood by R2-ROUTE (SPEC.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Transport {
    /// Bluetooth Low Energy.
    Ble = 0,
    /// WiFi (TCP/UDP).
    Wifi = 1,
    /// LoRa radio.
    Lora = 2,
    /// Internet (any IP bearer).
    Internet = 3,
}

impl Transport {
    /// Number of transport types.
    pub const COUNT: usize = 4;

    /// All transport variants.
    pub const fn all() -> [Transport; 4] {
        [
            Transport::Ble,
            Transport::Wifi,
            Transport::Lora,
            Transport::Internet,
        ]
    }

    /// Array index for this transport (0–3).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Bitmask for this transport in a [`TransportSet`].
    pub const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    /// Maximum payload size in bytes (SPEC.md §4).
    pub const fn max_payload(self) -> usize {
        match self {
            Transport::Ble => BLE_MAX_PAYLOAD,
            Transport::Wifi => WIFI_MAX_PAYLOAD,
            Transport::Lora => LORA_MAX_PAYLOAD,
            Transport::Internet => INTERNET_MAX_PAYLOAD,
        }
    }

    /// Relative power cost (SPEC.md §4).
    pub const fn power_cost(self) -> f32 {
        match self {
            Transport::Ble => 1.0,
            Transport::Wifi => 10.0,
            Transport::Lora => 5.0,
            Transport::Internet => 8.0,
        }
    }

    /// Jitter range `(min_ms, max_ms)` for relay collision avoidance (SPEC.md §4).
    pub fn jitter_range(self, congested: bool) -> (u32, u32) {
        match (self, congested) {
            (Transport::Ble, false) => BLE_JITTER,
            (Transport::Ble, true) => BLE_JITTER_CONGESTED,
            (Transport::Wifi, false) => WIFI_JITTER,
            (Transport::Wifi, true) => WIFI_JITTER_CONGESTED,
            (Transport::Lora, false) => LORA_JITTER,
            (Transport::Lora, true) => LORA_JITTER_CONGESTED,
            (Transport::Internet, false) => INTERNET_JITTER,
            (Transport::Internet, true) => INTERNET_JITTER_CONGESTED,
        }
    }
}

/// Bitset of transports supported by a neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportSet(u8);

impl TransportSet {
    /// Empty set (no transports).
    pub const fn empty() -> Self {
        TransportSet(0)
    }

    /// Construct from raw bits.
    pub const fn from_bits(bits: u8) -> Self {
        TransportSet(bits)
    }

    /// Raw bit representation.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Add a transport to the set.
    pub fn insert(&mut self, transport: Transport) {
        self.0 |= transport.bit();
    }

    /// Check if a transport is in the set.
    pub fn contains(&self, transport: Transport) -> bool {
        self.0 & transport.bit() != 0
    }

    /// Returns `true` if no transports are in the set.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

/// Quality sample from an observation (SPEC.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QualitySample {
    /// RSSI measurement in dBm.
    Rssi(i8),
    /// Signal-to-noise ratio.
    Snr(f32),
    /// Direct quality value ∈ [0, 1].
    Direct(f32),
    /// Ideal quality (1.0) — used for local/loopback.
    Ideal,
}

impl QualitySample {
    /// Convert to a quality value ∈ [0, 1] (SPEC.md §4.1).
    pub fn to_quality(self) -> f32 {
        match self {
            QualitySample::Rssi(rssi) => quality_from_rssi(rssi),
            QualitySample::Snr(snr) => quality_from_snr(snr),
            QualitySample::Direct(v) => clamp01(v),
            QualitySample::Ideal => 1.0,
        }
    }
}

/// Clamp a value to [0, 1].
#[inline]
pub fn clamp01(v: f32) -> f32 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// Map RSSI (dBm) to quality ∈ [0, 1] (SPEC.md §4.1).
///
/// Linear: -50 dBm → 1.0, -80 dBm → 0.0, clamped outside.
#[inline]
pub fn quality_from_rssi(rssi: i8) -> f32 {
    if rssi >= -50 {
        1.0
    } else if rssi <= -80 {
        0.0
    } else {
        ((rssi as f32) + 80.0) / 30.0
    }
}

/// Map SNR to quality ∈ [0, 1] (SPEC.md §4.1).
///
/// Linear: 10 dB → 1.0, -5 dB → 0.0, clamped outside.
#[inline]
pub fn quality_from_snr(snr: f32) -> f32 {
    if snr >= 10.0 {
        1.0
    } else if snr <= -5.0 {
        0.0
    } else {
        (snr + 5.0) / 15.0
    }
}
