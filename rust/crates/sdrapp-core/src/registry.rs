#![forbid(unsafe_code)]

//! Module registry — lists available sources and sinks, tracks the active ones.
//!
//! No dynamic loading (no dlopen). Modules are compiled in and registered at
//! startup. The registry tells the UI what choices are available.

use serde::{Deserialize, Serialize};

/// Which SDR source is currently active.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActiveSource {
    #[default]
    Rspdx,
    // Future: RtlSdr, HackRf, Synthetic, …
}

/// Which audio sink is currently active.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActiveSink {
    #[default]
    Cpal,
    // Future: Network, File, …
}

/// Static module registry — all sources and sinks available in this build.
pub struct ModuleRegistry {
    pub sources: Vec<SourceDescriptor>,
    pub sinks: Vec<SinkDescriptor>,
}

pub struct SourceDescriptor {
    pub id: ActiveSource,
    pub display_name: &'static str,
    pub description: &'static str,
}

pub struct SinkDescriptor {
    pub id: ActiveSink,
    pub display_name: &'static str,
    pub description: &'static str,
}

impl ModuleRegistry {
    /// Build the registry from all compiled-in modules.
    pub fn new() -> Self {
        Self {
            sources: vec![SourceDescriptor {
                id: ActiveSource::Rspdx,
                display_name: "SDRplay RSPdx-R2",
                description: "SDRplay RSPdx-R2 via sdrplay_api",
            }],
            sinks: vec![SinkDescriptor {
                id: ActiveSink::Cpal,
                display_name: "System Audio (cpal)",
                description: "CoreAudio output via cpal",
            }],
        }
    }

    pub fn source_names(&self) -> Vec<&str> {
        self.sources.iter().map(|s| s.display_name).collect()
    }

    pub fn sink_names(&self) -> Vec<&str> {
        self.sinks.iter().map(|s| s.display_name).collect()
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_rspdx_source() {
        let reg = ModuleRegistry::new();
        assert!(reg.sources.iter().any(|s| s.id == ActiveSource::Rspdx));
    }

    #[test]
    fn registry_has_cpal_sink() {
        let reg = ModuleRegistry::new();
        assert!(reg.sinks.iter().any(|s| s.id == ActiveSink::Cpal));
    }

    #[test]
    fn active_source_round_trips_json() {
        let src = ActiveSource::Rspdx;
        let json = serde_json::to_string(&src).unwrap();
        let restored: ActiveSource = serde_json::from_str(&json).unwrap();
        assert_eq!(src, restored);
    }

    #[test]
    fn mock_source_and_sink_register() {
        // Integration test: registry accepts a source and sink entry
        let reg = ModuleRegistry::new();
        assert!(!reg.source_names().is_empty());
        assert!(!reg.sink_names().is_empty());
    }
}
