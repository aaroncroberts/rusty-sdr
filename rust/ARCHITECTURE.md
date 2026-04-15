# SDR App — Architecture

## Overview

A Rust SDR application built on eframe/egui. The signal path runs as a tokio task; the UI renders at
60 fps reading shared state. No hardware dependency for building or testing the core crates.

---

## Crate Dependency Graph

```
sdrapp (bin)
├── sdrapp-ui          ──► sdrapp-core
├── sdrapp-audio       ──► sdrapp-core
├── sdrapp-midi        ──► sdrapp-core
├── sdrapp-recorder    ──► sdrapp-core
├── sdrapp-sdrplay     ──► sdrapp-sdrplay-sys
│                      ──► sdrapp-core
└── sdrapp-rtlsdr      ──► sdrapp-core

sdrapp-core            (no hardware, no UI — fully unit-testable in CI)
```

---

## Signal Path (`sdrapp-core/src/signal_path/`)

The signal path is split into three submodules:

| File | Contents |
|------|----------|
| `shared_state.rs` | `SharedState` and all sub-structs (`HardwareState`, `DemodState`, `FftDisplayState`, `RdsState`, `ScannerState`). Written by the signal path task, read by the UI each frame. |
| `commands.rs` | All command enums: `ReceiverCmd`, `HardwareCommand`, `DisplayCmd`, `BookmarkCmd`, `ScanCmd`, `SignalPathCommand`. Plus `From` impls for `.into()` at call sites. |
| `mod.rs` | `SignalPath` engine: IQ receive loop, FFT, demodulation, audio emit, scanner tick. Re-exports `shared_state::*` and `commands::*` so all public types stay at `sdrapp_core::signal_path::*`. |

### Data Flow

```
RSPdx-R2 hardware
    │ sdrplay_api callback (OS thread)
    │ crossbeam channel
    ▼
broadcast::Sender<Arc<[IqSample]>>  ──► signal path task (tokio)
                                         │
                                         ├─ FFT → SharedState.fft.fft_magnitudes
                                         │
                                         ├─ Demod → Vec<StereoFrame>
                                         │           │
                                         │           ├─► audio sink (cpal, OS thread)
                                         │           │
                                         │           └─► recorder (tokio task)
                                         │
                                         └─ Scanner tick → SharedState.scanner.*
```

### Channel Types

| Edge | Type | Rationale |
|------|------|-----------|
| Source → signal path | `broadcast::channel` | Multiple subscribers (signal path + recorder) |
| Signal path → audio sink | `crossbeam::channel` (sync) | Audio runs on OS callback thread, not async |
| Signal path → recorder | `tokio::mpsc` | Recorder is an async task |
| UI → signal path | `crossbeam::bounded(64)` | Non-blocking send; UI drops frame on backpressure |
| Signal path → hardware device | `crossbeam::channel` | Hardware device loop is sync (not async) |

### Command Pattern

All UI→signal path communication uses `SignalPathCommand`. Use `.into()` at call sites:

```rust
cmd_tx.try_send(ReceiverCmd::SetFrequency(101_700_000).into()).ok();
cmd_tx.try_send(HardwareCommand::SetLnaState(3).into()).ok();
```

Commands that affect hardware state update `SharedState` first, then forward a `HardwareCommand`
to the device thread. This means `SharedState.hardware` always mirrors the last command sent,
even if the device is disconnected.

### Hot-Plug Reconnect

When hardware reconnects:
```rust
SignalPathCommand::ReconnectSource {
    iq_rx: new_broadcast_receiver,
    hardware_cmd_tx: Some(new_hw_channel),
}
```

The signal path:
1. Swaps the IQ broadcast receiver
2. Swaps the hardware command channel
3. Re-applies all `SharedState.hardware` fields to the new device
4. Rebuilds the demodulator for the current mode
5. Clears IQ and audio accumulators

---

## Shared State Pattern

`Arc<parking_lot::RwLock<SharedState>>` is shared between:
- Signal path task (tokio) — primary writer
- MIDI controller task (tokio) — writes `midi_cc_to_knob`, `midi_device`, `midi_page`
- Audio sink — writes `audio_buffer_fill`
- UI render loop (main thread) — reader

**Lock discipline:**
- UI acquires `read()` for one frame only — never across an await point
- Signal path acquires `write()` for minimal mutations — never while waiting for IQ
- MIDI controller acquires `write()` on CC dispatch — brief, non-blocking

---

## UI Structure (`sdrapp-ui/src/app/panels/`)

| File | Method | Contents |
|------|--------|----------|
| `left.rs` | `left_panel()` | Source status, start/stop, frequency widget, demod mode, NFM settings, scanner |
| `left_bookmarks.rs` | `bookmarks_section()` | Bookmark list, inline edit form, save/export/import CSV |
| `left_device.rs` | `device_settings_section()` | Antenna, sample rate, AGC, LNA/IF knobs, Bias-T, HDR, notch filters, RDS display |
| `center.rs` | `center_panel()` | Spectrum + waterfall, FFT controls, zoom, band plan |
| `right.rs` | `right_panel()` | Volume knob + VU meter, band presets, recorder start/stop/schedule, MIDI status, rigctl config |
| `settings.rs` | `settings_panel()` | FFT size/window, waterfall colormap, font scale, NMF settings |
| `status.rs` | `status_bar()` | Bottom status bar: freq, SNR, sample rate, demod mode, buffer fill |

### UI→Signal Path Pattern

UI never writes hardware state directly. All changes go through `cmd_tx`:

```rust
// Read state for display
let agc = self.config.source.agc_enabled;

// On user action: update config + send command
self.config.source.agc_enabled = !agc;
self.config_dirty = true;
let _ = self.cmd_tx.try_send(HardwareCommand::SetAgcEnabled(!agc).into());
```

`config_dirty = true` triggers `AppConfig::save()` at frame-rate (debounced to 1 Hz).

---

## MIDI Learn

Rendezvous pattern — no direct UI↔MIDI coupling:

1. User right-clicks a knob → context menu → "Assign MIDI CC"
2. UI sets `shared.write().midi_learn_target = Some("knob_id")`
3. KnobWidget renders a pulsing amber ring while `learn_active`
4. MIDI dispatcher (tokio task) checks `midi_learn_target` on every CC event
5. On match: writes `shared.write().midi_cc_to_knob.insert(cc, knob_id)`; clears target
6. UI frame-rate sync: compares `midi_cc_to_knob.len()` vs `config.midi_learn.len()`;
   on change: snapshots bindings into `config.midi_learn` and sets `config_dirty`

Persistence: `AppConfig.midi_learn: HashMap<String, u8>` (knob_id → CC number).
Runtime: `SharedState.midi_cc_to_knob: HashMap<u8, String>` (CC → knob_id, reversed for fast lookup).

---

## Recording

The recorder (`sdrapp-recorder`) is an independent tokio task:

- Receives `RecorderCommand` (Start/Stop/Schedule) via `mpsc`
- Receives `Arc<[StereoFrame]>` audio via `mpsc` from the signal path
- Receives `Arc<[IqSample]>` IQ via `broadcast::Receiver` from the source
- On file open failure: writes to `SharedState.recorder_error` (displayed as error banner in UI)

Recording modes: `AudioOnly` (.wav), `IqOnly` (.iq), `Both`.

---

## Config

`AppConfig` (JSON, `serde`) persists to:
- macOS: `~/Library/Application Support/sdrapp/config.json`
- Linux: `~/.config/sdrapp/config.json`

Unknown JSON fields are silently ignored (`#[serde(deny_unknown_fields)]` is NOT set). All new
fields must have `#[serde(default)]` for backwards compatibility with older config files.

---

## Unsafe Budget

| Crate | unsafe? | Scope |
|-------|---------|-------|
| `sdrapp-sdrplay-sys` | Yes | bindgen output + SDRplay callback marshaling |
| all others | `#![forbid(unsafe_code)]` | Compiler-enforced |

The entire unsafe surface is ~150 lines in one crate.

---

## Testing Strategy

- **Unit tests**: DSP blocks have sync `process(input) -> output` methods tested in `#[test]`
- **Integration tests**: Signal path uses `#[tokio::test]` with synthetic broadcast sources
- **No hardware required**: All tests in CI pass without SDRplay hardware
- **CI exclusions**: `sdrapp-sdrplay` and `sdrapp-sdrplay-sys` require proprietary API headers
