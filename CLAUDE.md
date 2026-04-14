# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository Layout

```
rust/               ← ACTIVE DEVELOPMENT — Rust rewrite (eframe/egui UI, SDRplay, MIDI)
upstream-cpp/       ← ARCHIVED — original C++ SDR++ upstream (reference only, do not modify)
contributing.md     ← contributor guide for the Rust app
readme.md           ← project README
```

**All new development happens in `rust/`.** The `upstream-cpp/` directory is a read-only archive of the AlexandreRouma/SDRPlusPlus codebase kept for reference (e.g. consulting original DSP algorithms or module interfaces). Do not submit changes to it — PRs to the upstream repo are not accepted from this fork.

## Important Constraints

- **AI-generated pull requests are banned upstream.** This is a fork; code changes should not be submitted to the upstream repo (AlexandreRouma/SDRPlusPlus).
- Use `cp -f`, `mv -f`, `rm -f` for file operations — shell aliases for `-i` (interactive) mode cause hangs.

---

## Rust App (rust/)

The active SDR application. See `rust/Makefile` for common tasks:

```sh
make run      # build + run (requires SDRplay API)
make test     # run all tests
make lint     # clippy -D warnings
make fmt      # rustfmt
```

### Crate Map

| Crate | Purpose |
|---|---|
| `sdrapp` (bin) | Entry point — wires all crates together |
| `sdrapp-core` | Signal path, DSP blocks, shared state, config |
| `sdrapp-ui` | eframe/egui UI (spectrum, waterfall, controls) |
| `sdrapp-sdrplay` | SDRplay RSPdx-R2 FFI source |
| `sdrapp-sdrplay-sys` | Raw bindgen bindings to sdrplay_api |
| `sdrapp-audio` | cpal audio sink (CoreAudio on macOS) |
| `sdrapp-recorder` | WAV recorder |
| `sdrapp-midi` | MIDI controller (nanoKontrol2) |

### Building

```sh
brew install cmake fftw glfw libusb airspy airspyhf hackrf rtl-sdr portaudio codec2 zstd
# SDRplay API must be installed separately from sdrplay.com/api/
cd rust && cargo build
```

CI excludes `sdrapp-sdrplay` and `sdrapp-sdrplay-sys` (require proprietary SDRplay API):
```sh
cargo build --workspace --exclude sdrapp-sdrplay --exclude sdrapp-sdrplay-sys
```

---

## C++ Upstream Reference (upstream-cpp/)

The original SDR++ C++ codebase. Kept for reference only.

### Building (C++ — reference only, run from upstream-cpp/)

**Linux / BSD:**
```sh
cd upstream-cpp
mkdir build && cd build
cmake .. && make -j$(nproc)
sh ../create_root.sh && ./sdrpp -r root_dev
```

**macOS:**
```sh
cd upstream-cpp
sh build_macos.sh
sh make_macos_bundle.sh ./build ./SDR++.app
```

Critical cmake flags: `-DUSE_BUNDLE_DEFAULTS=ON` (required for correct .app paths), `-DOPT_BUILD_PORTAUDIO_SINK=ON`, `-DOPT_BUILD_SDRPLAY_SOURCE=ON`.

**If the C++ .app fails to start:** Delete `~/Library/Application Support/sdrpp/config.json` and relaunch.

### C++ Architecture (reference)

Plugin-based: thin executable + `.dylib` modules loaded at runtime.

- `core::configManager` — JSON config (nlohmann/json)
- `core::moduleManager` — load/unload plugins
- `sigpath::iqFrontEnd` — IQ pipeline source → VFOs
- `sigpath::vfoManager` — virtual frequency oscillators

Module directories: `source_modules/`, `sink_modules/`, `decoder_modules/`, `misc_modules/`

Config locations: `~/Library/Application Support/sdrpp/config.json` (macOS bundle), `~/.config/sdrpp/config.json` (Linux), `root_dev/config.json` (dev). Config is programmatically generated on first launch from `defConfig` in `core/src/core.cpp` — no seed file. Module paths are stored as `../Plugins` (relative, resolved via `chdir()` to `Contents/MacOS/` on launch).

## Formatting (C++ — run from upstream-cpp/)

```sh
cd upstream-cpp
./check_clang_format.sh   # check
./run_clang_format.sh     # apply
```

## Formatting (Rust — run from rust/)

```sh
cargo fmt --check   # check
cargo fmt           # apply
```
