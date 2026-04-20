//! NOAA APT (Automatic Picture Transmission) decoder.
//!
//! Feed 48 kHz mono f32 audio from a WFM/NFM demodulator into [`AptDecoder`].
//! When a sync is detected and a full line assembled, [`AptDecoder::push_audio`]
//! returns [`AptLine`] values that can be assembled into an image.

mod demod;
mod framer;

pub use framer::AptLine;

use demod::EnvelopeDetector;
use framer::AptFramer;

/// NOAA satellite downlink frequencies.
pub const NOAA_SATS: &[(&str, u64)] = &[
    ("NOAA-15", 137_620_000),
    ("NOAA-18", 137_912_500),
    ("NOAA-19", 137_100_000),
];

/// APT pixels per line (both channels combined).
pub const PIXELS_PER_LINE: usize = 2080;
/// Audio sample rate expected by this decoder.
pub const AUDIO_SAMPLE_RATE: f32 = 48_000.0;
/// APT line rate (lines per second).
pub const LINE_RATE: f32 = 2.0;
/// Samples per line at [`AUDIO_SAMPLE_RATE`].
pub const SAMPLES_PER_LINE: usize = (AUDIO_SAMPLE_RATE / LINE_RATE) as usize; // 24000

/// Pixel range for channel A image data within a line.
pub const CHAN_A_RANGE: std::ops::Range<usize> = 86..995;
/// Pixel range for channel B image data within a line.
pub const CHAN_B_RANGE: std::ops::Range<usize> = 1126..2035;

/// Width of channel A image in pixels.
pub const CHAN_A_WIDTH: usize = CHAN_A_RANGE.end - CHAN_A_RANGE.start; // 909
/// Width of channel B image in pixels.
pub const CHAN_B_WIDTH: usize = CHAN_B_RANGE.end - CHAN_B_RANGE.start; // 909

/// Stateful NOAA APT decoder.
///
/// Call [`push_audio`] with consecutive 48 kHz mono f32 audio batches.
/// Returns decoded image lines as they complete.
pub struct AptDecoder {
    detector: EnvelopeDetector,
    framer: AptFramer,
}

impl AptDecoder {
    pub fn new() -> Self {
        Self {
            detector: EnvelopeDetector::new(),
            framer: AptFramer::new(),
        }
    }

    /// Push a batch of 48 kHz mono f32 audio samples.
    /// Returns any complete [`AptLine`]s decoded from this batch.
    pub fn push_audio(&mut self, audio: &[f32]) -> Vec<AptLine> {
        let envelope = self.detector.process(audio);
        self.framer.push_envelope(&envelope)
    }

    /// Reset decoder state (call when starting a new pass).
    pub fn reset(&mut self) {
        self.detector.reset();
        self.framer.reset();
    }

    /// Number of complete lines received so far this session.
    pub fn line_count(&self) -> usize {
        self.framer.line_count()
    }
}

impl Default for AptDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ── PNG save helper ────────────────────────────────────────────────────────────

/// Save a collected set of APT lines as a PNG file.
///
/// The image is laid out as two side-by-side 909-pixel channels (A on the left,
/// B on the right), giving a total width of 1818 pixels and height = line count.
/// Returns the path the file was saved to, or an error string.
pub fn save_png(lines: &[AptLine], path: &std::path::Path) -> Result<(), String> {
    if lines.is_empty() {
        return Err("No lines to save".into());
    }
    let height = lines.len();
    let width = CHAN_A_WIDTH + CHAN_B_WIDTH; // 1818
    let mut pixels: Vec<u8> = vec![0u8; width * height];

    for (row, line) in lines.iter().enumerate() {
        // Channel A
        for (col, &px) in line.pixels[CHAN_A_RANGE].iter().enumerate() {
            pixels[row * width + col] = px;
        }
        // Channel B
        for (col, &px) in line.pixels[CHAN_B_RANGE].iter().enumerate() {
            pixels[row * width + CHAN_A_WIDTH + col] = px;
        }
    }

    // Use the image crate to write a grayscale PNG
    image::save_buffer(path, &pixels, width as u32, height as u32, image::ColorType::L8)
        .map_err(|e| format!("PNG write error: {e}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_starts_with_zero_lines() {
        let d = AptDecoder::new();
        assert_eq!(d.line_count(), 0);
    }

    #[test]
    fn decoder_reset_clears_count() {
        let mut d = AptDecoder::new();
        // push silence — no lines expected, but state is exercised
        let lines = d.push_audio(&vec![0.0f32; 48_000]);
        assert!(lines.is_empty());
        d.reset();
        assert_eq!(d.line_count(), 0);
    }

    #[test]
    fn samples_per_line_is_24000() {
        assert_eq!(SAMPLES_PER_LINE, 24_000);
    }

    #[test]
    fn chan_widths_are_909() {
        assert_eq!(CHAN_A_WIDTH, 909);
        assert_eq!(CHAN_B_WIDTH, 909);
    }
}
