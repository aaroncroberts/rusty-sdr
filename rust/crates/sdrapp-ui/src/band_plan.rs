//! USA frequency band allocations for the spectrum overlay.
//!
//! Each entry specifies the ITU / FCC-assigned band type so the spectrum
//! renderer can colour each region consistently.

use egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandType {
    Broadcast,
    Amateur,
    Aviation,
    Marine,
    Military,
    Satellite,
    Other,
}

impl BandType {
    /// Semi-transparent fill colour for the band region.
    pub fn fill(self) -> Color32 {
        match self {
            BandType::Broadcast => Color32::from_rgba_unmultiplied(180, 100, 0, 10),
            BandType::Amateur => Color32::from_rgba_unmultiplied(0, 180, 70, 10),
            BandType::Aviation => Color32::from_rgba_unmultiplied(0, 140, 255, 10),
            BandType::Marine => Color32::from_rgba_unmultiplied(0, 210, 200, 10),
            BandType::Military => Color32::from_rgba_unmultiplied(200, 60, 220, 10),
            BandType::Satellite => Color32::from_rgba_unmultiplied(80, 80, 255, 10),
            BandType::Other => Color32::from_rgba_unmultiplied(150, 150, 150, 8),
        }
    }

    /// Top edge / label colour — brighter version of fill for contrast.
    pub fn accent(self) -> Color32 {
        match self {
            BandType::Broadcast => Color32::from_rgba_unmultiplied(255, 160, 40, 200),
            BandType::Amateur => Color32::from_rgba_unmultiplied(60, 230, 110, 200),
            BandType::Aviation => Color32::from_rgba_unmultiplied(60, 180, 255, 200),
            BandType::Marine => Color32::from_rgba_unmultiplied(40, 240, 220, 200),
            BandType::Military => Color32::from_rgba_unmultiplied(230, 100, 255, 200),
            BandType::Satellite => Color32::from_rgba_unmultiplied(140, 140, 255, 200),
            BandType::Other => Color32::from_rgba_unmultiplied(180, 180, 180, 180),
        }
    }
}

pub struct Band {
    pub name: &'static str,
    pub band_type: BandType,
    /// Start frequency in Hz (inclusive).
    pub start_hz: u64,
    /// End frequency in Hz (exclusive).
    pub end_hz: u64,
}

/// USA band plan, sourced from the upstream SDR++ band plan JSON.
pub static USA: &[Band] = &[
    // ── LF / MF ──────────────────────────────────────────────────────────────
    Band {
        name: "2200m",
        band_type: BandType::Amateur,
        start_hz: 135_700,
        end_hz: 137_800,
    },
    Band {
        name: "Long Wave",
        band_type: BandType::Broadcast,
        start_hz: 148_500,
        end_hz: 519_000,
    },
    Band {
        name: "AM Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 525_000,
        end_hz: 1_705_000,
    },
    Band {
        name: "160m Ham",
        band_type: BandType::Amateur,
        start_hz: 1_800_000,
        end_hz: 2_000_000,
    },
    // ── HF Shortwave / Ham ────────────────────────────────────────────────────
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 2_300_000,
        end_hz: 2_468_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 3_200_000,
        end_hz: 3_400_000,
    },
    Band {
        name: "80m Ham",
        band_type: BandType::Amateur,
        start_hz: 3_500_000,
        end_hz: 4_000_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 4_750_000,
        end_hz: 4_995_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 5_005_000,
        end_hz: 5_060_000,
    },
    Band {
        name: "60m Ham",
        band_type: BandType::Amateur,
        start_hz: 5_330_500,
        end_hz: 5_406_500,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 5_900_000,
        end_hz: 6_200_000,
    },
    Band {
        name: "40m Ham",
        band_type: BandType::Amateur,
        start_hz: 7_000_000,
        end_hz: 7_300_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 7_300_000,
        end_hz: 7_450_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 9_400_000,
        end_hz: 9_900_000,
    },
    Band {
        name: "30m Ham",
        band_type: BandType::Amateur,
        start_hz: 10_100_000,
        end_hz: 10_150_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 11_600_000,
        end_hz: 12_100_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 13_570_000,
        end_hz: 13_870_000,
    },
    Band {
        name: "20m Ham",
        band_type: BandType::Amateur,
        start_hz: 14_000_000,
        end_hz: 14_350_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 15_100_000,
        end_hz: 15_800_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 17_480_000,
        end_hz: 17_900_000,
    },
    Band {
        name: "17m Ham",
        band_type: BandType::Amateur,
        start_hz: 18_068_000,
        end_hz: 18_168_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 18_900_000,
        end_hz: 19_020_000,
    },
    Band {
        name: "15m Ham",
        band_type: BandType::Amateur,
        start_hz: 21_000_000,
        end_hz: 21_450_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 21_450_000,
        end_hz: 21_850_000,
    },
    Band {
        name: "12m Ham",
        band_type: BandType::Amateur,
        start_hz: 24_890_000,
        end_hz: 24_990_000,
    },
    Band {
        name: "SW Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 25_670_000,
        end_hz: 26_100_000,
    },
    Band {
        name: "CB",
        band_type: BandType::Amateur,
        start_hz: 26_960_000,
        end_hz: 27_410_000,
    },
    Band {
        name: "10m Ham",
        band_type: BandType::Amateur,
        start_hz: 28_000_000,
        end_hz: 29_700_000,
    },
    // ── VHF ──────────────────────────────────────────────────────────────────
    Band {
        name: "6m Ham",
        band_type: BandType::Amateur,
        start_hz: 50_000_000,
        end_hz: 54_000_000,
    },
    Band {
        name: "TV Ch 2-4",
        band_type: BandType::Broadcast,
        start_hz: 54_000_000,
        end_hz: 72_000_000,
    },
    Band {
        name: "TV Ch 5-6",
        band_type: BandType::Broadcast,
        start_hz: 76_000_000,
        end_hz: 88_000_000,
    },
    Band {
        name: "FM Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 87_500_000,
        end_hz: 108_000_000,
    },
    Band {
        name: "Air VOR/ILS",
        band_type: BandType::Aviation,
        start_hz: 108_000_000,
        end_hz: 118_000_000,
    },
    Band {
        name: "Air Voice",
        band_type: BandType::Aviation,
        start_hz: 118_000_000,
        end_hz: 137_000_000,
    },
    Band {
        name: "Polar Sats",
        band_type: BandType::Satellite,
        start_hz: 137_000_000,
        end_hz: 138_000_000,
    },
    Band {
        name: "2m Ham",
        band_type: BandType::Amateur,
        start_hz: 144_000_000,
        end_hz: 148_000_000,
    },
    Band {
        name: "MURS",
        band_type: BandType::Amateur,
        start_hz: 151_820_000,
        end_hz: 151_940_000,
    },
    Band {
        name: "MURS",
        band_type: BandType::Amateur,
        start_hz: 154_570_000,
        end_hz: 154_600_000,
    },
    Band {
        name: "Marine",
        band_type: BandType::Marine,
        start_hz: 156_000_000,
        end_hz: 162_025_000,
    },
    Band {
        name: "NOAA Weather",
        band_type: BandType::Broadcast,
        start_hz: 162_362_500,
        end_hz: 162_587_500,
    },
    Band {
        name: "TV Ch 7-13",
        band_type: BandType::Broadcast,
        start_hz: 174_000_000,
        end_hz: 216_000_000,
    },
    Band {
        name: "1.25m Ham",
        band_type: BandType::Amateur,
        start_hz: 219_000_000,
        end_hz: 220_000_000,
    },
    Band {
        name: "1.25m Ham",
        band_type: BandType::Amateur,
        start_hz: 222_000_000,
        end_hz: 225_000_000,
    },
    // ── UHF ──────────────────────────────────────────────────────────────────
    Band {
        name: "Military Air",
        band_type: BandType::Military,
        start_hz: 225_000_000,
        end_hz: 380_000_000,
    },
    Band {
        name: "70cm Ham",
        band_type: BandType::Amateur,
        start_hz: 420_000_000,
        end_hz: 450_000_000,
    },
    Band {
        name: "FRS/GMRS",
        band_type: BandType::Amateur,
        start_hz: 462_550_000,
        end_hz: 467_725_000,
    },
    Band {
        name: "TV Ch 14-36",
        band_type: BandType::Broadcast,
        start_hz: 470_000_000,
        end_hz: 608_000_000,
    },
    Band {
        name: "TV Broadcast",
        band_type: BandType::Broadcast,
        start_hz: 614_000_000,
        end_hz: 698_000_000,
    },
    // ── SHF ──────────────────────────────────────────────────────────────────
    Band {
        name: "33cm Ham",
        band_type: BandType::Amateur,
        start_hz: 902_000_000,
        end_hz: 928_000_000,
    },
    Band {
        name: "23cm Ham",
        band_type: BandType::Amateur,
        start_hz: 1_240_000_000,
        end_hz: 1_300_000_000,
    },
    Band {
        name: "13cm Ham",
        band_type: BandType::Amateur,
        start_hz: 2_300_000_000,
        end_hz: 2_310_000_000,
    },
    Band {
        name: "13cm Ham",
        band_type: BandType::Amateur,
        start_hz: 2_390_000_000,
        end_hz: 2_450_000_000,
    },
];
