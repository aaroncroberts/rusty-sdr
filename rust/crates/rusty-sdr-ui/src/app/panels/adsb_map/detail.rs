//! Aircraft detail panel and pure formatting helpers.

use egui::{Color32, Grid, RichText, Sense, Vec2};
use rusty_sdr_atc_db::AtcFrequency;
use rusty_sdr_adsb::state::AircraftState;

use super::flight_info::{FlightInfo, FlightLookupState};

// ── Aircraft detail panel ─────────────────────────────────────────────────────

/// Render the aircraft detail side panel.
/// Returns `true` if the user clicked the deselect button.
pub(super) fn show_aircraft_detail(
    ui: &mut egui::Ui,
    ac: &AircraftState,
    flight: Option<&FlightLookupState>,
    flight_expanded: &mut bool,
    atc_freqs: &[AtcFrequency],
    tune_frequency_hz: &mut Option<u64>,
) -> bool {
    let muted = Color32::from_rgb(0x5A, 0x6A, 0x7A);
    let value_color = Color32::from_rgb(0xD8, 0xE8, 0xF0);
    let accent = Color32::from_rgb(0x4E, 0xC9, 0xE0);
    let stale = ac.is_stale();

    // ── ICAO + stale badge ────────────────────────────────────────────────────
    let icao_str = format!("{:06X}", ac.icao);
    ui.horizontal(|ui| {
        let icao_resp = ui.add(
            egui::Label::new(
                RichText::new(&icao_str).monospace().size(18.0).color(accent),
            )
            .sense(Sense::click()),
        );
        if icao_resp.clicked() {
            ui.ctx().copy_text(icao_str.clone());
        }
        icao_resp.on_hover_text("Click to copy ICAO address");

        if stale {
            ui.label(
                RichText::new("LOST").small()
                    .color(Color32::from_rgb(0xE8, 0xA0, 0x40))
                    .background_color(Color32::from_rgba_premultiplied(60, 30, 0, 140)),
            );
        }
    });

    // ── Callsign (ADS-B broadcast or API fallback) ────────────────────────────
    let adsb_cs = ac.callsign.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let api_cs = if let Some(FlightLookupState::Ready(ref info)) = flight {
        info.callsign_api.as_deref()
    } else {
        None
    };
    let callsign = adsb_cs.or(api_cs).unwrap_or("—");
    let cs_color = if stale { muted } else { Color32::WHITE };
    ui.label(RichText::new(callsign).size(15.0).color(cs_color));

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // ── Telemetry grid ───────────────────────────────────────────────────────
    Grid::new("adsb_detail_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            // Altitude
            ui.label(RichText::new("ALT").small().color(muted));
            let alt_text = ac.altitude_ft.map(format_altitude).unwrap_or_else(|| "—".into());
            let alt_color = if ac.altitude_ft.is_some() { value_color } else { muted };
            ui.label(RichText::new(alt_text).small().color(alt_color));
            ui.end_row();

            // Speed
            ui.label(RichText::new("SPD").small().color(muted));
            let spd_text = ac.speed_kt.map(format_speed).unwrap_or_else(|| "—".into());
            let spd_color = if ac.speed_kt.is_some() { value_color } else { muted };
            ui.label(RichText::new(spd_text).small().color(spd_color));
            ui.end_row();

            // Heading
            ui.label(RichText::new("HDG").small().color(muted));
            let hdg_text = ac.heading_deg
                .map(|h| format!("{h:.0}°  {}", heading_compass(h)))
                .unwrap_or_else(|| "—".into());
            let hdg_color = if ac.heading_deg.is_some() { value_color } else { muted };
            ui.label(RichText::new(hdg_text).small().color(hdg_color));
            ui.end_row();

            // Vertical rate
            ui.label(RichText::new("V/S").small().color(muted));
            if let Some(vr) = ac.vert_rate_fpm {
                let (arrow, vr_color) = vert_rate_display(vr, muted);
                ui.label(RichText::new(format!("{arrow} {vr:+} fpm")).small().color(vr_color));
            } else {
                ui.label(RichText::new("—").small().color(muted));
            }
            ui.end_row();

            // Position
            ui.label(RichText::new("POS").small().color(muted));
            let pos_text = ac.lat.zip(ac.lon)
                .map(|(lat, lon)| format_position(lat, lon))
                .unwrap_or_else(|| "—".into());
            let pos_color = if ac.lat.is_some() { value_color } else { muted };
            ui.label(RichText::new(pos_text).small().color(pos_color));
            ui.end_row();

            // Last seen
            ui.label(RichText::new("AGE").small().color(muted));
            let age = ac.last_seen.elapsed().as_secs_f32();
            let age_color = if age < 5.0 {
                Color32::from_rgb(0x73, 0xC9, 0x91)
            } else if age < 15.0 {
                value_color
            } else {
                Color32::from_rgb(0xE8, 0xC5, 0x4B)
            };
            ui.label(RichText::new(format_age(age)).small().color(age_color));
            ui.end_row();

            // Squawk (Mode A identity code) — shown when received via DF5/21.
            if let Some(sq) = ac.squawk {
                ui.label(RichText::new("SQK").small().color(muted));
                let sq_str = format!("{sq:04}");
                let (sq_color, sq_bg) = match sq {
                    7500 => (Color32::WHITE, Color32::from_rgba_premultiplied(180, 20, 20, 200)),
                    7600 => (Color32::WHITE, Color32::from_rgba_premultiplied(180, 100, 0, 200)),
                    7700 => (Color32::WHITE, Color32::from_rgba_premultiplied(180, 20, 20, 200)),
                    _    => (value_color, Color32::TRANSPARENT),
                };
                let sq_label = RichText::new(&sq_str).small().color(sq_color)
                    .background_color(sq_bg);
                let tip = match sq {
                    7500 => " HIJACK",
                    7600 => " RADIO FAILURE",
                    7700 => " EMERGENCY",
                    _    => "",
                };
                ui.label(sq_label).on_hover_text(format!("{sq_str}{tip}"));
                ui.end_row();
            }
        });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // ── FLIGHT DATA section (collapsible) ─────────────────────────────────────
    ui.horizontal(|ui| {
        // Painted triangle toggle
        let expand_icon_rect = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover()).0;
        let c = expand_icon_rect.center();
        let tri_pts: Vec<egui::Pos2> = if *flight_expanded {
            vec![egui::pos2(c.x - 4.0, c.y - 2.5), egui::pos2(c.x + 4.0, c.y - 2.5), egui::pos2(c.x, c.y + 3.0)]
        } else {
            vec![egui::pos2(c.x - 2.5, c.y - 4.0), egui::pos2(c.x + 3.0, c.y), egui::pos2(c.x - 2.5, c.y + 4.0)]
        };
        ui.painter().add(egui::Shape::convex_polygon(tri_pts, muted, egui::Stroke::NONE));

        let hdr = ui.add(
            egui::Label::new(RichText::new("FLIGHT DATA").small().color(muted))
                .sense(Sense::click()),
        );
        if hdr.clicked() { *flight_expanded = !*flight_expanded; }

        // Status chip
        match flight {
            None | Some(FlightLookupState::Fetching) => { ui.spinner(); }
            Some(FlightLookupState::Failed) => {
                ui.label(RichText::new("—").small().color(muted));
            }
            Some(FlightLookupState::Ready(_)) => {
                let (dot_r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(dot_r.center(), 3.0, Color32::from_rgb(0x73, 0xC9, 0x91));
            }
        }
    });

    if *flight_expanded {
        ui.add_space(4.0);
        match flight {
            None | Some(FlightLookupState::Fetching) => {
                ui.label(RichText::new("  Looking up…").small().color(muted));
            }
            Some(FlightLookupState::Failed) => {
                ui.label(RichText::new("  No data available").small().color(muted));
            }
            Some(FlightLookupState::Ready(info)) => {
                show_flight_info(ui, info, muted, value_color, accent);
            }
        }
        ui.add_space(4.0);
    }

    ui.separator();
    ui.add_space(4.0);

    // ── External lookup ───────────────────────────────────────────────────────
    let icao_hex = format!("{:06X}", ac.icao);
    let fa_url    = format!("https://flightaware.com/live/modes/{}/redirect", icao_hex.to_lowercase());
    let adsbx_url = format!("https://globe.adsbexchange.com/?icao={}", icao_hex.to_lowercase());
    let ps_url    = format!("https://www.planespotters.net/hex/{}", icao_hex.to_uppercase());

    let btn_fill = Color32::from_rgb(0x16, 0x20, 0x2E);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 3.0);
        if ui.add(egui::Button::new(RichText::new("↗ FlightAware").small().color(accent)).fill(btn_fill))
            .on_hover_text(&fa_url).clicked() { let _ = open::that(&fa_url); }
        if ui.add(egui::Button::new(RichText::new("↗ ADS-B Exch.").small().color(accent)).fill(btn_fill))
            .on_hover_text(&adsbx_url).clicked() { let _ = open::that(&adsbx_url); }
        if ui.add(egui::Button::new(RichText::new("↗ Planespotters").small().color(accent)).fill(btn_fill))
            .on_hover_text(&ps_url).clicked() { let _ = open::that(&ps_url); }
    });

    // ── ATC COMMS ─────────────────────────────────────────────────────────────
    if ac.lat.is_some() && ac.lon.is_some() {
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new("ATC COMMS").small().color(muted));
        ui.add_space(2.0);

        if atc_freqs.is_empty() {
            ui.label(RichText::new("No nearby airports found").small().color(muted));
        } else {
            // VHF AM ground stations have ~50–100 nm line-of-sight range.
            // Airports beyond this threshold from the receiver home position
            // are physically unlikely to be receivable.
            const MAX_RECEIVABLE_NM: f32 = 150.0;

            let warn_color = Color32::from_rgb(0xFF, 0x88, 0x44);

            // Group by airport (walk in order — already sorted by distance)
            let mut last_ident = "";
            for f in atc_freqs {
                if f.airport_ident != last_ident {
                    last_ident = &f.airport_ident;
                    let receivable = f.home_distance_nm <= MAX_RECEIVABLE_NM;
                    let hdr_color = if receivable { accent } else { muted };
                    ui.add_space(4.0);
                    ui.label(RichText::new(&f.airport_name).small().color(hdr_color));
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("acft {:.0} nm", f.distance_nm))
                                .size(9.0)
                                .color(muted),
                        );
                        ui.label(RichText::new("·").size(9.0).color(muted));
                        let home_color = if receivable { muted } else { warn_color };
                        ui.label(
                            RichText::new(format!("home {:.0} nm", f.home_distance_nm))
                                .size(9.0)
                                .color(home_color),
                        );
                        if !receivable {
                            ui.label(
                                RichText::new("⚠ likely out of VHF range")
                                    .size(9.0)
                                    .color(warn_color),
                            );
                        }
                    });
                }
                let freq_mhz = f.freq_hz as f64 / 1_000_000.0;
                let btn_label = format!("[{}] {:.3}", f.freq_type, freq_mhz);
                let receivable = f.home_distance_nm <= MAX_RECEIVABLE_NM;
                let btn_color = if receivable { value_color } else { muted };
                let hover = if receivable {
                    format!("Tune to {freq_mhz:.3} MHz AM · pauses ADS-B")
                } else {
                    format!(
                        "Tune to {freq_mhz:.3} MHz AM · ground station is {:.0} nm away — may not be receivable",
                        f.home_distance_nm
                    )
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(&btn_label).small().color(btn_color))
                            .fill(Color32::from_rgb(0x10, 0x1E, 0x30))
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    *tune_frequency_hz = Some(f.freq_hz);
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("Tuning pauses ADS-B tracking")
                    .size(9.0)
                    .color(Color32::from_rgb(0xFF, 0xCC, 0x44)),
            );
        }
    }

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(4.0);

    // ── Deselect button ───────────────────────────────────────────────────────
    let close_clicked = ui
        .add(egui::Button::new(RichText::new("✕  Deselect").small().color(muted)).frame(false))
        .on_hover_text("Deselect aircraft (or press Escape)")
        .clicked();

    ui.add_space(2.0);
    ui.label(RichText::new("Click map to change selection").size(9.0).color(muted));

    close_clicked
}

fn show_flight_info(
    ui: &mut egui::Ui,
    info: &FlightInfo,
    muted: Color32,
    value_color: Color32,
    accent: Color32,
) {
    // ── Operator (most important — show prominently) ──────────────
    if let Some(ref op) = info.operator {
        ui.add_space(2.0);
        ui.label(RichText::new(op).color(Color32::WHITE).strong());
    }

    // ── Route banner: KORD → KJFK ─────────────────────────────────
    let has_route = info.origin.is_some() || info.destination.is_some();
    if has_route {
        let origin = info.origin.as_deref().unwrap_or("???");
        let dest   = info.destination.as_deref().unwrap_or("???");
        ui.label(
            RichText::new(format!("{origin}  →  {dest}"))
                .strong()
                .color(accent),
        );
    }

    // ── Registration + readable aircraft type ──────────────────────
    // Prefer human-readable desc ("Airbus A-321") over raw code ("A21N").
    let reg = info.registration.as_deref();
    let type_display = info.aircraft_desc.as_deref()
        .or(info.aircraft_type.as_deref());
    if reg.is_some() || type_display.is_some() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if let Some(r) = reg {
                ui.label(RichText::new(r).small().strong().color(accent));
            }
            if let (Some(_), Some(t)) = (reg, type_display) {
                ui.label(RichText::new("·").small().color(muted));
                ui.label(RichText::new(t).small().color(value_color));
            } else if let Some(t) = type_display {
                ui.label(RichText::new(t).small().color(value_color));
            }
        });
    }

    if info.operator.is_none() && !has_route && reg.is_none() {
        ui.label(RichText::new("No data available").small().color(muted));
    }
}

// ── Detail panel pure formatters ──────────────────────────────────────────────

/// Format altitude as "38000 ft / 11582 m".
pub(crate) fn format_altitude(ft: i32) -> String {
    let m = (ft as f64 * 0.3048) as i32;
    format!("{ft} ft / {m} m")
}

/// Format speed as "450 kts / 834 km/h".
pub(crate) fn format_speed(kt: f32) -> String {
    let kmh = (kt * 1.852) as u32;
    format!("{kt:.0} kts / {kmh} km/h")
}

/// Format position as "41.499°N 81.694°W".
pub(crate) fn format_position(lat: f64, lon: f64) -> String {
    let lat_hem = if lat >= 0.0 { "N" } else { "S" };
    let lon_hem = if lon >= 0.0 { "E" } else { "W" };
    format!("{:.3}°{lat_hem} {:.3}°{lon_hem}", lat.abs(), lon.abs())
}

/// Arrow glyph and Color32 for a vertical rate in fpm.
/// Returns (arrow, color): ▲ green for climb, ▼ orange for descent, ━ muted for level.
pub(crate) fn vert_rate_display(fpm: i32, muted: Color32) -> (&'static str, Color32) {
    if fpm > 64 {
        ("▲", Color32::from_rgb(0x73, 0xC9, 0x91))
    } else if fpm < -64 {
        ("▼", Color32::from_rgb(0xFF, 0x88, 0x55))
    } else {
        ("━", muted)
    }
}

/// Format age as "12.3 s ago" (< 60 s) or "2 min ago" (≥ 60 s).
pub(crate) fn format_age(secs: f32) -> String {
    if secs < 60.0 {
        format!("{secs:.1} s ago")
    } else {
        format!("{:.0} min ago", secs / 60.0)
    }
}

/// Convert an ALL-CAPS string to Title Case (e.g. "AIRBUS A-321" → "Airbus A-321").
pub(super) fn titlecase(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut cap_next = true;
    for c in s.chars() {
        if c == ' ' || c == '-' {
            result.push(c);
            cap_next = true;
        } else if cap_next {
            result.extend(c.to_uppercase());
            cap_next = false;
        } else {
            result.extend(c.to_lowercase());
        }
    }
    result
}

/// Cardinal compass label for a heading in degrees.
pub(crate) fn heading_compass(deg: f32) -> &'static str {
    let idx = ((deg + 22.5) / 45.0) as usize % 8;
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][idx]
}
