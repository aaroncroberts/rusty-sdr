#![forbid(unsafe_code)]

//! Safe Rust wrapper for the SDRplay RSPdx-R2.
//!
//! All unsafe FFI calls are delegated to `sdrapp-sdrplay-sys`.
//! This crate exposes only safe Rust types and implements the `Source` trait.

mod config;
mod device;

pub use config::RspdxConfig;
pub use device::RspdxSource;
