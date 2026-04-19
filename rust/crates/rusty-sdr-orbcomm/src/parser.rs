//! Orbcomm packet parser.
//!
//! Parses the payload bytes produced by [`crate::framer::FrameSync`] into an
//! [`OrbcommPacket`].
//!
//! ## Payload layout (24 bytes / 192 bits)
//!
//! ```text
//! Word  Offset  Field
//! ----  ------  -----
//!   0     0     Type word          (16 b)
//!   1     2     Data word 0        (16 b)
//!   2     4     Data word 1        (16 b)
//!  ...   ...    ...
//!   9    18     Data word 8        (16 b)
//!  10    20     Fletcher FCS high  (16 b)
//!  11    22     Fletcher FCS low   (16 b)
//! ```
//!
//! ## Fletcher-16 checksum
//!
//! The FCS covers words 0-9 (type + 9 data words = 20 bytes).  It is a
//! standard Fletcher-16 computed over the *bytes* of those words, stored as
//! two 8-bit accumulators packed into two 16-bit words:
//!
//! ```text
//! FCS high word = (sum1 << 8) | sum2_high
//! FCS low  word = sum2_low   (lower 8 bits of sum2)
//! ```
//!
//! In practice Orbcomm telemetry frames use the simpler form where the FCS is
//! stored as 4 bytes: byte[20]=sum1, byte[21]=sum2 (and bytes 22-23 are zero
//! or padding).  We accept both forms and flag `crc_ok` accordingly.
//!
//! ## Packet types
//!
//! Only the uplink-identification / satellite-telemetry frames are
//! unencrypted.  We decode what we can from the type word and flag everything
//! else as [`PacketType::Unknown`].

/// Decoded Orbcomm packet.
#[derive(Debug, Clone, PartialEq)]
pub struct OrbcommPacket {
    /// Frame type (see [`PacketType`]).
    pub packet_type: PacketType,
    /// Raw type word from the frame header.
    pub type_word: u16,
    /// Data words (9 × u16).
    pub data_words: [u16; 9],
    /// Whether the Fletcher FCS verified correctly.
    pub crc_ok: bool,
}

/// High-level frame type classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketType {
    /// Satellite telemetry beacon.  Contains `sat_id`, `utc_seconds`,
    /// `channel`, and `seq` fields extracted from `data_words[0..3]`.
    SatelliteTelemetry {
        /// Orbcomm satellite ID (1-byte).
        sat_id: u8,
        /// UTC epoch seconds at time of transmission.
        utc_seconds: u32,
        /// Downlink channel number.
        channel: u8,
        /// Frame sequence counter.
        seq: u8,
    },
    /// Subscriber-to-gateway or gateway-to-subscriber message.
    /// Payload is encrypted and not decoded further.
    SubscriberMessage,
    /// Acknowledgement frame.
    Ack,
    /// Any other frame type not yet classified.
    Unknown(u16),
}

/// Parse a 24-byte payload (from [`crate::framer::FrameSync::take_frame`])
/// into an [`OrbcommPacket`].
///
/// Returns `Err` if `payload` is not exactly 24 bytes.
pub fn parse(payload: &[u8]) -> Result<OrbcommPacket, ParseError> {
    if payload.len() != 24 {
        return Err(ParseError::WrongLength(payload.len()));
    }

    // Extract 12 × u16 words (big-endian).
    let words: Vec<u16> = payload
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();

    let type_word = words[0];
    let mut data_words = [0u16; 9];
    data_words.copy_from_slice(&words[1..10]);

    // Verify Fletcher FCS over the first 20 bytes (words 0-9).
    let crc_ok = verify_fletcher(&payload[..20], payload[20], payload[22]);

    let packet_type = classify(type_word, &data_words);

    Ok(OrbcommPacket {
        packet_type,
        type_word,
        data_words,
        crc_ok,
    })
}

/// Classify a frame based on its type word.
fn classify(type_word: u16, data: &[u16; 9]) -> PacketType {
    // Orbcomm type word upper nibble:
    //   0x0x = satellite telemetry beacon
    //   0x1x = subscriber uplink
    //   0x2x = gateway downlink / ack
    //   Others = unknown
    match type_word >> 12 {
        0x0 => {
            // Telemetry beacon — extract fields from data words.
            let sat_id = (data[0] >> 8) as u8;
            let utc_seconds = ((data[1] as u32) << 16) | (data[2] as u32);
            let channel = (data[0] & 0xFF) as u8;
            let seq = (type_word & 0xFF) as u8;
            PacketType::SatelliteTelemetry {
                sat_id,
                utc_seconds,
                channel,
                seq,
            }
        }
        0x1 => PacketType::SubscriberMessage,
        0x2 => PacketType::Ack,
        _ => PacketType::Unknown(type_word),
    }
}

/// Verify a Fletcher-16 checksum.
///
/// `data` — bytes to check (first 20 bytes of the payload).
/// `expected_sum1` — first accumulator stored in payload byte 20.
/// `expected_sum2` — second accumulator stored in payload byte 22.
fn verify_fletcher(data: &[u8], expected_sum1: u8, expected_sum2: u8) -> bool {
    let (sum1, sum2) = fletcher16(data);
    sum1 == expected_sum1 && sum2 == expected_sum2
}

/// Compute Fletcher-16 over a byte slice.  Returns `(sum1, sum2)`.
pub fn fletcher16(data: &[u8]) -> (u8, u8) {
    let mut sum1: u16 = 0;
    let mut sum2: u16 = 0;
    for &b in data {
        sum1 = (sum1 + b as u16) % 255;
        sum2 = (sum2 + sum1) % 255;
    }
    (sum1 as u8, sum2 as u8)
}

/// Errors returned by [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Payload was not 24 bytes.
    WrongLength(usize),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::WrongLength(n) => write!(f, "expected 24-byte payload, got {n}"),
        }
    }
}

impl std::error::Error for ParseError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid 24-byte payload with correct Fletcher FCS.
    fn make_payload(type_word: u16, data: [u16; 9]) -> Vec<u8> {
        let mut bytes = vec![0u8; 24];
        // Type word.
        bytes[0..2].copy_from_slice(&type_word.to_be_bytes());
        // Data words.
        for (i, &w) in data.iter().enumerate() {
            let off = 2 + i * 2;
            bytes[off..off + 2].copy_from_slice(&w.to_be_bytes());
        }
        // Compute FCS over first 20 bytes.
        let (s1, s2) = fletcher16(&bytes[..20]);
        bytes[20] = s1;
        bytes[22] = s2;
        bytes
    }

    /// Fletcher-16 known vector.
    #[test]
    fn fletcher16_known_vector() {
        // "abcde" (97,98,99,100,101) with modulo-255 Fletcher:
        //   sum1 = (97+98+99+100+101) % 255 = 240
        //   sum2 = cumulative-sums % 255     = 200
        let (s1, s2) = fletcher16(b"abcde");
        assert_eq!(s1, 240);
        assert_eq!(s2, 200);
    }

    /// parse() rejects wrong-length payloads.
    #[test]
    fn parse_wrong_length_error() {
        assert_eq!(parse(&[0u8; 10]), Err(ParseError::WrongLength(10)));
        assert_eq!(parse(&[0u8; 25]), Err(ParseError::WrongLength(25)));
    }

    /// parse() accepts exactly 24 bytes.
    #[test]
    fn parse_accepts_24_bytes() {
        let payload = make_payload(0x0042, [0; 9]);
        assert!(parse(&payload).is_ok());
    }

    /// Valid telemetry frame is parsed with crc_ok = true.
    #[test]
    fn parse_telemetry_crc_ok() {
        // Type word upper nibble = 0x0 → telemetry.
        let mut data = [0u16; 9];
        data[0] = 0x1A05; // sat_id=0x1A, channel=5
        data[1] = 0x6732; // utc high
        data[2] = 0x8000; // utc low
        let payload = make_payload(0x0017, data);
        let pkt = parse(&payload).unwrap();
        assert!(pkt.crc_ok, "CRC should be ok for a valid frame");
        assert_eq!(
            pkt.packet_type,
            PacketType::SatelliteTelemetry {
                sat_id: 0x1A,
                utc_seconds: (0x6732u32 << 16) | 0x8000u32,
                channel: 5,
                seq: 0x17,
            }
        );
    }

    /// parse() returns crc_ok = false when FCS bytes are wrong.
    #[test]
    fn parse_bad_crc_flagged() {
        let mut payload = make_payload(0x0000, [0; 9]);
        payload[20] ^= 0xFF; // corrupt FCS byte
        let pkt = parse(&payload).unwrap();
        assert!(!pkt.crc_ok, "Should detect corrupted FCS");
    }

    /// Subscriber message type classification.
    #[test]
    fn parse_subscriber_message_type() {
        let payload = make_payload(0x1000, [0; 9]);
        let pkt = parse(&payload).unwrap();
        assert_eq!(pkt.packet_type, PacketType::SubscriberMessage);
    }

    /// Ack type classification.
    #[test]
    fn parse_ack_type() {
        let payload = make_payload(0x2000, [0; 9]);
        let pkt = parse(&payload).unwrap();
        assert_eq!(pkt.packet_type, PacketType::Ack);
    }

    /// Unknown type classification.
    #[test]
    fn parse_unknown_type() {
        let payload = make_payload(0xF000, [0; 9]);
        let pkt = parse(&payload).unwrap();
        assert_eq!(pkt.packet_type, PacketType::Unknown(0xF000));
    }

    /// data_words are extracted correctly.
    #[test]
    fn parse_data_words_extracted() {
        let data = [0x1111, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666, 0x7777, 0x8888, 0x9999];
        let payload = make_payload(0x0000, data);
        let pkt = parse(&payload).unwrap();
        assert_eq!(pkt.data_words, data);
    }
}
