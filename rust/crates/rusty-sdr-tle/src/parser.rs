//! Two-Line Element set parser.
//!
//! TLE files consist of groups of three text lines:
//! ```text
//! ORBCOMM FM107           ← title line (satellite name)
//! 1 40086U 14037A   ...   ← TLE line 1 (epoch, drag, etc.)
//! 2 40086  47.0001 ...    ← TLE line 2 (orbital elements)
//! ```
//!
//! [`parse_tle_text`] accepts the full multi-satellite text from Celestrak
//! and returns one [`TleEntry`] per satellite.

/// One parsed TLE entry (title + two lines).
#[derive(Debug, Clone)]
pub struct TleEntry {
    /// Human-readable satellite name (from the title line).
    pub name: String,
    /// NORAD catalog number extracted from TLE line 1.
    pub norad_id: u32,
    /// Raw TLE line 1 (exactly as received).
    pub line1: String,
    /// Raw TLE line 2 (exactly as received).
    pub line2: String,
}

/// Parse a multi-satellite TLE text (e.g. from Celestrak) into entries.
///
/// Lines are trimmed; blank lines are skipped.  Every group of three
/// non-blank lines is interpreted as (title, line1, line2).
///
/// Returns an error if the NORAD ID field in line 1 is malformed.
pub fn parse_tle_text(text: &str) -> Result<Vec<TleEntry>, ParseError> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if lines.len() % 3 != 0 {
        return Err(ParseError::WrongLineCount(lines.len()));
    }

    let mut entries = Vec::with_capacity(lines.len() / 3);
    for chunk in lines.chunks_exact(3) {
        let title = chunk[0].to_string();
        let line1 = chunk[1].to_string();
        let line2 = chunk[2].to_string();

        // Validate line identifiers.
        if !line1.starts_with('1') {
            return Err(ParseError::MissingLine1(title.clone()));
        }
        if !line2.starts_with('2') {
            return Err(ParseError::MissingLine2(title.clone()));
        }

        // Extract NORAD ID from columns 2-6 of line 1 (1-indexed: cols 3-7).
        let norad_str = line1.get(2..7).unwrap_or("").trim();
        let norad_id = norad_str
            .parse::<u32>()
            .map_err(|_| ParseError::BadNoradId(title.clone(), norad_str.to_string()))?;

        entries.push(TleEntry { name: title, norad_id, line1, line2 });
    }

    Ok(entries)
}

/// Errors from [`parse_tle_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Total non-blank line count is not divisible by 3.
    WrongLineCount(usize),
    /// Expected a line starting with `1` for TLE line 1.
    MissingLine1(String),
    /// Expected a line starting with `2` for TLE line 2.
    MissingLine2(String),
    /// NORAD ID field in line 1 is not a valid integer.
    BadNoradId(String, String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::WrongLineCount(n) => {
                write!(f, "TLE text has {n} non-blank lines (not divisible by 3)")
            }
            ParseError::MissingLine1(name) => {
                write!(f, "Expected TLE line 1 for '{name}'")
            }
            ParseError::MissingLine2(name) => {
                write!(f, "Expected TLE line 2 for '{name}'")
            }
            ParseError::BadNoradId(name, raw) => {
                write!(f, "Bad NORAD ID '{raw}' for '{name}'")
            }
        }
    }
}

impl std::error::Error for ParseError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal but syntactically valid TLE for ORBCOMM FM107 (OG2).
    const SAMPLE_TLE: &str = "\
ORBCOMM FM107
1 40086U 14037A   24001.50000000  .00000100  00000-0  10000-4 0  9994
2 40086  47.0001 123.4567 0001234  12.3456 347.6543 14.40000000123456
";

    #[test]
    fn parse_single_entry() {
        let entries = parse_tle_text(SAMPLE_TLE).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ORBCOMM FM107");
        assert_eq!(entries[0].norad_id, 40086);
        assert!(entries[0].line1.starts_with('1'));
        assert!(entries[0].line2.starts_with('2'));
    }

    #[test]
    fn parse_multiple_entries() {
        let two_sats = format!("{SAMPLE_TLE}{SAMPLE_TLE}");
        let entries = parse_tle_text(&two_sats).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let with_blanks = format!("\n\n{SAMPLE_TLE}\n\n");
        let entries = parse_tle_text(&with_blanks).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn wrong_line_count_returns_error() {
        // Only 2 lines (no title)
        let bad = "1 40086U 14037A   24001.50000000  .00000100  00000-0  10000-4 0  9994\n\
                   2 40086  47.0001 123.4567 0001234  12.3456 347.6543 14.40000000123456\n";
        assert!(matches!(parse_tle_text(bad), Err(ParseError::WrongLineCount(2))));
    }

    #[test]
    fn missing_line1_marker_returns_error() {
        // Swap line 1 and line 2 so line1 starts with '2' instead of '1'
        let bad = "ORBCOMM FM107\n\
                   2 40086  47.0001 123.4567 0001234  12.3456 347.6543 14.40000000123456\n\
                   2 40086  47.0001 123.4567 0001234  12.3456 347.6543 14.40000000123456\n";
        assert!(matches!(parse_tle_text(bad), Err(ParseError::MissingLine1(_))));
    }

    #[test]
    fn norad_id_extracted_correctly() {
        let entries = parse_tle_text(SAMPLE_TLE).unwrap();
        assert_eq!(entries[0].norad_id, 40_086);
    }

    #[test]
    fn norad_id_lookup_known_ids() {
        use crate::orbcomm_norad_id;
        assert_eq!(orbcomm_norad_id(1), Some(40_086));
        assert_eq!(orbcomm_norad_id(7), Some(40_967));
        assert_eq!(orbcomm_norad_id(18), Some(40_978));
    }

    #[test]
    fn norad_id_lookup_unknown_returns_none() {
        use crate::orbcomm_norad_id;
        assert_eq!(orbcomm_norad_id(0), None);
        assert_eq!(orbcomm_norad_id(255), None);
    }
}
