// sdrplay-sys: raw, unsafe FFI bindings to the SDRplay API.
//
// ALL unsafe code in the sdrapp workspace lives here.
// No other crate may use unsafe — enforced by #![forbid(unsafe_code)] elsewhere.
//
// Do not add logic here. This crate is a thin FFI shell only.
// See sdrapp-sdrplay for the safe Rust wrapper.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
