//! Cisco CIMC vendor decoding: the LED sensors, and the Extended Sensor Range.
//!
//! # LED sensors
//!
//! CIMC exposes the chassis LEDs as Platform Alert sensors reported with an OEM
//! event/reading type, so neither their SEL events nor their readings decode
//! against the IPMI tables. See [`led_state`].
//!
//! # Extended Sensor Range
//!
//! Cisco UCS servers can carry more sensors than IPMI's 8-bit sensor number
//! allows, so CIMC keeps a parallel repository addressed by a 32-bit sensor
//! number: a sensor above 255 is a "Cisco extended sensor" (CES), and the
//! extended SEL is a superset of the standard one — it holds every standard
//! record reformatted, plus the events of extended sensors.
//!
//! That matters for decoding, because an event from an extended sensor still
//! appears in the standard SEL with an 8-bit sensor number that no standard SDR
//! record names. Reading it back through ESR is the documented way to recover
//! the real sensor number.
//!
//! Reference: *Cisco UCS Manager Troubleshooting Reference Guide*,
//! "Troubleshooting issues with Cisco IPMI Extensions".

use ipmi_rs::connection::NetFn;

use crate::conn::Conn;

/// IANA 5771. The Cisco ESR algorithm keys on this exact ID: IANA 9 is also
/// registered to Cisco, but UCS servers report 0x168B.
pub const UCS_MANUFACTURER_ID: u32 = 0x00168B;

/// Sensor type and event/reading type of the Cisco LED sensors.
pub const LED_SENSOR_TYPE: u8 = 0x24;
pub const LED_EVENT_TYPE: u8 = 0x7F;

/// One state a Cisco LED sensor reports.
///
/// A single LED carries two independent fields at once: whether it is lit, and
/// what colour it is set to. Both are asserted as separate offsets of the same
/// sensor, so a reading can hold one of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedState {
    Off,
    On,
    Blinking,
    Green,
    Amber,
    Red,
}

impl LedState {
    /// CIMC's own wording for the state, where Cisco publishes one.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "LED is off",
            Self::On => "LED is on",
            Self::Blinking => "LED is blinking",
            Self::Green => "LED color is green",
            Self::Amber => "LED color is amber",
            Self::Red => "LED color is red",
        }
    }

    /// Whether the LED is emitting light. `None` for a colour, which says
    /// nothing on its own: CIMC assigns a colour to LEDs that are switched off.
    pub fn lit(self) -> Option<bool> {
        match self {
            Self::Off => Some(false),
            Self::On | Self::Blinking => Some(true),
            Self::Green | Self::Amber | Self::Red => None,
        }
    }
}

/// Decode one offset of a Cisco LED sensor.
///
/// The offsets are documented by example in the *Cisco UCS Faults and Error
/// Messages Reference Guide*, "SEL Record Examples → LED Color Changes", which
/// prints raw SEL records beside CIMC's own translation of them:
///
/// ```text
/// .. 24 56 7f 00 04 10  ->  Platform alert LED_MEZZ_TP_FLT #0x56 | LED is off
/// .. 24 56 7f 07 04 10  ->  Platform alert LED_MEZZ_TP_FLT #0x56 | LED color is red
/// .. 24 58 7f 04 04 10  ->  Platform alert LED_SYS_ACT #0x58     | LED color is green
/// .. 24 5a 7f 05 04 10  ->  Platform alert LED_SAS1_FAULT #0x5a  | LED color is amber
/// ```
///
/// Two more offsets are established by observation rather than by Cisco:
///
/// - **01h**, the complement of 00h: a healthy C-series reads 0x12 on every
///   `LED_*_STATUS` sensor, which is offsets 01h and 04h — "on" plus "green".
/// - **02h**, a second lit state, established causally on a C240 M4. Driving
///   the chassis identify LED with Chassis Identify (`identify`) moved
///   `FP_ID_LED` from 0x41 to 0x44 and back, repeatably — offset 00h giving way
///   to 02h while the colour bit held. Timed blink and force-on both produce
///   it, so the two cannot be told apart from IPMI; "blinking" is what a
///   locator LED does, but we never saw the panel.
///
/// Offsets 03h and 06h stay undecoded. 06h is a colour this hardware does use —
/// it is the identify LED's, held across that whole experiment — but Cisco
/// names it nowhere citable, and a datacenter machine offers no way to look.
pub fn led_state(offset: u8) -> Option<LedState> {
    Some(match offset {
        0x00 => LedState::Off,
        0x01 => LedState::On,
        0x02 => LedState::Blinking,
        0x04 => LedState::Green,
        0x05 => LedState::Amber,
        0x07 => LedState::Red,
        _ => return None,
    })
}

const CMD_GET_ESR_CAPABILITIES: u8 = 0xF5;
const CMD_GET_ESR_SEL_INFO: u8 = 0xF2;

/// The `CISCO` signature every ESR capabilities response starts with.
const ESR_SIGNATURE: [u8; 5] = *b"CISCO";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EsrCapabilities {
    /// Whether the controller actually has ESR turned on. The commands must
    /// not be used when this is clear, even though they answered.
    pub enabled: bool,
    pub api_version: u8,
    pub doc_version_major: u8,
    pub doc_version_minor: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EsrSelInfo {
    pub entries: u32,
    pub free_bytes: u32,
    pub overflow: bool,
}

/// Ask the BMC whether it implements ESR.
///
/// `Ok(None)` means the controller answered but is not an ESR implementation,
/// which is a normal outcome and not an error.
pub fn esr_capabilities(conn: &mut Conn) -> Result<Option<EsrCapabilities>, String> {
    let response = conn
        .send_raw(NetFn::Storage, CMD_GET_ESR_CAPABILITIES, Vec::new())
        .map_err(|error| format!("Get ESR Capabilities failed: {error}"))?;
    if response.cc() != 0 {
        // An unsupported command is the expected answer on most controllers.
        return Ok(None);
    }
    Ok(parse_esr_capabilities(response.data()))
}

/// Read the size and fullness of the extended SEL repository.
pub fn esr_sel_info(conn: &mut Conn) -> Result<EsrSelInfo, String> {
    let response = conn
        .send_raw(NetFn::Storage, CMD_GET_ESR_SEL_INFO, Vec::new())
        .map_err(|error| format!("Get Cisco Extended SEL Info failed: {error}"))?;
    if response.cc() != 0 {
        return Err(format!(
            "Get Cisco Extended SEL Info: completion code 0x{:02X}",
            response.cc()
        ));
    }
    parse_esr_sel_info(response.data())
}

/// Parse the Get ESR Capabilities response body (the bytes after the
/// completion code).
///
/// Cisco documents 36 bytes, of which everything past the version fields is
/// reserved. Controllers that answer with the signature but a short tail are
/// still usable, so only the fields we read are required.
fn parse_esr_capabilities(data: &[u8]) -> Option<EsrCapabilities> {
    if data.len() < 9 || data[..5] != ESR_SIGNATURE {
        return None;
    }
    Some(EsrCapabilities {
        enabled: data[5] & 0x01 != 0,
        api_version: data[6],
        doc_version_minor: data[7],
        doc_version_major: data[8],
    })
}

fn parse_esr_sel_info(data: &[u8]) -> Result<EsrSelInfo, String> {
    // Counts, then the add and erase timestamps. The flags byte after them is
    // optional here: firmware that omits it still reports usable counts.
    if data.len() < 16 {
        return Err(format!(
            "Get Cisco Extended SEL Info: short response ({} bytes)",
            data.len()
        ));
    }
    Ok(EsrSelInfo {
        entries: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        free_bytes: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        // The flags byte is documented as byte 18 of the response, one past the
        // erase timestamp. Older firmware stops at the timestamps.
        overflow: data.get(16).is_some_and(|flags| flags & 0x80 != 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities_response(flags: u8) -> Vec<u8> {
        let mut data = b"CISCO".to_vec();
        data.push(flags);
        data.extend_from_slice(&[1, 2, 3]); // API version, doc minor, doc major
        data.extend_from_slice(&[0; 27]); // reserved
        data
    }

    #[test]
    fn decodes_documented_led_offsets() {
        assert_eq!(led_state(0x00).map(LedState::label), Some("LED is off"));
        assert_eq!(led_state(0x01).map(LedState::label), Some("LED is on"));
        assert_eq!(
            led_state(0x02).map(LedState::label),
            Some("LED is blinking")
        );
        assert_eq!(
            led_state(0x04).map(LedState::label),
            Some("LED color is green")
        );
        assert_eq!(
            led_state(0x05).map(LedState::label),
            Some("LED color is amber")
        );
        assert_eq!(
            led_state(0x07).map(LedState::label),
            Some("LED color is red")
        );
        // Undecoded offsets, including the colour 06h this hardware uses.
        for offset in [0x03, 0x06, 0x08, 0x0F] {
            assert_eq!(led_state(offset), None, "offset {offset:#04X}");
        }

        // Lit state and colour are separate fields of one sensor.
        assert_eq!(LedState::Off.lit(), Some(false));
        assert_eq!(LedState::On.lit(), Some(true));
        assert_eq!(LedState::Blinking.lit(), Some(true));
        assert_eq!(LedState::Amber.lit(), None);
        assert_eq!(LedState::Red.lit(), None);
    }

    #[test]
    fn parses_esr_capabilities() {
        let capabilities = parse_esr_capabilities(&capabilities_response(0x01)).unwrap();
        assert!(capabilities.enabled);
        assert_eq!(capabilities.api_version, 1);
        assert_eq!(capabilities.doc_version_minor, 2);
        assert_eq!(capabilities.doc_version_major, 3);

        // Answering the command is not the same as having ESR switched on.
        let disabled = parse_esr_capabilities(&capabilities_response(0x00)).unwrap();
        assert!(!disabled.enabled);
    }

    #[test]
    fn rejects_responses_without_the_cisco_signature() {
        let mut wrong = capabilities_response(0x01);
        wrong[0] = b'X';
        assert!(parse_esr_capabilities(&wrong).is_none());
        assert!(parse_esr_capabilities(&[]).is_none());
        assert!(parse_esr_capabilities(b"CISC").is_none());
    }

    #[test]
    fn parses_esr_sel_info() {
        let mut data = Vec::new();
        data.extend_from_slice(&300u32.to_le_bytes()); // total entries
        data.extend_from_slice(&4096u32.to_le_bytes()); // free space
        data.extend_from_slice(&[0; 4]); // add timestamp
        data.extend_from_slice(&[0; 4]); // erase timestamp
        data.push(0x80); // overflow

        let info = parse_esr_sel_info(&data).unwrap();
        assert_eq!(info.entries, 300);
        assert_eq!(info.free_bytes, 4096);
        assert!(info.overflow);

        // Firmware that stops after the timestamps still reports the counts.
        let short = parse_esr_sel_info(&data[..16]).unwrap();
        assert_eq!(short.entries, 300);
        assert!(!short.overflow);

        assert!(parse_esr_sel_info(&data[..8]).is_err());
    }
}
