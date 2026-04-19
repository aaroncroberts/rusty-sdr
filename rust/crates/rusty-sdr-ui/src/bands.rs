#![forbid(unsafe_code)]

//! Known radio bands — frequency range, display name, and expected demod mode.
//!
//! Used by the frequency context label (spectrum corner) and the demod
//! auto-suggest hint.

use rusty_sdr_core::signal_path::DemodMode;

pub struct BandInfo {
    pub name: &'static str,
    /// Inclusive lower bound in Hz.
    pub low_hz: u64,
    /// Inclusive upper bound in Hz.
    pub high_hz: u64,
    /// Expected demod mode for this band, if any.
    pub expected_demod: Option<DemodMode>,
}

/// All known bands, checked in order — first match wins.
pub static BANDS: &[BandInfo] = &[
    BandInfo {
        name: "AM Broadcast",
        low_hz: 520_000,
        high_hz: 1_710_000,
        expected_demod: Some(DemodMode::Am),
    },
    BandInfo {
        name: "160m Amateur",
        low_hz: 1_800_000,
        high_hz: 2_000_000,
        expected_demod: Some(DemodMode::Lsb),
    },
    BandInfo {
        name: "80m Amateur",
        low_hz: 3_500_000,
        high_hz: 4_000_000,
        expected_demod: Some(DemodMode::Lsb),
    },
    BandInfo {
        name: "40m Amateur",
        low_hz: 7_000_000,
        high_hz: 7_300_000,
        expected_demod: Some(DemodMode::Lsb),
    },
    BandInfo {
        name: "20m Amateur",
        low_hz: 14_000_000,
        high_hz: 14_350_000,
        expected_demod: Some(DemodMode::Usb),
    },
    BandInfo {
        name: "Aviation HF",
        low_hz: 2_850_000,
        high_hz: 18_030_000,
        expected_demod: Some(DemodMode::Usb),
    },
    BandInfo {
        name: "CB Radio",
        low_hz: 26_965_000,
        high_hz: 27_405_000,
        expected_demod: Some(DemodMode::Am),
    },
    BandInfo {
        name: "10m Amateur",
        low_hz: 28_000_000,
        high_hz: 29_700_000,
        expected_demod: Some(DemodMode::Usb),
    },
    BandInfo {
        name: "FM Broadcast",
        low_hz: 87_000_000,
        high_hz: 108_000_000,
        expected_demod: Some(DemodMode::Wbfm),
    },
    BandInfo {
        name: "Aviation VHF",
        low_hz: 108_000_000,
        high_hz: 136_975_000,
        expected_demod: Some(DemodMode::Am),
    },
    BandInfo {
        name: "2m Amateur",
        low_hz: 144_000_000,
        high_hz: 148_000_000,
        expected_demod: Some(DemodMode::Nfm),
    },
    BandInfo {
        name: "NOAA Weather",
        low_hz: 162_400_000,
        high_hz: 162_550_000,
        expected_demod: Some(DemodMode::Nfm),
    },
    BandInfo {
        name: "Marine VHF",
        low_hz: 156_000_000,
        high_hz: 174_000_000,
        expected_demod: Some(DemodMode::Nfm),
    },
    BandInfo {
        name: "ISM 433 MHz",
        low_hz: 433_050_000,
        high_hz: 434_790_000,
        expected_demod: None,
    },
    BandInfo {
        name: "70cm Amateur",
        low_hz: 420_000_000,
        high_hz: 450_000_000,
        expected_demod: Some(DemodMode::Nfm),
    },
    BandInfo {
        name: "ISM 915 MHz",
        low_hz: 902_000_000,
        high_hz: 928_000_000,
        expected_demod: None,
    },
];

/// Returns the first band whose range contains `freq_hz`, or `None`.
pub fn band_for_freq(freq_hz: u64) -> Option<&'static BandInfo> {
    BANDS
        .iter()
        .find(|b| freq_hz >= b.low_hz && freq_hz <= b.high_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm_broadcast_detected() {
        let b = band_for_freq(105_700_000).unwrap();
        assert_eq!(b.name, "FM Broadcast");
        assert_eq!(b.expected_demod, Some(DemodMode::Wbfm));
    }

    #[test]
    fn unknown_frequency_returns_none() {
        assert!(band_for_freq(50_000_000).is_none());
    }

    #[test]
    fn noaa_weather_detected() {
        let b = band_for_freq(162_475_000).unwrap();
        assert_eq!(b.name, "NOAA Weather");
    }
}
