# Contributing

## Dev Environment

**Required:**
- Rust stable toolchain (`rustup toolchain install stable`)
- macOS system dependencies:

```sh
brew install cmake fftw glfw libusb airspy airspyhf hackrf rtl-sdr portaudio codec2 zstd
```

- SDRplay API (optional — only needed to run on real hardware):
  Install from [sdrplay.com/api](https://www.sdrplay.com/api/) (proprietary, not in any package manager).

**Verify the setup:**
```sh
cd rust
# CI build (no SDRplay hardware required):
cargo build --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys
cargo test  --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys
```

Note: `sdrapp-sdrplay` and `sdrapp-sdrplay-sys` require the proprietary SDRplay API headers and are excluded from CI. All other crates compile and test cleanly without hardware.

## Crate Map

All crates live under `rust/crates/`. `sdrapp-core` is the only shared dependency — no hardware, no UI.

| Crate | Role |
|---|---|
| `sdrapp-core` | DSP blocks, signal path engine, shared state, config. Fully unit-testable. |
| `sdrapp-sdrplay-sys` | bindgen FFI to the SDRplay C API — **all `unsafe` code lives here** |
| `sdrapp-sdrplay` | Safe RSPdx-R2 source wrapper (hot-plug reconnect, hardware commands) |
| `sdrapp-rtlsdr` | RTL-SDR source wrapper |
| `sdrapp-audio` | cpal CoreAudio sink for demodulated audio output |
| `sdrapp-recorder` | hound WAV recorder + raw IQ capture; errors surfaced via `SharedState.recorder_error` |
| `sdrapp-midi` | midir nanoKontrol2 MIDI controller, 3-page CYCLE mapping, MIDI Learn |
| `sdrapp-adsb` | ADS-B Mode S decoder + aircraft state store (no UI, no hardware dependency) |
| `sdrapp-ui` | egui/eframe spectrum, waterfall, controls; reads `SharedState` each frame |

For deeper design context see [`rust/ARCHITECTURE.md`](rust/ARCHITECTURE.md).

## Key Design Patterns

**Command flow**: UI never touches hardware directly. Sends `SignalPathCommand` (crossbeam channel)
→ signal path task updates `SharedState` + forwards `HardwareCommand` to device thread.

**SharedState**: `parking_lot::RwLock<SharedState>` shared between signal path (writer) and UI
(reader). UI holds the read guard for one frame only; no reads across await points.

**Hot-plug**: When hardware reconnects, `ReconnectSource { iq_rx, hardware_cmd_tx }` command
swaps the IQ broadcast receiver AND hardware command channel, then re-applies all current
`SharedState.hardware` fields to the new device.

**MIDI Learn**: `SharedState.midi_learn_target = Some(knob_id)` → MIDI controller intercepts
next CC → writes `midi_cc_to_knob` → UI frame-rate sync check persists to `AppConfig.midi_learn`.

**Config evolution**: All new `AppConfig` / `UiConfig` fields must have `#[serde(default)]` so
existing config files deserialise without error. New fields with non-trivial defaults use a
`fn default_field_name() -> T` function named for the field (required by serde's default attr).

## Coding Conventions

**Unsafe code:** `#![forbid(unsafe_code)]` is set on every crate except `sdrapp-sdrplay-sys`.
Keep all FFI boundary code in that one crate.

**Error handling:** No `unwrap()` in library code. `?` with typed errors. `unwrap()` is acceptable
only in tests and `main`. Recoverable errors should surface to the UI via `SharedState` fields
(e.g. `recorder_error: Option<String>`).

**Tests:** `#[cfg(test)] mod tests` block at the bottom of each file. All tests must pass without
any hardware connected. Use `tokio::test` + synthetic IQ sources for signal path integration tests.

**Formatting:** Enforced by `rustfmt`. Run `cargo fmt` before committing.

## Running Checks

All of these must be clean before submitting a PR:

```sh
# Run from rust/
cargo test   --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys
cargo clippy --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys -- -D warnings
cargo fmt --check
```

## PR Process

1. Branch from `master` with a short descriptive name (`feat/midi-page-cycle`, `fix/waterfall-flicker`).
2. Keep commits focused — one logical change per commit.
3. All CI checks must pass (test, clippy, fmt).
4. Open a PR against `master` with a description of what changed and why.
5. Hardware-specific changes (SDRplay, MIDI) should note whether they were tested on real hardware.
