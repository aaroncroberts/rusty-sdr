#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Persistent configuration for the audio output sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Output device name. None = system default.
    pub device_name: Option<String>,
    pub sample_rate: u32,
    /// Linear volume 0.0–1.0.
    pub volume: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 48_000,
            volume: 0.8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_json() {
        let cfg = AudioConfig {
            device_name: Some("Built-in Output".into()),
            sample_rate: 44_100,
            volume: 0.5,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: AudioConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.device_name.as_deref(), Some("Built-in Output"));
        assert_eq!(restored.sample_rate, 44_100);
    }
}
