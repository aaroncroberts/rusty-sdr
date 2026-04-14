# Contributing

## Dev Environment

**Required:**
- Rust stable toolchain (`rustup toolchain install stable`)
- SDRplay API installed from [sdrplay.com/api](https://www.sdrplay.com/api/) (proprietary, not in any package manager)
- macOS system dependencies via Homebrew:

```sh
brew install cmake fftw glfw libusb
```

**Verify the setup:**
```sh
cd rust
cargo build        # should compile without errors
cargo test         # must pass without SDRplay hardware connected
```

## Crate Map

All crates live under `rust/crates/`.

| Crate | Role |
|---|---|
| `sdrapp-core` | DSP types, signal path, block traits, shared application state |
| `sdrapp-sdrplay-sys` | bindgen FFI to the SDRplay C API — all `unsafe` code lives here |
| `sdrapp-sdrplay` | Safe RSPdx-R2 source wrapper built on top of `sdrapp-sdrplay-sys` |
| `sdrapp-audio` | cpal CoreAudio sink for demodulated audio output |
| `sdrapp-recorder` | hound-based WAV recorder |
| `sdrapp-midi` | midir nanoKontrol2 MIDI controller and binding system |
| `sdrapp-ui` | egui spectrum, waterfall, and VFO widgets |

For deeper design context see [`rust/ARCHITECTURE.md`](rust/ARCHITECTURE.md).

## Coding Conventions

**Unsafe code:** `#![forbid(unsafe_code)]` is set on every crate except `sdrapp-sdrplay-sys` and `sdrapp-sdrplay`. Keep all FFI boundary code in those two crates.

**Error handling:** No `unwrap()` in library code. Use `?` with `anyhow::Result` or a typed error enum. `unwrap()` is acceptable only in tests and `main`.

**Tests:** Place unit tests in a `#[cfg(test)] mod tests` block at the bottom of each file. All tests must pass without SDRplay hardware connected — mock or stub hardware interactions.

**Formatting:** Enforced by `rustfmt`. Run `cargo fmt` before committing.

## Running Checks

All of these must be clean before submitting a PR:

```sh
cargo test                           # all workspace tests
cargo clippy -- -D warnings          # no warnings allowed
cargo fmt --check                    # formatting enforced
```

## PR Process

1. Create a branch from `master` with a short descriptive name (`feat/midi-page-cycle`, `fix/waterfall-flicker`).
2. Keep commits focused — one logical change per commit.
3. Ensure `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass.
4. Open a pull request against `master` with a description of what changed and why.
5. Hardware-specific changes (SDRplay, MIDI) should note whether they were tested on real hardware.
