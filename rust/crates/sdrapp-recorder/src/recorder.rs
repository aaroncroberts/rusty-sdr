#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use sdrapp_core::sample::StereoFrame;

use crate::config::RecorderConfig;

/// Commands that control recording state.
#[derive(Debug, Clone)]
pub enum RecorderCommand {
    Start,
    Stop,
}

/// Records stereo audio to WAV files via the `hound` crate.
///
/// Receives StereoFrame batches from the audio pipeline and commands
/// from the MIDI controller or UI.
pub struct Recorder {
    config: RecorderConfig,
    /// Incoming audio frames from the signal path.
    audio_rx: Option<mpsc::Receiver<Arc<[StereoFrame]>>>,
    pub audio_tx: mpsc::Sender<Arc<[StereoFrame]>>,
    /// Control channel: Start / Stop commands.
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
            cmd_rx: Some(cmd_rx),
            cmd_tx,
        }
    }

    pub fn start(&mut self) -> JoinHandle<()> {
        let mut audio_rx = self.audio_rx.take().expect("Recorder::start called twice");
        let mut cmd_rx = self.cmd_rx.take().unwrap();
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>> = None;

            loop {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            RecorderCommand::Start => {
                                if writer.is_none() {
                                    let path = next_recording_path(&config.output_dir);
                                    match open_wav_writer(&path, config.sample_rate) {
                                        Ok(w) => {
                                            tracing::info!(path = %path.display(), "recording started");
                                            writer = Some(w);
                                        }
                                        Err(e) => tracing::error!("failed to open WAV: {e}"),
                                    }
                                }
                            }
                            RecorderCommand::Stop => {
                                if let Some(w) = writer.take() {
                                    if let Err(e) = w.finalize() {
                                        tracing::error!("failed to finalize WAV: {e}");
                                    } else {
                                        tracing::info!("recording stopped");
                                    }
                                }
                            }
                        }
                    }
                    Some(frames) = audio_rx.recv() => {
                        if let Some(ref mut w) = writer {
                            for frame in frames.iter() {
                                // hound writes interleaved samples: L, R, L, R, ...
                                let _ = w.write_sample(frame.left);
                                let _ = w.write_sample(frame.right);
                            }
                        }
                    }
                    else => break,
                }
            }
        })
    }
}

fn next_recording_path(dir: &std::path::Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("sdrapp_{ts}.wav"))
}

fn open_wav_writer(
    path: &std::path::Path,
    sample_rate: u32,
) -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>, hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    hound::WavWriter::create(path, spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn wav_spec_is_stereo_f32() {
        // Verify our hound spec matches expected WAV format.
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

        // Read back and verify
        buf.set_position(0);
        let mut reader = hound::WavReader::new(&mut buf).unwrap();
        let s: Vec<f32> = reader.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(s.len(), 2);
        assert!((s[0] - 0.5).abs() < 1e-6);
        assert!((s[1] + 0.5).abs() < 1e-6);
    }
}
