# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Important Constraints

- **AI-generated pull requests are banned upstream.** This is a fork; code changes should not be submitted to the upstream repo (AlexandreRouma/SDRPlusPlus).
- Use `cp -f`, `mv -f`, `rm -f` for file operations — shell aliases for `-i` (interactive) mode cause hangs.

## Building

### Linux / BSD

Dependencies: `cmake`, `fftw3`, `glfw`, `libvolk`, `zstd`, plus per-module libs.

```sh
mkdir build && cd build
cmake ..                          # add -DOPT_BUILD_<MODULE>=ON/OFF as needed
make -j$(nproc)
```

Create dev root and run:
```sh
sh ./create_root.sh
./build/sdrpp -r root_dev
```

### macOS

Use the provided build script (handles all required flags automatically):

```sh
brew install cmake fftw glfw libusb airspy airspyhf hackrf rtl-sdr portaudio codec2 zstd
# SDRplay API must be installed separately from sdrplay.com/api/
sh build_macos.sh
sh make_macos_bundle.sh ./build ./SDR++.app
```

For development (run without bundling):
```sh
sh build_macos.sh
sh create_root.sh
./build/sdrpp -r root_dev
```

**Critical cmake flags** (`build_macos.sh` sets these automatically):
- `-DUSE_BUNDLE_DEFAULTS=ON` — **required**; without it the .app uses wrong Linux paths and fails to start
- `-DOPT_BUILD_AUDIO_SINK=OFF -DOPT_BUILD_PORTAUDIO_SINK=ON -DOPT_BUILD_NEW_PORTAUDIO_SINK=ON` — macOS uses PortAudio, not RtAudio
- `-DOPT_BUILD_SDRPLAY_SOURCE=ON` — SDRplay is off by default; requires SDRplay API installed system-wide

**If the .app fails to start / modules don't load:** Delete `~/Library/Application Support/sdrpp/config.json` and relaunch. A stale config from a bad build is the most common cause — the app cannot auto-repair wrong path values.

### Windows

Requires vcpkg (`fftw3`, `glfw3`, `zstd`) and PothosSDR installed to `C:/Program Files/PothosSDR`.

```bat
mkdir build && cd build
cmake .. "-DCMAKE_TOOLCHAIN_FILE=<vcpkg>/scripts/buildsystems/vcpkg.cmake" -G "Visual Studio 16 2019"
cmake --build . --config Release
```

## Architecture

SDR++ is a **plugin-based SDR application**. The executable is thin (`src/main.cpp`); almost all functionality lives in shared libraries (`.so`/`.dylib`/`.dll`) loaded at runtime.

### Core (`core/src/`)

The core library (`sdrpp_core`) exposes global singletons that modules interact with:

- `core::configManager` — JSON config persistence
- `core::moduleManager` — load/unload `.so`/`.dylib`/`.dll` plugins
- `core::modComManager` — inter-module RPC
- `sigpath::iqFrontEnd` — IQ sample pipeline from source to VFOs
- `sigpath::vfoManager` — virtual frequency oscillators (one per demodulator instance)
- `sigpath::sourceManager` — registered SDR source backends
- `sigpath::sinkManager` — registered audio/network output backends

DSP primitives live in `core/src/dsp/` as templated header-only blocks. The signal chain is: **source → IQ frontend (resampling, FFT) → VFO (frequency shift + decimation) → decoder module → sink**.

### Module Types

| Directory | Purpose |
|---|---|
| `source_modules/` | SDR hardware drivers (RTL-SDR, HackRF, AirSpy, etc.) |
| `sink_modules/` | Audio/network output (RtAudio, PortAudio, network) |
| `decoder_modules/` | Demodulators/decoders (radio AM/FM/SSB, M17, APRS, etc.) |
| `misc_modules/` | UI features (recorder, frequency manager, rigctl server, etc.) |

### Writing a Module

Every module is a shared library. Use `sdrpp_module.cmake` as the build helper — it links against `sdrpp_core` and installs to `lib/sdrpp/plugins/`. A module must export:

```cpp
MOD_EXPORT ModuleManager::ModuleInfo_t _SDRPP_MOD_INFO { ... };
MOD_EXPORT void _init() { ... }
MOD_EXPORT ModuleManager::Instance* _create_instance(std::string name) { ... }
```

Instances implement the `ModuleManager::Instance` interface (`postInit`, `enable`, `disable`, `isEnabled`).

### GUI

Built on [Dear ImGui](https://github.com/ocornut/imgui) with GLFW backend. `smgui.h` provides SDR++-specific widget wrappers. The main window (`core/src/gui/main_window.cpp`) drives the render loop and module menu rendering.

### Config

Modules use `core::configManager` (JSON via nlohmann/json). Config locations:
- **macOS .app**: `~/Library/Application Support/sdrpp/config.json`
- **Dev build** (`-r root_dev`): `root_dev/config.json`
- **Linux install**: `~/.config/sdrpp/config.json`

The `-r <path>` CLI flag overrides the default root directory (which contains `config.json`).

**Config generation:** On first launch, `defConfig` in `core/src/core.cpp` generates a platform-appropriate config programmatically. There is no seed config file — everything is code. The macOS default sets:
- `modulesDirectory` = `"../Plugins"` (relative to `SDR++.app/Contents/MacOS/` → resolves to `Contents/Plugins/`)
- `resourcesDirectory` = `"../Resources"` (→ `Contents/Resources/`)

**Why relative paths work:** On macOS bundle launch, the app `chdir()`s to the directory containing the binary (`SDR++.app/Contents/MacOS/`), so `../Plugins` always resolves correctly regardless of where the `.app` is located. This is intentional and correct — **do not change these to absolute paths**.

**`USE_BUNDLE_DEFAULTS=ON` is critical:** Without this cmake flag, `modulesDirectory` defaults to a Linux path (`/usr/lib/sdrpp/plugins`), which breaks the bundle silently. `build_macos.sh` sets it automatically; manual cmake invocations must include it explicitly.

**Module loading** is two-stage: (1) directory scan of `modulesDirectory` for all `.dylib` files, then (2) instance creation from `moduleInstances` in config. A module listed in `moduleInstances` but not present in `modulesDirectory` logs an error and is skipped — it does not crash the app. The `modules[]` array in config is for loading extra `.dylib` files at explicit paths (usually empty).

**Stale config symptoms:** If `modulesDirectory` in config.json contains an absolute path from a previous build location, modules won't load. Fix: delete `~/Library/Application Support/sdrpp/config.json` and relaunch to regenerate from `defConfig`.

## Formatting

```sh
./check_clang_format.sh   # check
./run_clang_format.sh     # apply
```
