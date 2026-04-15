#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Antenna port on the RSPdx-R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Antenna {
    #[default]
    A,
    B,
    C,
}

/// IF bandwidth modes supported by the RSPdx-R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IfMode {
    ZeroIf,
    #[default]
    LowIf200kHz,
    LowIf500kHz,
    LowIf1MHz,
    LowIf2MHz,
}

/// Persistent configuration for the RSPdx-R2 source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspdxConfig {
    /// Center frequency in Hz.
    pub frequency_hz: u64,
    /// Sample rate in samples per second.
    pub sample_rate_sps: u32,
    pub antenna: Antenna,
    pub if_mode: IfMode,
    /// LNA gain reduction (0 = max gain).
    pub lna_state: u8,
    /// Enable automatic gain control.
    pub agc_enabled: bool,
    /// IF gain in dBFS (−59 to 0); used when AGC is disabled.
    #[serde(default)]
    pub if_gain_dbfs: i32,
    /// AGC setpoint in dBFS (−60 to 0).
    #[serde(default = "default_agc_setpoint")]
    pub agc_setpoint_dbfs: i32,
    /// Bias-T power supply on the coax connector.
    #[serde(default)]
    pub bias_t_enabled: bool,
    /// High Dynamic Range mode (RSPdx-R2 specific).
    #[serde(default)]
    pub hdr_mode: bool,
    /// AM broadcast notch filter.
    #[serde(default)]
    pub am_notch_enabled: bool,
    /// FM broadcast / DAB notch filter.
    #[serde(default)]
    pub fm_notch_enabled: bool,
}

fn default_agc_setpoint() -> i32 {
    -60
}

impl Default for RspdxConfig {
    fn default() -> Self {
        Self {
            frequency_hz: 100_000_000,  // 100 MHz
            sample_rate_sps: 2_000_000, // 2 Msps
            antenna: Antenna::A,
            if_mode: IfMode::ZeroIf,
            lna_state: 3,
            agc_enabled: true,
            if_gain_dbfs: 0,
            agc_setpoint_dbfs: -60,
            bias_t_enabled: false,
            hdr_mode: false,
            am_notch_enabled: false,
            fm_notch_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_json() {
        let cfg = RspdxConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: RspdxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.frequency_hz, restored.frequency_hz);
        assert_eq!(cfg.antenna, restored.antenna);
        assert_eq!(cfg.agc_enabled, restored.agc_enabled);
    }
}
