//! Tune-action helpers: aircraft ATC, satellite (Orbcomm/NOAA), and NOAA APT.
//!
//! These are called from the embedded view layouts (views.rs) when the embedded
//! panels signal a tune request via their `tune_frequency_hz` field.

use rusty_sdr_core::signal_path::{DemodMode, HardwareCommand, ReceiverCmd, SignalPathCommand};

use super::super::SdrApp;

impl SdrApp {
    /// ATC tune requested by the ADS-B map toolbar.
    ///
    /// When `freq_hz == 1_090_000_000` the user is returning to ADS-B monitoring;
    /// otherwise we switch to AM, antenna C, and unmute so the pilot audio is audible.
    pub(in crate::app) fn handle_aircraft_tune(&mut self, freq_hz: u64) {
        let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(freq_hz).into());
        if freq_hz == 1_090_000_000 {
            // Returning to ADS-B — switch back to ADS-B antenna (B = port 1)
            let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(1).into());
            // Re-mute if ADS-B owns the mute
            if self.adsb_did_mute && !self.muted {
                self.muted = true;
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(true).into());
            }
            self.atc_mode_active = false;
        } else {
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(DemodMode::Am).into());
            // Switch to ML-31 antenna (C = port 2) — optimised for VHF aviation band
            let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(2).into());
            // Unmute so the user can hear ATC audio
            if self.adsb_did_mute && self.muted {
                self.muted = false;
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(false).into());
            }
            self.config.ui.frequency_hz = freq_hz;
            self.frequency_widget = crate::frequency::FrequencyWidget::new(freq_hz);
            self.config_dirty = true;
            self.atc_mode_active = true;
        }
    }

    /// Sat-map tune button: start hardware if idle, then tune to the requested frequency.
    ///
    /// Used for both the Orbcomm "Tune 137.500 MHz" button and any NOAA pass tune buttons
    /// that live on the satellite map itself (not the NOAA APT panel).
    pub(in crate::app) fn handle_sat_tune(&mut self, freq_hz: u64) {
        if !self.shared.read().is_running {
            let _ = self.cmd_tx.try_send(SignalPathCommand::Start);
        }
        self.apply_tune(freq_hz);
    }

    /// NOAA APT tune: WBFM + antenna C (ML-31) + mute speakers + wire audio to decoder.
    /// Saves the current antenna port so `stop_noaa_decode` can restore it.
    pub(in crate::app) fn handle_noaa_tune(&mut self, freq_hz: u64) {
        if !self.shared.read().is_running {
            let _ = self.cmd_tx.try_send(SignalPathCommand::Start);
        }
        // Save current antenna before switching to ML-31 (port C)
        if self.noaa_prev_antenna.is_none() {
            self.noaa_prev_antenna = Some(self.config.source.antenna.clone());
        }
        let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(freq_hz).into());
        let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(DemodMode::Wbfm).into());
        let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(2).into());
        self.config.source.antenna = "C".into();
        self.config_dirty = true;
        // Mute speakers — APT subcarrier audio is not useful to hear
        let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(true).into());
        // Wire audio to the NOAA decoder
        let (noaa_tx, noaa_rx) = crossbeam_channel::bounded(32);
        self.shared.write().noaa_audio_tx = Some(noaa_tx);
        self.noaa_apt.lock().set_audio_rx(noaa_rx);
        self.noaa_apt.lock().reset_decoder();
        self.config.ui.frequency_hz = freq_hz;
        self.frequency_widget = crate::frequency::FrequencyWidget::new(freq_hz);
        self.config_dirty = true;
        self.noaa_apt.lock().is_active = true;
    }

    /// Stop NOAA APT decoding: clear audio tap, restore previous antenna + demod mode.
    pub(in crate::app) fn stop_noaa_decode(&mut self) {
        // Clear audio pipeline
        self.shared.write().noaa_audio_tx = None;
        self.noaa_apt.lock().deactivate();
        // Unmute speakers
        if !self.muted {
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetMuted(false).into());
        }
        // Restore previous antenna port
        if let Some(prev) = self.noaa_prev_antenna.take() {
            let port: u8 = match prev.as_str() { "B" => 1, "C" => 2, _ => 0 };
            self.config.source.antenna = prev;
            self.config_dirty = true;
            let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(port).into());
            tracing::info!(antenna = port, "NOAA stop: restoring previous antenna");
        }
    }
}
