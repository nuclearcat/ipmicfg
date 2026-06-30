//! `sel` — System Event Log: list, summarize, clear.

use std::fmt;

use ipmi_rs::storage::sdr::SensorType;
use ipmi_rs::storage::sel::{
    ClearSel, Entry, ErasureProgress, GetSelEntry, GetSelInfo, RecordId, ReserveSel, SelCommand,
};

use crate::cli::{SelAction, SelArgs};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Table};

pub fn run(conn: &mut Conn, args: &SelArgs) -> Result<(), String> {
    match args.action.as_ref().unwrap_or(&SelAction::List) {
        SelAction::List => list(conn, args.raw),
        SelAction::Info => info(conn),
        SelAction::Clear { yes } => clear(conn, *yes),
    }
}

fn info(conn: &mut Conn) -> Result<(), String> {
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;
    ui::header("SEL Information");
    ui::kv(
        "Version",
        &format!("{}.{}", info.version_maj, info.version_min),
        14,
    );
    ui::kv("Entries", &info.entries.to_string(), 14);
    ui::kv("Free space", &format!("{} bytes", info.bytes_free), 14);
    if info.overflow {
        ui::kv("Overflow", &ui::red("yes — events were lost"), 14);
    }
    println!();
    Ok(())
}

fn list(conn: &mut Conn, raw: bool) -> Result<(), String> {
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;

    ui::header(&format!("System Event Log ({} entries)", info.entries));
    if info.overflow {
        println!("  {}", ui::red("! SEL overflow flag set — some events were lost"));
    }
    if info.entries == 0 {
        println!("  {}", ui::dim("log is empty"));
        println!();
        return Ok(());
    }

    let mut entries = Vec::new();
    let mut record_id = RecordId::FIRST;
    loop {
        let entry_info = match conn.send_recv(GetSelEntry::new(None, record_id)) {
            Ok(e) => e,
            Err(e) => {
                println!("  {}", ui::yellow(&format!("stopped reading SEL: {e:?}")));
                break;
            }
        };

        let next = entry_info.next_entry;
        entries.push(entry_info.entry);

        if next.is_last() {
            break;
        }
        record_id = next;
    }

    let mut table = Table::new(
        &["ID", "TIMESTAMP", "SENSOR", "EVENT"],
        &[Align::Right, Align::Left, Align::Left, Align::Left],
    );

    let mut i = 0;
    while i < entries.len() {
        // Collapse a run of multi-part OEM text records into one row, unless
        // the user asked for the raw per-record hex dump.
        if !raw {
            if let Some(group) = oem_text_group(&entries[i..]) {
                let label = if group.count > 1 {
                    format!("OEM 0x{:02X} ×{}", group.ty, group.count)
                } else {
                    format!("OEM 0x{:02X}", group.ty)
                };
                let text = format!("\"{}\"", group.text);
                table.row(vec![
                    Cell::new(format!("0x{:04X}", group.first_id.value())),
                    Cell::new(ui::dim("—")),
                    Cell::new(label),
                    Cell::colored(text.clone(), ui::cyan(&text)),
                ]);
                i += group.count;
                continue;
            }
        }
        push_entry(&mut table, &entries[i]);
        i += 1;
    }

    table.print();
    println!();
    Ok(())
}

/// A run of consecutive non-timestamped OEM records reassembled into text.
struct OemText {
    first_id: RecordId,
    ty: u8,
    count: usize,
    text: String,
}

/// Detect a multi-part OEM text message at the start of `entries`.
///
/// The shape (inferred, not spec'd): each non-timestamped OEM record carries a
/// constant tag in byte 0, a chunk sequence number in byte 1 (starting at 0 and
/// incrementing), and up to 11 ASCII text bytes in bytes 2..13, NUL-padded in
/// the final chunk. We only collapse a run when it parses cleanly into printable
/// text; anything else falls back to the per-record hex dump.
fn oem_text_group(entries: &[Entry]) -> Option<OemText> {
    let (first_id, tag, ty) = match entries.first()? {
        Entry::OemNotTimestamped {
            record_id,
            ty,
            data,
        } if data[1] == 0 => (*record_id, data[0], *ty),
        _ => return None,
    };

    let mut bytes = Vec::new();
    let mut count = 0usize;
    for entry in entries {
        match entry {
            Entry::OemNotTimestamped { ty: t, data, .. }
                if *t == ty && data[0] == tag && data[1] as usize == count =>
            {
                bytes.extend_from_slice(&data[2..]);
                count += 1;
            }
            _ => break,
        }
    }

    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.is_empty() || !bytes.iter().all(|&b| (0x20..0x7F).contains(&b)) {
        return None;
    }

    Some(OemText {
        first_id,
        ty,
        count,
        text: String::from_utf8(bytes).ok()?,
    })
}

fn push_entry(table: &mut Table, entry: &Entry) {
    match entry {
        Entry::System {
            record_id,
            timestamp,
            sensor_type,
            sensor_number,
            ..
        } => {
            let sensor = format!("{} #{}", SensorType::from(*sensor_type), sensor_number);
            let desc = Desc(entry).to_string();
            let desc = if desc.is_empty() {
                ui::dim("(no description)")
            } else {
                desc
            };
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(timestamp.to_string()),
                Cell::new(sensor),
                Cell::colored(strip(&desc), desc),
            ]);
        }
        Entry::OemTimestamped {
            record_id,
            ty,
            timestamp,
            manufacturer_id,
            data,
        } => {
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(timestamp.to_string()),
                Cell::new(format!("OEM 0x{ty:02X}")),
                Cell::new(format!(
                    "mfg 0x{manufacturer_id:06X}  {}",
                    oem_dump(data)
                )),
            ]);
        }
        Entry::OemNotTimestamped {
            record_id,
            ty,
            data,
        } => {
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(ui::dim("—")),
                Cell::new(format!("OEM 0x{ty:02X}")),
                Cell::new(oem_dump(data)),
            ]);
        }
    }
}

fn clear(conn: &mut Conn, yes: bool) -> Result<(), String> {
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;

    // Note: there is no capability bit for the Clear SEL command (0x47) — it is
    // mandatory for any SEL device. The Operation Support bit the library calls
    // `Clear` is really "Delete SEL Entry supported" (the optional per-entry
    // delete, 0x44), so we must not gate Clear on it.
    //
    // TODO(ipmi-rs#54): upstream renames the mislabeled `Command::Clear` variant
    // (https://github.com/datdenkikniet/ipmi-rs/pull/54). Once we bump the pinned
    // rev past that merge, drop the stale `SelCommand::Clear` variant references.

    if !yes && !confirm(&format!("Erase all {} SEL entries?", info.entries)) {
        println!("Aborted.");
        return Ok(());
    }

    // Clear SEL takes a reservation ID; when the device has no reservation
    // mechanism, 0x0000 (None) is valid per IPMI 2.0 §31.9.
    let reservation = if info.supported_cmds.contains(&SelCommand::Reserve) {
        Some(
            conn.send_recv(ReserveSel)
                .map_err(|e| format!("Reserve SEL failed: {e:?}"))?,
        )
    } else {
        None
    };

    let mut progress = conn
        .send_recv(ClearSel::initiate(reservation))
        .map_err(|e| format!("Clear SEL failed: {e:?}"))?;

    while matches!(progress, ErasureProgress::InProgress) {
        std::thread::sleep(std::time::Duration::from_millis(150));
        progress = conn
            .send_recv(ClearSel::get_status(reservation))
            .map_err(|e| format!("Clear SEL status failed: {e:?}"))?;
    }

    println!("{}", ui::green("SEL cleared."));
    Ok(())
}

/// Render raw OEM SEL bytes as `hex  |ascii|`.
///
/// The IPMI spec defines no structure for OEM record payloads (record types
/// 0xC0–0xFF), so the bytes are vendor-specific and can't be decoded
/// generically. Dumping hex alongside a printable-ASCII gutter is the most
/// useful vendor-agnostic view: many BMCs pack short text (e.g. panic context
/// appended to an OS critical-stop event) into these records.
fn oem_dump(data: &[u8]) -> String {
    let hex = data
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii: String = data
        .iter()
        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
        .collect();
    format!("{hex}  |{ascii}|")
}

/// Display wrapper that delegates to `Entry::event_description`.
struct Desc<'a>(&'a Entry);

impl fmt::Display for Desc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.event_description(f)
    }
}

/// Best-effort removal of the trailing reset escape so the plain-width estimate
/// for colored cells stays close to reality. `ui::dim` is the only colorizer we
/// feed through `Cell::colored` here, and width over-estimates are harmless.
fn strip(s: &str) -> String {
    s.replace("\x1b[2m", "").replace("\x1b[0m", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oem(id: u16, data: [u8; 13]) -> Entry {
        Entry::OemNotTimestamped {
            record_id: RecordId::new(id).expect("valid record id"),
            ty: 0xF0,
            data,
        }
    }

    #[test]
    fn reassembles_license_message() {
        // Exact bytes captured from the BMC SEL.
        let entries = vec![
            oem(0x00FE, *b"\x20\x00License exp"),
            oem(0x00FF, *b"\x20\x01ired, shutt"),
            oem(0x0100, *b"\x20\x02ing down\x00\x00\x00"),
        ];
        let g = oem_text_group(&entries).expect("should group");
        assert_eq!(g.count, 3);
        assert_eq!(g.ty, 0xF0);
        assert_eq!(g.first_id.value(), 0x00FE);
        assert_eq!(g.text, "License expired, shutting down");
    }

    #[test]
    fn requires_sequence_to_start_at_zero() {
        let entries = vec![oem(0x0001, *b"\x20\x01ired, shutt")];
        assert!(oem_text_group(&entries).is_none());
    }

    #[test]
    fn rejects_binary_payload() {
        let entries = vec![oem(0x0001, [0x20, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7])];
        assert!(oem_text_group(&entries).is_none());
    }

    #[test]
    fn stops_at_sequence_gap() {
        // seq 0 then seq 2 — only the first chunk should be consumed.
        let entries = vec![
            oem(0x0001, *b"\x20\x00abcdefghijk"),
            oem(0x0002, *b"\x20\x02xxxxxxxxxxx"),
        ];
        let g = oem_text_group(&entries).expect("first chunk groups alone");
        assert_eq!(g.count, 1);
        assert_eq!(g.text, "abcdefghijk");
    }
}
