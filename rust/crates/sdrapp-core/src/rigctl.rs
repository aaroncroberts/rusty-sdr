#![forbid(unsafe_code)]

//! Hamlib-compatible rigctl TCP server.
//!
//! Listens on a configurable port (default 4532).  External logging software
//! such as WSJT-X, fldigi, and Log4OM connects and issues CAT commands to
//! read/set the active frequency, mode, and signal level.
//!
//! Supported commands (short and extended Hamlib protocol):
//!   f / \get_freq           — return current frequency in Hz
//!   F <hz> / \set_freq <hz> — tune to frequency
//!   m / \get_mode           — return mode + passband
//!   M <mode> <bw> / \set_mode <mode> <bw> — set demod mode
//!   l STRENGTH / \get_level STRENGTH — return signal level (SNR proxy)
//!   q / Q                  — close connection
//!
//! Mode name mapping (Hamlib ↔ DemodMode):
//!   USB ↔ Usb,  LSB ↔ Lsb,  AM ↔ Am,  FM ↔ Nfm,  WFM ↔ Wbfm,  CW ↔ Cw

use parking_lot::RwLock;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

use crate::signal_path::{DemodMode, ReceiverCmd, SharedState, SignalPathCommand};

/// Start the rigctl server and return a join handle.
///
/// The server runs indefinitely and accepts multiple sequential clients.
/// Call `handle.abort()` to stop it.
pub fn start(
    port: u16,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                tracing::info!(port, "rigctl server listening");
                loop {
                    match listener.accept().await {
                        Ok((stream, addr)) => {
                            tracing::debug!(%addr, "rigctl client connected");
                            let shared = Arc::clone(&shared);
                            let tx = cmd_tx.clone();
                            tokio::spawn(async move {
                                handle_client(stream, shared, tx).await;
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "rigctl accept error");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!(port, error = %e, "rigctl server failed to bind");
            }
        }
    })
}

async fn handle_client(
    stream: TcpStream,
    shared: Arc<RwLock<SharedState>>,
    cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = process_command(&line, &shared, &cmd_tx);

        if response == "QUIT" {
            break;
        }

        if writer.write_all(response.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// Process a single rigctl command and return the response string.
fn process_command(
    line: &str,
    shared: &Arc<RwLock<SharedState>>,
    cmd_tx: &crossbeam_channel::Sender<SignalPathCommand>,
) -> String {
    // Normalise: strip leading backslash for extended protocol
    let cmd = line.trim_start_matches('\\');
    let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
    let verb = parts[0];

    match verb {
        // ── Get frequency ─────────────────────────────────────────────────────
        "f" | "get_freq" => {
            let freq = shared.read().center_freq_hz;
            format!("{freq}\nRPRT 0\n")
        }

        // ── Set frequency ─────────────────────────────────────────────────────
        "F" | "set_freq" => {
            if let Some(hz_str) = parts.get(1) {
                if let Ok(hz) = hz_str.trim().parse::<u64>() {
                    let _ = cmd_tx.try_send(ReceiverCmd::SetFrequency(hz).into());
                    return "RPRT 0\n".into();
                }
            }
            "RPRT -1\n".into()
        }

        // ── Get mode ──────────────────────────────────────────────────────────
        "m" | "get_mode" => {
            let mode = shared.read().demod.demod_mode;
            let (mode_str, bw) = mode_to_hamlib(mode);
            format!("{mode_str}\n{bw}\nRPRT 0\n")
        }

        // ── Set mode ──────────────────────────────────────────────────────────
        "M" | "set_mode" => {
            if let Some(mode_str) = parts.get(1) {
                if let Some(mode) = hamlib_to_mode(mode_str.trim()) {
                    let _ = cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
                    return "RPRT 0\n".into();
                }
            }
            "RPRT -1\n".into()
        }

        // ── Get level (S-meter proxy) ─────────────────────────────────────────
        "l" | "get_level" => {
            // STRENGTH is defined as 0.0–1.0 by Hamlib (maps to S0–S9+40).
            // We return SNR normalised to roughly 0.0–1.0 (60 dB = 1.0).
            let snr = shared.read().fft.snr_db.unwrap_or(-10.0);
            let level = (snr / 60.0).clamp(0.0, 1.0);
            format!("{level:.6}\nRPRT 0\n")
        }

        // ── Quit ──────────────────────────────────────────────────────────────
        "q" | "Q" | "quit" => "QUIT".into(),

        // ── Dump state (some apps request this on connect) ────────────────────
        "dump_state" | "1" => {
            // Minimal response that keeps apps happy: just report model ID 1
            "1\n2\n1\n150000.000000 1500000000.000000 0x1ff -1 -1 0x10000003 0x3\n0 0\n0 0\nRPRT 0\n".into()
        }

        // ── Unknown ───────────────────────────────────────────────────────────
        _ => {
            tracing::debug!(cmd = verb, "rigctl: unknown command");
            "RPRT -11\n".into() // RIG_ENAVAIL
        }
    }
}

/// Convert our DemodMode to a Hamlib mode string and default passband (Hz).
fn mode_to_hamlib(mode: DemodMode) -> (&'static str, u32) {
    match mode {
        DemodMode::Usb => ("USB", 3_000),
        DemodMode::Lsb => ("LSB", 3_000),
        DemodMode::Am => ("AM", 10_000),
        DemodMode::Nfm => ("FM", 12_500),
        DemodMode::Wbfm => ("WFM", 200_000),
        DemodMode::Cw => ("CW", 500),
        DemodMode::Dsb => ("DSB", 6_000),
    }
}

/// Parse a Hamlib mode string to our DemodMode.
fn hamlib_to_mode(s: &str) -> Option<DemodMode> {
    match s.to_ascii_uppercase().as_str() {
        "USB" | "PKTUSB" => Some(DemodMode::Usb),
        "LSB" | "PKTLSB" => Some(DemodMode::Lsb),
        "AM" | "SAM" | "SAL" | "SAH" => Some(DemodMode::Am),
        "FM" | "FMN" | "PKTFM" => Some(DemodMode::Nfm),
        "WFM" => Some(DemodMode::Wbfm),
        "CW" | "CWR" | "CWUSB" | "CWLSB" => Some(DemodMode::Cw),
        "DSB" => Some(DemodMode::Dsb),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_roundtrip() {
        for mode in [
            DemodMode::Usb,
            DemodMode::Lsb,
            DemodMode::Am,
            DemodMode::Nfm,
            DemodMode::Wbfm,
            DemodMode::Cw,
            DemodMode::Dsb,
        ] {
            let (s, _) = mode_to_hamlib(mode);
            assert_eq!(hamlib_to_mode(s), Some(mode));
        }
    }

    #[test]
    fn pkt_aliases_map_to_ssb() {
        assert_eq!(hamlib_to_mode("PKTUSB"), Some(DemodMode::Usb));
        assert_eq!(hamlib_to_mode("PKTLSB"), Some(DemodMode::Lsb));
    }

    #[test]
    fn unknown_mode_returns_none() {
        assert!(hamlib_to_mode("OLIVIA").is_none());
    }
}
