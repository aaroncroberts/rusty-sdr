## Summary

<!-- What does this PR do, and why? -->

## Changes

- 
- 

## Test Plan

<!-- How was this tested? Include commands, hardware used, or scenarios exercised. -->

- [ ] 

## Checklist

- [ ] `cargo test --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys` passes
- [ ] `cargo clippy --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys -- -D warnings` is clean
- [ ] `cargo fmt --all` applied (no formatting diff)
- [ ] Hardware-only code paths (sdrplay, audio I/O) are not the sole test coverage for new logic
