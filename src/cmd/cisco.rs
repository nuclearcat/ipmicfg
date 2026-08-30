//! Cisco CIMC Extended Sensor Range (ESR) helpers.
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
