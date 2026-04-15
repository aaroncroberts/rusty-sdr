#![forbid(unsafe_code)]

use std::io::{BufWriter, Write};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use sdrapp_core::sample::{IqSample, StereoFrame};
pub use sdrapp_core::signal_path::RecordingMode;

use crate::config::RecorderConfig;

/// Commands that control recording state.
#[derive(Debug, Clone)]
pub enum RecorderCommand {
    /// Start recording immediately.
    Start {
        /// Center frequency in Hz — embedded in the filename.
        freq_hz: u64,
        /// IQ sample rate in sps — used for IQ filename metadata.
        iq_sample_rate: u32,
        mode: RecordingMode,
    },
    /// Stop recording immediately.
    Stop,
    /// Arm a future recording.
    ///
    /// The recorder will start automatically at `start_unix_secs` and stop
    /// after `duration_secs`.  Sending a `Stop` before that cancels it.
    Schedule {
        /// Wall-clock start time (seconds since Unix epoch, UTC).
        start_unix_secs: u64,
        /// Recording duration in seconds.
        duration_secs: u32,
        freq_hz: u64,
        iq_sample_rate: u32,
        mode: RecordingMode,
    },
}

/// Records audio and/or raw I/Q to files.
///
/// - Audio → stereo f32 WAV (`sdrapp_{freq_mhz}_{YYYYMMDD_HHMMSS}.wav`)
/// - IQ    → interleaved f32 binary (`sdrapp_{freq_mhz}_{YYYYMMDD_HHMMSS}.iq`)
///
/// Start/stop is controlled via the `RecorderCommand` channel sent from the
/// MIDI controller or UI.  Scheduled recording is also supported.
pub struct Recorder {
    config: RecorderConfig,
    /// Incoming demodulated audio from the signal path.
    audio_rx: Option<mpsc::Receiver<Arc<[StereoFrame]>>>,
    pub audio_tx: mpsc::Sender<Arc<[StereoFrame]>>,
    /// Raw IQ from the SDR source (optional — set via `set_iq_source`).
    iq_source_rx: Option<broadcast::Receiver<Arc<[IqSample]>>>,
    /// Control channel: Start / Stop / Schedule commands.
    cmd_rx: Option<mpsc::Receiver<RecorderCommand>>,
    pub cmd_tx: mpsc::Sender<RecorderCommand>,
}

impl Recorder {
    pub fn new(config: RecorderConfig) -> Self {
        let (audio_tx, audio_rx) = mpsc::channel(64);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        Self {
            config,
            audio_rx: Some(audio_rx),
            audio_tx,
            iq_source_rx: None,
            cmd_rx: Some(cmd_rx),
            cmd_tx,
        }
    }

    /// Attach a raw IQ broadcast receiver so the recorder can write .iq files.
    pub fn set_iq_source(&mut self, rx: broadcast::Receiver<Arc<[IqSample]>>) {
        self.iq_source_rx = Some(rx);
    }

    pub fn start(&mut self) -> JoinHandle<()> {
        let mut audio_rx = self.audio_rx.take().expect("Recorder::start called twice");
        let mut cmd_rx = self.cmd_rx.take().unwrap();
        let mut iq_source_rx = self.iq_source_rx.take();
        let config = self.config.clone();
        let cmd_tx_clone = self.cmd_tx.clone();

        tokio::spawn(async move {
            let mut wav_writer: Option<hound::WavWriter<BufWriter<std::fs::File>>> = None;
            let mut iq_writer: Option<BufWriter<std::fs::File>> = None;
            // When a scheduled stop is armed, we store the deadline.
            let mut stop_at: Option<tokio::time::Instant> = None;

            loop {
                let stop_fut = async {
                    match stop_at {
                        Some(t) => tokio::time::sleep_until(t).await,
                        None => std::future::pending::<()>().await,
                    }
                };

                // IQ channel may be absent; if present, only pull when a writer is active.
                let iq_active = iq_writer.is_some();

                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        handle_command(
                            cmd,
                            &config,
                            &mut wav_writer,
                            &mut iq_writer,
                            &mut stop_at,
                            cmd_tx_clone.clone(),
                        ).await;
                    }

                    Some(frames) = audio_rx.recv() => {
                        if let Some(ref mut w) = wav_writer {
                            for frame in frames.iter() {
                                let _ = w.write_sample(frame.left);
                                let _ = w.write_sample(frame.right);
                            }
                        }
                    }

                    result = async {
                        match iq_source_rx {
                            Some(ref mut rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    }, if iq_active => {
                        match result {
                            Ok(batch) => {
                                if let Some(ref mut w) = iq_writer {
                                    for sample in batch.iter() {
                                        let _ = w.write_all(&sample.re.to_le_bytes());
                                        let _ = w.write_all(&sample.im.to_le_bytes());
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(dropped = n, "IQ recorder lagged, samples dropped");
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                tracing::info!("IQ source closed");
                            }
                        }
                    }

                    _ = stop_fut => {
                        stop_at = None;
                        finalize(&mut wav_writer, &mut iq_writer);
                    }

                    else => break,
                }
            }

            // Finalise any open files on shutdown.
            finalize(&mut wav_writer, &mut iq_writer);
        })
    }
}

async fn handle_command(
    cmd: RecorderCommand,
    config: &RecorderConfig,
    wav_writer: &mut Option<hound::WavWriter<BufWriter<std::fs::File>>>,
    iq_writer: &mut Option<BufWriter<std::fs::File>>,
    stop_at: &mut Option<tokio::time::Instant>,
    cmd_tx: mpsc::Sender<RecorderCommand>,
) {
    match cmd {
        RecorderCommand::Start { freq_hz, iq_sample_rate, mode } => {
            if wav_writer.is_some() || iq_writer.is_some() {
                tracing::warn!("recording already in progress — ignoring Start");
                return;
            }
            let ts = unix_now_secs();
            let ts_str = format_unix_as_datetime(ts);
            let freq_mhz = freq_hz as f64 / 1_000_000.0;
            let stem = format!("sdrapp_{freq_mhz:.3}MHz_{ts_str}");

            if matches!(mode, RecordingMode::AudioOnly | RecordingMode::Both) {
                let path = config.output_dir.join(format!("{stem}.wav"));
                match open_wav_writer(&path, config.sample_rate) {
                    Ok(w) => {
                        tracing::info!(path = %path.display(), "audio recording started");
                        *wav_writer = Some(w);
                    }
                    Err(e) => tracing::error!("failed to open WAV: {e}"),
                }
            }

            if matches!(mode, RecordingMode::IqOnly | RecordingMode::Both) {
                let path = config.output_dir.join(format!("{stem}_{iq_sample_rate}sps.iq"));
                match open_iq_writer(&path) {
                    Ok(w) => {
                        tracing::info!(path = %path.display(), "IQ recording started");
                        *iq_writer = Some(w);
                    }
                    Err(e) => tracing::error!("failed to open IQ file: {e}"),
                }
            }
        }

        RecorderCommand::Stop => {
            *stop_at = None;
            finalize(wav_writer, iq_writer);
        }

        RecorderCommand::Schedule { start_unix_secs, duration_secs, freq_hz, iq_sample_rate, mode } => {
            let now = unix_now_secs();
            let delay_secs = start_unix_secs.saturating_sub(now);

            tracing::info!(
                delay_secs,
                duration_secs,
                freq_hz,
                "recording scheduled"
            );

            // Spawn a task that fires Start after the delay, then Stop after duration.
            let cmd_tx2 = cmd_tx.clone();
            tokio::spawn(async move {
                if delay_secs > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                }
                let _ = cmd_tx2.send(RecorderCommand::Start { freq_hz, iq_sample_rate, mode }).await;
                tokio::time::sleep(std::time::Duration::from_secs(duration_secs as u64)).await;
                let _ = cmd_tx2.send(RecorderCommand::Stop).await;
            });
        }
    }
}

fn finalize(
    wav_writer: &mut Option<hound::WavWriter<BufWriter<std::fs::File>>>,
    iq_writer: &mut Option<BufWriter<std::fs::File>>,
) {
    if let Some(w) = wav_writer.take() {
        if let Err(e) = w.finalize() {
            tracing::error!("failed to finalize WAV: {e}");
        } else {
            tracing::info!("audio recording stopped");
        }
    }
    if let Some(mut w) = iq_writer.take() {
        if let Err(e) = w.flush() {
            tracing::error!("failed to flush IQ file: {e}");
        } else {
            tracing::info!("IQ recording stopped");
        }
    }
}

fn open_wav_writer(
    path: &std::path::Path,
    sample_rate: u32,
) -> Result<hound::WavWriter<BufWriter<std::fs::File>>, hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    hound::WavWriter::create(path, spec)
}

fn open_iq_writer(path: &std::path::Path) -> std::io::Result<BufWriter<std::fs::File>> {
    std::fs::File::create(path).map(BufWriter::new)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp as `YYYYMMDD_HHMMSS` (UTC, no chrono dependency).
fn format_unix_as_datetime(unix_secs: u64) -> String {
    let time_of_day = unix_secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    let total_days = unix_secs / 86400;
    let (y, mo, d) = days_since_epoch_to_ymd(total_days as i64);

    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

/// Gregorian calendar conversion from days-since-1970-01-01.
///
/// Algorithm: Howard Hinnant's public-domain date arithmetic
/// (<https://howardhinnant.github.io/date_algorithms.html>).
fn days_since_epoch_to_ymd(days: i64) -> (u32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month of year [0, 11] (Mar-based)
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u32, mo, d)
}

/// Build a recording start path without writing — used in tests.
#[cfg(test)]
fn recording_stem(freq_hz: u64, unix_secs: u64) -> String {
    let ts_str = format_unix_as_datetime(unix_secs);
    let freq_mhz = freq_hz as f64 / 1_000_000.0;
    format!("sdrapp_{freq_mhz:.3}MHz_{ts_str}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn wav_spec_is_stereo_f32() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
        writer.write_sample(0.5_f32).unwrap();
        writer.write_sample(-0.5_f32).unwrap();
        writer.finalize().unwrap();

        buf.set_position(0);
        let mut reader = hound::WavReader::new(&mut buf).unwrap();
        let s: Vec<f32> = reader.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(s.len(), 2);
        assert!((s[0] - 0.5).abs() < 1e-6);
        assert!((s[1] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn date_formatting_known_epoch() {
        // Unix 0 = 1970-01-01 00:00:00
        assert_eq!(format_unix_as_datetime(0), "19700101_000000");
    }

    #[test]
    fn date_formatting_2026_04_14() {
        // 2026-04-14 15:30:00 UTC
        // Days from epoch to 2026-04-14:
        // Leap years 1970-2025: 14 (1972,76,80,84,88,92,96,2000,04,08,12,16,20,24)
        // 56 years * 365 + 14 leap days = 20440 + 14 = 20454 days to Jan 1, 2026
        // Jan(31)+Feb(28)+Mar(31)+Apr 1-14(14) = 104 days into 2026 (0-indexed: 103)
        // Total days = 20454 + 103 = 20557
        // 15:30:00 = 15*3600 + 30*60 = 55800 secs
        let unix_secs = 20557u64 * 86400 + 55800;
        assert_eq!(format_unix_as_datetime(unix_secs), "20260414_153000");
    }

    #[test]
    fn recording_stem_embeds_freq_and_time() {
        let stem = recording_stem(93_500_000, 0);
        assert!(stem.starts_with("sdrapp_93.500MHz_19700101_000000"), "stem: {stem}");
    }

    #[test]
    fn iq_file_is_interleaved_f32_le() {
        // Write two I/Q samples and verify byte layout.
        let samples: Vec<IqSample> = vec![
            IqSample::new(1.0_f32, -1.0_f32),
            IqSample::new(0.5_f32, 0.25_f32),
        ];
        let mut buf = Vec::new();
        for s in &samples {
            buf.extend_from_slice(&s.re.to_le_bytes());
            buf.extend_from_slice(&s.im.to_le_bytes());
        }
        assert_eq!(buf.len(), 16); // 2 samples × 2 floats × 4 bytes
        let re0 = f32::from_le_bytes(buf[0..4].try_into().unwrap());
        let im0 = f32::from_le_bytes(buf[4..8].try_into().unwrap());
        assert!((re0 - 1.0).abs() < 1e-6);
        assert!((im0 + 1.0).abs() < 1e-6);
    }
}
