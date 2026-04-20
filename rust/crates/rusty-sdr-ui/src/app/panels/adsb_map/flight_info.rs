//! Flight info lookup types and async fetcher.

use crossbeam_channel::Sender;

use super::detail::titlecase;

// ── Flight info lookup ────────────────────────────────────────────────────────

/// Enriched aircraft data fetched from external APIs.
#[derive(Debug, Clone, Default)]
pub struct FlightInfo {
    /// Tail / registration number (e.g. "N12345", "G-EUOE").
    pub registration: Option<String>,
    /// ICAO aircraft type code (e.g. "B738", "A320").
    pub aircraft_type: Option<String>,
    /// Human-readable aircraft description (e.g. "BOEING 737-800").
    pub aircraft_desc: Option<String>,
    /// Operator / airline (e.g. "United Airlines").
    pub operator: Option<String>,
    /// Flight / callsign from API lookup (e.g. "DAL1234").
    pub callsign_api: Option<String>,
    /// ICAO departure airport (e.g. "KORD").
    pub origin: Option<String>,
    /// ICAO arrival airport (e.g. "KJFK").
    pub destination: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum FlightLookupState {
    Fetching,
    Ready(FlightInfo),
    Failed,
}

pub(super) struct FlightInfoResult {
    pub(super) icao: u32,
    pub(super) info: Option<FlightInfo>,
}

/// Spawn two background threads to fetch aircraft data:
/// 1. adsb.lol  — registration, type, operator (fast, no auth)
/// 2. OpenSky   — estimated departure / arrival airports
pub(super) fn fetch_flight_info_async(
    tx: Sender<FlightInfoResult>,
    icao: u32,
    callsign: Option<String>,
) {
    std::thread::spawn(move || {
        let hex = format!("{:06x}", icao);
        let mut info = FlightInfo::default();

        // ── adsb.lol: registration + type + operator ──────────────────────────
        let lol_url = format!("https://api.adsb.lol/v2/icao/{hex}");
        if let Ok(resp) = ureq::get(&lol_url)
            .set("User-Agent", "sdrapp ADS-B map/1.0 (desktop SDR application)")
            .call()
        {
            if let Ok(body) = resp.into_string() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(ac) = json["ac"].as_array().and_then(|a| a.first()) {
                        info.registration = ac["r"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        info.aircraft_type = ac["t"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        // Human-readable description (e.g. "AIRBUS A-321") —
                        // prefer this over the raw ICAO type code for display.
                        info.aircraft_desc = ac["desc"].as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| titlecase(s));
                        info.operator = ac["ownOp"].as_str()
                            .filter(|s| !s.is_empty()).map(str::to_string);
                        // Callsign / flight number from live feed (may be more
                        // up-to-date than what the aircraft has broadcast so far).
                        info.callsign_api = ac["flight"].as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                }
            }
        }

        // ── OpenSky: departure / arrival airports (last 24 h) ─────────────────
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let begin = now.saturating_sub(86_400);
        let sky_url = format!(
            "https://opensky-network.org/api/flights/aircraft?icao24={hex}&begin={begin}&end={now}"
        );
        if let Ok(resp) = ureq::get(&sky_url)
            .set("User-Agent", "sdrapp ADS-B map/1.0 (desktop SDR application)")
            .call()
        {
            if let Ok(body) = resp.into_string() {
                if let Ok(serde_json::Value::Array(flights)) =
                    serde_json::from_str::<serde_json::Value>(&body)
                {
                    // Use the most recent entry (last in the array).
                    if let Some(last) = flights.last() {
                        let valid_airport = |v: &serde_json::Value| -> Option<String> {
                            v.as_str()
                                .filter(|s| !s.is_empty() && *s != "null")
                                .map(str::to_string)
                        };
                        info.origin = valid_airport(&last["estDepartureAirport"]);
                        info.destination = valid_airport(&last["estArrivalAirport"]);
                        // Fall back to callsign from OpenSky if we didn't have one.
                        if callsign.as_deref().map(str::trim).unwrap_or("").is_empty() {
                            if let Some(cs) = last["callsign"].as_str()
                                .map(str::trim).filter(|s| !s.is_empty())
                            {
                                let _ = cs; // callsign already in AircraftState
                            }
                        }
                    }
                }
            }
        }

        let _ = tx.send(FlightInfoResult { icao, info: Some(info) });
    });
}
