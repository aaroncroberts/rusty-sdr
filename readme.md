# SDR App — Rust SDR Receiver

A spectrum analyzer and receiver application written in Rust, targeting the SDRplay RSPdx-R2 hardware with Korg nanoKontrol2 MIDI control. This is the active development branch of a fork of [SDR++](https://github.com/AlexandreRouma/SDRPlusPlus) — the Rust rewrite lives in `rust/`; the C++ original remains at the repo root.

<!-- screenshot here -->

## Hardware Requirements

| Hardware | Notes |
|---|---|
| SDRplay RSPdx-R2 | Primary source. SDRplay API must be installed separately (see Quick Start). |
| RTL-SDR | Alternative source. Supported via `rtlsdr` library. |
| Korg nanoKontrol2 | Optional. Enables MIDI control of frequency, gain, zoom, and more. |
| macOS 12+ | Only supported platform for the Rust rewrite. |

## Features

**Receiver**
- Real-time spectrum display and scrolling waterfall with adjustable floor/ceiling
- Demodulation modes: WBFM (stereo), NFM, AM, USB, LSB, DSB, CW
- NFM settings: configurable bandwidth (2.5–25 kHz), squelch threshold, CTCSS squelch
- Arrow-key frequency stepping with configurable step size
- Auto-reference level tracking (noise floor and signal ceiling EMA)

**Bookmarks**
- Named frequency bookmarks with mode and category
- Per-channel NFM settings stored per bookmark (bandwidth, squelch, CTCSS) — recalling a bookmark fully restores the receiver configuration
- Category filter and frequency-sort
- Inline edit dialog; CSV export/import; configurable bookmarks file path
- Default bookmarks: BBC R4, WMJI 105.7, NOAA Weather KEC93

**Scanner**
- Bookmark scanner: dwells on bookmarks matching a category filter, squelch-gated
- FM range scanner: sweeps a frequency range with configurable step, squelch, dwell, and stereo-only lock option

**ADS-B Flight Tracking**
- One-click `✈ Map` button: auto-tunes 1090 MHz, starts the ADS-B decoder, and opens the aircraft map
- Live Mercator-projected map with aircraft positions, heading vectors, and altitude-coded colors
- Position trails per aircraft (fades over time)
- Aircraft detail side panel (ICAO, callsign, altitude, speed, heading, squawk)
- Aircraft count badge in the panel header when decoder is running
- Configurable home coordinates (saved to config); ⌖ resets map to home; 📍 saves current viewport as home

**MIDI Control (nanoKontrol2)**
- 47 physical controls mapped across 3 pages (CYCLE button switches pages)
- Page 1: Frequency, volume, FFT floor/ceiling, waterfall level, gain knobs
- Page 2: Scanner controls, LNA/IF gain, squelch
- Page 3: Recorder, band presets, display controls
- MIDI Learn: right-click any knob → assign any CC; bindings persisted in config

**Recording**
- WAV recording of demodulated audio
- Raw IQ capture
- Scheduled recording (delay + duration)

**Other**
- RDS decoder (FM station name, radio text)
- rigctl server (network frequency/mode control from external tools)
- Operators Handbook (F1) with inline documentation
- Configurable color theme, waterfall colormap (Thermal, Inferno, Grayscale, Classic), font scale

## Quick Start

```sh
git clone <repo>
cd sdrpp

# Install SDRplay API from https://www.sdrplay.com/api/

brew install cmake fftw glfw libusb airspy airspyhf hackrf rtl-sdr portaudio codec2 zstd

cd rust
cargo build
cargo run
```

The app looks for the SDRplay API at its default install path. If the API is not found, the SDR source will be unavailable but the rest of the UI still loads (RTL-SDR source works without the SDRplay API).

## Configuration

Config is stored as JSON:
- macOS: `~/Library/Application Support/sdrapp/config.json`
- Linux: `~/.config/sdrapp/config.json`

Notable config fields:

| Field | Default | Purpose |
|-------|---------|---------|
| `bookmarks_file` | `null` | Path to an external CSV bookmarks file (overrides embedded bookmarks) |
| `bookmarks_export_path` | `~/bookmarks.csv` | Default path for CSV export |
| `ui.home_lat` / `ui.home_lon` | 41.5 / −81.7 (Cleveland OH) | ADS-B map home position |

## For Developers

- Architecture and crate layout: [`rust/ARCHITECTURE.md`](rust/ARCHITECTURE.md)
- Contribution guidelines, coding conventions, and PR process: [`contributing.md`](contributing.md)

## License

GPL-3.0. See [`LICENSE`](LICENSE).

---

> **Note:** The C++ SDR++ source remains at the repo root. All active development targets the Rust rewrite in `rust/`. Do not submit patches to the upstream SDR++ project from this fork.
