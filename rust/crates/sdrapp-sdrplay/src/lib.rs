// Unsafe is intentionally allowed in this crate.
// sdrapp-sdrplay is the sole FFI boundary for the SDRplay C API.
// All other crates in the workspace use #![forbid(unsafe_code)].

//! SDRplay RSPdx-R2 source.
//!
//! Wraps the proprietary sdrplay_api C library.
//! All unsafe code is confined to device.rs (the FFI boundary).

mod config;
mod device;

pub use config::{Antenna, IfMode, RspdxConfig};
pub use device::RspdxSource;
