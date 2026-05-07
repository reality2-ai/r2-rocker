//! Forwarding strategy vector (SPEC.md §3.2).

use crate::transport::Transport;

/// Tunable forwarding parameters. Configure per device or trust group.
#[derive(Debug, Clone, Copy)]
pub struct StrategyVector {
    /// Probability of relaying a message (0.0–1.0). 1.0 = always relay.
    pub relay_probability: f32,
    /// BLE transport weight for next-hop selection.
    pub ble_weight: f32,
    /// WiFi transport weight.
    pub wifi_weight: f32,
    /// LoRa transport weight.
    pub lora_weight: f32,
    /// Internet transport weight.
    pub internet_weight: f32,
    /// Maximum copies to spray per message.
    pub replication_budget: u8,
    /// Minimum path confidence to attempt directed routing.
    pub forwarding_threshold: f32,
}

impl StrategyVector {
    /// Get the weight for a specific transport.
    pub fn transport_weight(&self, transport: Transport) -> f32 {
        match transport {
            Transport::Ble => self.ble_weight,
            Transport::Wifi => self.wifi_weight,
            Transport::Lora => self.lora_weight,
            Transport::Internet => self.internet_weight,
        }
    }
}

impl Default for StrategyVector {
    fn default() -> Self {
        StrategyVector {
            relay_probability: 1.0,
            ble_weight: 1.0,
            wifi_weight: 1.0,
            lora_weight: 1.0,
            internet_weight: 1.0,
            replication_budget: 3,
            forwarding_threshold: 0.0,
        }
    }
}
