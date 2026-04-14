# SDR App — Rust SDR Receiver

A spectrum analyzer and receiver application written in Rust, targeting the SDRplay RSPdx-R2 hardware with Korg nanoKontrol2 MIDI control. This is the active development branch of a fork of [SDR++](https://github.com/AlexandreRouma/SDRPlusPlus) — the Rust rewrite lives in `rust/`; the C++ original remains at the repo root.

<!-- screenshot here -->

## Hardware Requirements

| Hardware | Notes |
|---|---|
| SDRplay RSPdx-R2 | Required. SDRplay API must be installed separately (see Quick Start). |
| Korg nanoKontrol2 | Optional. Enables MIDI control of frequency, gain, and other parameters. |
| macOS 12+ | Only supported platform for the Rust rewrite. |

## Features

- Real-time spectrum display and scrolling waterfall
- MIDI control via nanoKontrol2 (frequency tuning, gain, zoom, and more)
- WAV recording of demodulated audio
- Band presets for quick frequency navigation
- Multiple VFOs for simultaneous monitoring

## Quick Start

```sh
git clone <repo>
cd sdrpp

# Install SDRplay API from https://www.sdrplay.com/api/

brew install cmake fftw glfw libusb  # macOS dependencies

cd rust
cargo build
cargo run
```

The app looks for the SDRplay API at its default install path. If the API is not found, the SDR source will be unavailable but the rest of the UI still loads.

## For Developers

- Architecture and crate layout: [`rust/ARCHITECTURE.md`](rust/ARCHITECTURE.md)
- Contribution guidelines, coding conventions, and PR process: [`CONTRIBUTING.md`](CONTRIBUTING.md)

## License

GPL-3.0. See [`LICENSE`](LICENSE).

---

> **Note:** The C++ SDR++ source remains at the repo root. All active development targets the Rust rewrite in `rust/`. Do not submit patches to the upstream SDR++ project from this fork.
