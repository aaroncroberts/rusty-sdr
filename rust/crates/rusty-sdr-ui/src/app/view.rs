//! View system for the single-window layout.
//!
//! # Views
//! The main window shows one of three named views, switchable via the tab bar:
//!
//! * [`ActiveView::Listen`] — Full spectrum + waterfall. The default radio mode.
//! * [`ActiveView::Aircraft`] — ADS-B map (center) + flight data sidebar + mini signal strip.
//! * [`ActiveView::Satellite`] — Satellite map (center) + pass list / NOAA sidebar + mini signal strip.
//!
//! # PanelCtx
//! Every panel's `show()` method receives a [`PanelCtx`] that bundles the three
//! resources all panels need: a lock to the shared signal-path state, the command
//! channel, and the egui context (for `request_repaint`, spawning viewports, etc.).
//!
//! ## Panel convention
//! All embedded panels expose:
//! ```ignore
//! fn show(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx)
//! ```
//! They own their own state, do not depend on [`SdrApp`] fields, and render into
//! whatever `Ui` region they are given.

use crossbeam_channel::Sender;
use parking_lot::RwLock;
use std::sync::Arc;

use rusty_sdr_core::signal_path::{SharedState, SignalPathCommand};

/// Which view is currently displayed in the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ActiveView {
    /// Full-size spectrum + waterfall — the default radio receive mode.
    #[default]
    Listen,
    /// ADS-B aircraft map with flight data sidebar and mini signal strip.
    Aircraft,
    /// Satellite map with pass list / NOAA APT sidebar and mini signal strip.
    Satellite,
}

impl ActiveView {
    pub fn label(self) -> &'static str {
        match self {
            ActiveView::Listen => "📻  Listen",
            ActiveView::Aircraft => "✈  Aircraft",
            ActiveView::Satellite => "🛰  Satellite",
        }
    }
}

/// Shared context passed to every panel's `show()` method.
///
/// Panels borrow from this; it is cheap to create on the stack each frame.
pub struct PanelCtx<'a> {
    /// Live signal-path state (FFT magnitudes, demod mode, hardware state, …).
    pub shared: &'a Arc<RwLock<SharedState>>,
    /// Channel to send commands to the signal path and hardware.
    pub cmd_tx: &'a Sender<SignalPathCommand>,
    /// egui context — use for `request_repaint()`, spawning viewports, etc.
    pub egui_ctx: egui::Context,
}
