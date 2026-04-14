#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderConfig {
    pub output_dir: PathBuf,
    pub sample_rate: u32,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            output_dir: dirs::audio_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
            sample_rate: 48_000,
        }
    }
}
