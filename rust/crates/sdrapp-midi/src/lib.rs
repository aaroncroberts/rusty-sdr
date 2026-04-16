#![forbid(unsafe_code)]

//! MIDI controller support — nanoKontrol2 profile + MIDI learn.
//!
//! Uses `midir` for CoreMIDI access on macOS.
//! Incoming MIDI messages are dispatched via a tokio channel to an async
//! dispatcher that maps them to `MidiAction`s.

mod action;
mod config;
mod controller;
mod nanokontrol2;
pub mod layout;

pub use action::MidiAction;
pub use config::{MidiActionTag, MidiConfig, MidiKey, MidiKeyKind};
pub use controller::MidiController;
pub use layout::{ControlDef, ControlRect, ControlType, ControllerLayout, NanoKontrol2Layout};
