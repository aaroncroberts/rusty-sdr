#![forbid(unsafe_code)]

//! egui/eframe UI layer.
//!
//! Contains the main App struct and custom widgets:
//! - SpectrumWidget: real-time FFT spectrum display
//! - WaterfallWidget: scrolling spectrogram (pixel-buffer based, not egui_plot)
//! - FrequencyWidget: clickable/scrollable frequency display with VFO overlay

pub mod app;
pub mod spectrum;
pub mod waterfall;
pub mod frequency;
