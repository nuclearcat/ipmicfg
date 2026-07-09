//! Fujitsu iRMC OEM protocol helpers.
//!
//! Implements `F5 43 - Get SEL entry long text` from the iRMC S4
//! "Concepts and Interfaces" guide.  This asks the controller firmware to
//! translate its own vendor events instead of maintaining an incomplete table
//! of OEM event bytes in the client.

use ipmi_rs::connection::NetFn;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cmd::raw::hex_bytes;
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Table};

const NETFN_OEM_GROUP: u8 = 0x2E;
const CMD_IRMC: u8 = 0xF5;
const FUJITSU_IANA_LE: [u8; 3] = [0x80, 0x28, 0x00];
const GET_SEL_ENTRY_LONG_TEXT: u8 = 0x43;

// Documented maximum on Pilot-1 designs. It also keeps pagination exercised
// and avoids assuming that every local IPMI driver accepts a 100-byte chunk.
const MAX_CHUNK_LEN: u8 = 56;
const RESPONSE_HEADER_LEN: usize = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Informational,
    Minor,
    Major,
    Critical,
    Unknown,
}

impl Severity {
    fn from_byte(value: u8) -> Self {
        match (value >> 4) & 0x07 {
            0 => Self::Informational,
            1 => Self::Minor,
            2 => Self::Major,
            3 => Self::Critical,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> (&'static str, String) {
        match self {
            Self::Informational => ("informational", ui::green("informational")),
            Self::Minor => ("minor", ui::yellow("minor")),
            Self::Major => ("major", ui::red("major")),
            Self::Critical => ("critical", ui::red("critical")),
            Self::Unknown => ("unknown", ui::dim("unknown")),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ResponseChunk {
    next_record_id: u16,
    actual_record_id: u16,
    record_type: u8,
    timestamp: u32,
    severity_byte: u8,
    total_text_len: u8,
    text: Vec<u8>,
}

pub(crate) struct DecodedSelEntry {
    pub record_id: u16,
    pub record_type: u8,
    pub timestamp: u32,
    pub severity: Severity,
    pub css: bool,
    pub text: String,
}

pub fn decode_sel_entries(conn: &mut Conn, record_ids: &[u16], debug: bool) -> Result<(), String> {
    let mut decoded = Vec::with_capacity(record_ids.len());
    for &record_id in record_ids {
        decoded.push(fetch_long_text(conn, record_id, debug).map_err(|error| {
            format!("Fujitsu SEL decode for record 0x{record_id:04X} failed: {error}")
        })?);
    }

    ui::header("Fujitsu iRMC SEL Text");
    let mut table = Table::new(
        &["ID", "TIMESTAMP", "SEVERITY", "CSS", "TYPE", "EVENT"],
        &[
            Align::Right,
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Left,
        ],
    );
    for entry in decoded {
        let (severity, rendered_severity) = entry.severity.label();
        let css = if entry.css { "yes" } else { "no" };
        table.row(vec![
            Cell::new(format!("0x{:04X}", entry.record_id)),
            Cell::new(format_timestamp(entry.timestamp)),
            Cell::colored(severity, rendered_severity),
            Cell::new(css),
            Cell::new(format!("0x{:02X}", entry.record_type)),
            Cell::new(entry.text),
        ]);
    }
    table.print();
    println!();
    Ok(())
}

pub(crate) fn fetch_long_text(
    conn: &mut Conn,
    record_id: u16,
    debug: bool,
) -> Result<DecodedSelEntry, String> {
    let mut text = Vec::new();
    let mut first: Option<ResponseChunk> = None;

    loop {
        let offset = u8::try_from(text.len())
            .map_err(|_| "translated text exceeds the protocol's 255-byte offset".to_string())?;
        let requested = first
            .as_ref()
            .map(|chunk| {
                chunk
                    .total_text_len
                    .saturating_sub(offset)
                    .min(MAX_CHUNK_LEN)
            })
            .unwrap_or(MAX_CHUNK_LEN);
        let request = long_text_request(record_id, offset, requested);

        if debug {
            println!(
                "  request  netfn=0x{NETFN_OEM_GROUP:02X} cmd=0x{CMD_IRMC:02X} data={}",
                hex_bytes(&request)
            );
        }

        let response = conn
            .send_raw(NetFn::from(NETFN_OEM_GROUP), CMD_IRMC, request)
            .map_err(|e| format!("transport error: {e}"))?;

        if debug {
            println!(
                "  response cc=0x{:02X} data={}",
                response.cc(),
                hex_bytes(response.data())
            );
        }
        if response.cc() != 0 {
            let detail = match response.cc() {
                0xC1 => " (command not supported by this BMC)",
                0xC7 => " (invalid request length)",
                0xD4 => " (administrator privilege may be required)",
                _ => "",
            };
            return Err(format!("completion code 0x{:02X}{detail}", response.cc()));
        }

        let chunk = parse_long_text_response(response.data())?;
        validate_chunk(&chunk, first.as_ref(), record_id, requested, text.len())?;
        if chunk.text.is_empty() && text.len() < usize::from(chunk.total_text_len) {
            return Err("controller returned an empty text chunk before completion".to_string());
        }
        text.extend_from_slice(&chunk.text);

        let total_len = usize::from(chunk.total_text_len);
        if first.is_none() {
            first = Some(chunk);
        }
        if text.len() >= total_len {
            break;
        }
    }

    let first = first.ok_or_else(|| "controller returned no response".to_string())?;
    Ok(DecodedSelEntry {
        record_id: first.actual_record_id,
        record_type: first.record_type,
        timestamp: first.timestamp,
        severity: Severity::from_byte(first.severity_byte),
        css: first.severity_byte & 0x80 != 0,
        text: String::from_utf8_lossy(&text).into_owned(),
    })
}

fn long_text_request(record_id: u16, offset: u8, max_size: u8) -> Vec<u8> {
    let [record_lo, record_hi] = record_id.to_le_bytes();
    vec![
        FUJITSU_IANA_LE[0],
        FUJITSU_IANA_LE[1],
        FUJITSU_IANA_LE[2],
        GET_SEL_ENTRY_LONG_TEXT,
        record_lo,
        record_hi,
        offset,
        max_size,
    ]
}

fn parse_long_text_response(data: &[u8]) -> Result<ResponseChunk, String> {
    if data.len() < RESPONSE_HEADER_LEN + 1 {
        return Err(format!(
            "short response: expected at least {} data bytes, received {}",
            RESPONSE_HEADER_LEN + 1,
            data.len()
        ));
    }
    if data[..3] != FUJITSU_IANA_LE {
        return Err(format!(
            "unexpected enterprise ID: expected 80 28 00, received {}",
            hex_bytes(&data[..3])
        ));
    }

    let encoded_text = &data[RESPONSE_HEADER_LEN..];
    let terminator = encoded_text
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| "response text is missing its NUL terminator".to_string())?;

    Ok(ResponseChunk {
        next_record_id: u16::from_le_bytes([data[3], data[4]]),
        actual_record_id: u16::from_le_bytes([data[5], data[6]]),
        record_type: data[7],
        timestamp: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        severity_byte: data[12],
        total_text_len: data[13],
        text: encoded_text[..terminator].to_vec(),
    })
}

fn validate_chunk(
    chunk: &ResponseChunk,
    first: Option<&ResponseChunk>,
    requested_id: u16,
    requested_size: u8,
    accumulated_len: usize,
) -> Result<(), String> {
    if requested_id != 0x0000 && requested_id != 0xFFFF && chunk.actual_record_id != requested_id {
        return Err(format!(
            "controller returned record 0x{:04X} for requested record 0x{requested_id:04X}",
            chunk.actual_record_id
        ));
    }
    if chunk.text.len() > usize::from(requested_size) {
        return Err(format!(
            "controller returned {} text bytes after at most {requested_size} were requested",
            chunk.text.len()
        ));
    }
    if accumulated_len + chunk.text.len() > usize::from(chunk.total_text_len) {
        return Err("controller returned more text than its declared total length".to_string());
    }
    if let Some(first) = first {
        if chunk.total_text_len != first.total_text_len
            || chunk.actual_record_id != first.actual_record_id
            || chunk.record_type != first.record_type
            || chunk.timestamp != first.timestamp
            || chunk.severity_byte != first.severity_byte
        {
            return Err("controller changed SEL metadata during pagination".to_string());
        }
    }
    Ok(())
}

fn format_timestamp(timestamp: u32) -> String {
    match OffsetDateTime::from_unix_timestamp(i64::from(timestamp)) {
        Ok(value) => value
            .format(&Rfc3339)
            .unwrap_or_else(|_| timestamp.to_string()),
        Err(_) => timestamp.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &[u8], total_len: u8) -> Vec<u8> {
        let mut data = vec![
            0x80, 0x28, 0x00, // IANA
            0xA7, 0x00, // next ID
            0xA6, 0x00, // actual ID
            0x02, // record type
            0x10, 0x20, 0x30, 0x40, // timestamp
            0xA0, // CSS + major
            total_len,
        ];
        data.extend_from_slice(text);
        data.push(0);
        data
    }

    #[test]
    fn builds_f5_43_request_in_documented_byte_order() {
        assert_eq!(
            long_text_request(0x00A6, 0, 56),
            [0x80, 0x28, 0x00, 0x43, 0xA6, 0x00, 0x00, 0x38]
        );
    }

    #[test]
    fn parses_f5_43_response() {
        let parsed = parse_long_text_response(&response(b"Battery event", 13)).unwrap();
        assert_eq!(parsed.next_record_id, 0x00A7);
        assert_eq!(parsed.actual_record_id, 0x00A6);
        assert_eq!(parsed.record_type, 0x02);
        assert_eq!(parsed.timestamp, 0x4030_2010);
        assert_eq!(parsed.severity_byte, 0xA0);
        assert_eq!(parsed.total_text_len, 13);
        assert_eq!(parsed.text, b"Battery event");
        assert_eq!(Severity::from_byte(parsed.severity_byte), Severity::Major);
        assert_ne!(parsed.severity_byte & 0x80, 0);
    }

    #[test]
    fn rejects_wrong_iana_and_unterminated_text() {
        let mut wrong_iana = response(b"x", 1);
        wrong_iana[0] = 0;
        assert!(parse_long_text_response(&wrong_iana)
            .unwrap_err()
            .contains("unexpected enterprise ID"));

        let mut unterminated = response(b"x", 1);
        unterminated.pop();
        assert!(parse_long_text_response(&unterminated)
            .unwrap_err()
            .contains("NUL terminator"));
    }

    #[test]
    fn rejects_mismatched_record_and_overlong_chunk() {
        let parsed = parse_long_text_response(&response(b"abcd", 4)).unwrap();
        assert!(validate_chunk(&parsed, None, 0x00A5, 56, 0)
            .unwrap_err()
            .contains("returned record"));
        assert!(validate_chunk(&parsed, None, 0x00A6, 3, 0)
            .unwrap_err()
            .contains("at most 3"));
    }

    #[test]
    fn permits_first_and_last_record_selectors() {
        let parsed = parse_long_text_response(&response(b"abcd", 4)).unwrap();
        validate_chunk(&parsed, None, 0x0000, 56, 0).unwrap();
        validate_chunk(&parsed, None, 0xFFFF, 56, 0).unwrap();
    }
}
