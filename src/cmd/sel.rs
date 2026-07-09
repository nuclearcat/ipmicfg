//! `sel` — System Event Log: list, summarize, clear.

use std::collections::{HashMap, HashSet};
use std::fmt;

use ipmi_rs::connection::NetFn;
use ipmi_rs::storage::sdr::record::{IdentifiableSensor, InstancedSensor, RecordContents};
use ipmi_rs::storage::sdr::SensorType;
use ipmi_rs::storage::sel::{
    ClearSel, Entry, ErasureProgress, EventDirection, GetSelEntry, GetSelInfo, RecordId,
    ReserveSel, SelCommand,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::{SelAction, SelArgs, SelSeverity};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Table};

pub fn run(conn: &mut Conn, args: &SelArgs) -> Result<(), String> {
    match args.action.as_ref().unwrap_or(&SelAction::List) {
        SelAction::List => list(conn, args),
        SelAction::Info => info(conn),
        SelAction::Clear { yes } => clear(conn, *yes),
        SelAction::Delete { record_id, yes } => delete(conn, *record_id, *yes),
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

fn list(conn: &mut Conn, args: &SelArgs) -> Result<(), String> {
    let since = args.since.as_deref().map(parse_time).transpose()?;
    let until = args.until.as_deref().map(parse_time).transpose()?;
    if matches!((since, until), (Some(start), Some(end)) if start > end) {
        return Err("--since must not be later than --until".to_string());
    }

    let names = sensor_names(conn)?;
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;
    let entries = if info.entries == 0 {
        Vec::new()
    } else {
        read_entries(conn)?
    };
    let mut known: HashSet<u16> = entries
        .iter()
        .map(|entry| entry_id(entry).value())
        .collect();
    print_entries(
        entries,
        args,
        &names,
        since,
        until,
        Some((info.entries, info.overflow)),
    );

    if !args.follow {
        return Ok(());
    }
    println!("  {}", ui::dim("following SEL; press Ctrl-C to stop"));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let current_info = conn
            .send_recv(GetSelInfo)
            .map_err(|e| format!("Get SEL Info failed while following: {e:?}"))?;
        let current = if current_info.entries == 0 {
            Vec::new()
        } else {
            read_entries(conn)?
        };
        if current.len() < known.len() {
            known.clear();
        }
        let fresh = current
            .into_iter()
            .filter(|entry| known.insert(entry_id(entry).value()))
            .collect::<Vec<_>>();
        if !fresh.is_empty() {
            print_entries(fresh, args, &names, since, until, None);
        }
    }
}

fn read_entries(conn: &mut Conn) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut record_id = RecordId::FIRST;
    let mut seen = HashSet::new();
    loop {
        let entry_info = conn
            .send_recv(GetSelEntry::new(None, record_id))
            .map_err(|e| format!("Get SEL Entry 0x{:04X} failed: {e:?}", record_id.value()))?;
        let next = entry_info.next_entry;
        entries.push(entry_info.entry);
        if next.is_last() {
            break;
        }
        if !seen.insert(next.value()) {
            return Err(format!(
                "SEL pagination loop detected at record 0x{:04X}",
                next.value()
            ));
        }
        record_id = next;
    }
    Ok(entries)
}

fn print_entries(
    entries: Vec<Entry>,
    args: &SelArgs,
    names: &HashMap<u8, String>,
    since: Option<i64>,
    until: Option<i64>,
    heading: Option<(u16, bool)>,
) {
    if let Some((total, overflow)) = heading {
        ui::header(&format!("System Event Log ({total} entries)"));
        if overflow {
            println!(
                "  {}",
                ui::red("! SEL overflow flag set — some events were lost")
            );
        }
    } else {
        ui::header("New SEL entries");
    }

    let mut entries = entries
        .into_iter()
        .filter(|entry| matches_entry(entry, args, names, since, until))
        .collect::<Vec<_>>();
    if let Some(limit) = args.limit {
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
    }
    if entries.is_empty() {
        println!("  {}", ui::dim("no entries matched"));
        println!();
        return;
    }

    let mut table = Table::new(
        &["ID", "TIMESTAMP", "SEVERITY", "SENSOR", "EVENT"],
        &[
            Align::Right,
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Left,
        ],
    );

    let mut i = 0;
    while i < entries.len() {
        // Collapse a run of multi-part OEM text records into one row, unless
        // the user asked for the raw per-record hex dump.
        if !args.raw {
            if let Some(group) = oem_text_group(&entries[i..]) {
                let label = if group.count > 1 {
                    format!("OEM 0x{:02X} ×{}", group.ty, group.count)
                } else {
                    format!("OEM 0x{:02X}", group.ty)
                };
                let text = format!("\"{}\"", group.text);
                let (severity, rendered_severity) = severity_label(SelSeverity::Warning);
                table.row(vec![
                    Cell::new(format!("0x{:04X}", group.first_id.value())),
                    Cell::new(ui::dim("—")),
                    Cell::colored(severity, rendered_severity),
                    Cell::new(label),
                    Cell::colored(text.clone(), ui::cyan(&text)),
                ]);
                i += group.count;
                continue;
            }
        }
        push_entry(&mut table, &entries[i], names);
        i += 1;
    }

    table.print();
    println!();
}

fn sensor_names(conn: &mut Conn) -> Result<HashMap<u8, String>, String> {
    Ok(conn
        .collect_sdrs()?
        .iter()
        .filter_map(|record| match &record.contents {
            RecordContents::FullSensor(sensor) => Some((
                sensor.key_data().sensor_number.get(),
                sensor.id_string().to_string(),
            )),
            RecordContents::CompactSensor(sensor) => Some((
                sensor.key_data().sensor_number.get(),
                sensor.id_string().to_string(),
            )),
            RecordContents::EventOnlySensor(sensor) => {
                Some((sensor.key.sensor_number.get(), sensor.id_string.to_string()))
            }
            _ => None,
        })
        .collect())
}

fn matches_entry(
    entry: &Entry,
    args: &SelArgs,
    names: &HashMap<u8, String>,
    since: Option<i64>,
    until: Option<i64>,
) -> bool {
    if since.is_some() || until.is_some() {
        let Some(timestamp) = entry_timestamp(entry) else {
            return false;
        };
        if since.is_some_and(|value| timestamp < value)
            || until.is_some_and(|value| timestamp > value)
        {
            return false;
        }
    }
    if let Some(pattern) = &args.sensor {
        let haystack = entry_sensor(entry, names);
        if !haystack
            .to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
        {
            return false;
        }
    }
    args.severity
        .is_none_or(|severity| entry_severity(entry) == severity)
}

fn parse_time(value: &str) -> Result<i64, String> {
    if let Ok(unix) = value.parse::<i64>() {
        return Ok(unix);
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|time| time.unix_timestamp())
        .map_err(|_| format!("invalid time '{value}'; use RFC 3339 or a Unix timestamp"))
}

fn entry_timestamp(entry: &Entry) -> Option<i64> {
    let rendered = match entry {
        Entry::System { timestamp, .. } | Entry::OemTimestamped { timestamp, .. } => {
            timestamp.to_string()
        }
        Entry::OemNotTimestamped { .. } => return None,
    };
    if rendered == "Unknown" {
        None
    } else if let Ok(unix) = rendered.parse::<i64>() {
        Some(unix)
    } else {
        OffsetDateTime::parse(&rendered, &Rfc3339)
            .ok()
            .map(|time| time.unix_timestamp())
    }
}

fn entry_id(entry: &Entry) -> RecordId {
    match entry {
        Entry::System { record_id, .. }
        | Entry::OemTimestamped { record_id, .. }
        | Entry::OemNotTimestamped { record_id, .. } => *record_id,
    }
}

fn entry_sensor(entry: &Entry, names: &HashMap<u8, String>) -> String {
    match entry {
        Entry::System {
            sensor_type,
            sensor_number,
            ..
        } => {
            let ty = SensorType::from(*sensor_type);
            match names.get(sensor_number) {
                Some(name) => format!("{name} ({ty} #{sensor_number})"),
                None => format!("{ty} #{sensor_number}"),
            }
        }
        Entry::OemTimestamped { ty, .. } | Entry::OemNotTimestamped { ty, .. } => {
            format!("OEM 0x{ty:02X}")
        }
    }
}

/// Infer severity because SEL records do not contain a standard severity field.
pub fn entry_severity(entry: &Entry) -> SelSeverity {
    let Entry::System {
        event_direction,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return SelSeverity::Warning;
    };
    if *event_direction == EventDirection::Deassert {
        return SelSeverity::Normal;
    }
    if *event_type == 0x01 {
        return match event_data.offset {
            0 | 1 | 6 | 7 => SelSeverity::Warning,
            2..=5 | 8..=11 => SelSeverity::Critical,
            _ => SelSeverity::Warning,
        };
    }

    let description = entry_description(entry).to_ascii_lowercase();
    if [
        "failure",
        "failed",
        "fault",
        "critical",
        "non-recoverable",
        "lost",
        "thermal trip",
        "uncorrectable",
        "limit exceeded",
    ]
    .iter()
    .any(|word| description.contains(word))
    {
        SelSeverity::Critical
    } else if ["present", "enabled", "fully redundant", "power on"]
        .iter()
        .any(|word| description.contains(word))
    {
        SelSeverity::Normal
    } else {
        SelSeverity::Warning
    }
}

fn severity_label(severity: SelSeverity) -> (&'static str, String) {
    match severity {
        SelSeverity::Normal => ("normal", ui::green("normal")),
        SelSeverity::Warning => ("warning", ui::yellow("warning")),
        SelSeverity::Critical => ("critical", ui::red("critical")),
    }
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

fn push_entry(table: &mut Table, entry: &Entry, names: &HashMap<u8, String>) {
    match entry {
        Entry::System {
            record_id,
            timestamp,
            ..
        } => {
            let sensor = entry_sensor(entry, names);
            let desc = entry_description(entry);
            let desc = if desc.is_empty() {
                ui::dim("(no description)")
            } else {
                desc
            };
            let (severity, rendered_severity) = severity_label(entry_severity(entry));
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(timestamp.to_string()),
                Cell::colored(severity, rendered_severity),
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
            let (severity, rendered_severity) = severity_label(entry_severity(entry));
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(timestamp.to_string()),
                Cell::colored(severity, rendered_severity),
                Cell::new(format!("OEM 0x{ty:02X}")),
                Cell::new(format!("mfg 0x{manufacturer_id:06X}  {}", oem_dump(data))),
            ]);
        }
        Entry::OemNotTimestamped {
            record_id,
            ty,
            data,
        } => {
            let (severity, rendered_severity) = severity_label(entry_severity(entry));
            table.row(vec![
                Cell::new(format!("0x{:04X}", record_id.value())),
                Cell::new(ui::dim("—")),
                Cell::colored(severity, rendered_severity),
                Cell::new(format!("OEM 0x{ty:02X}")),
                Cell::new(oem_dump(data)),
            ]);
        }
    }
}

pub fn entry_description(entry: &Entry) -> String {
    Desc(entry).to_string()
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

fn delete(conn: &mut Conn, record_id: u16, yes: bool) -> Result<(), String> {
    let record = RecordId::new(record_id).ok_or_else(|| {
        "record ID must be between 0x0001 and 0xFFFE (FIRST/LAST are selectors)".to_string()
    })?;
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;
    if !info.supported_cmds.contains(&SelCommand::Delete) {
        return Err("this BMC does not advertise per-entry SEL deletion".to_string());
    }
    if !yes && !confirm(&format!("Delete SEL entry 0x{:04X}?", record.value())) {
        println!("Aborted.");
        return Ok(());
    }

    let reservation = if info.supported_cmds.contains(&SelCommand::Reserve) {
        conn.send_recv(ReserveSel)
            .map_err(|e| format!("Reserve SEL failed: {e:?}"))?
            .get()
    } else {
        0
    };
    let mut data = Vec::with_capacity(4);
    data.extend_from_slice(&reservation.to_le_bytes());
    data.extend_from_slice(&record.value().to_le_bytes());
    let response = conn
        .send_raw(NetFn::Storage, 0x46, data)
        .map_err(|e| format!("Delete SEL Entry failed: {e}"))?;
    if response.cc() != 0 {
        return Err(format!(
            "Delete SEL Entry rejected: completion code 0x{:02X}",
            response.cc()
        ));
    }
    println!(
        "{} entry 0x{:04X} deleted",
        ui::green("OK:"),
        record.value()
    );
    Ok(())
}

pub struct HealthSummary {
    pub entries: u16,
    pub overflow: bool,
    pub critical: usize,
    pub recent_critical: Vec<String>,
}

/// Read a compact SEL health summary for the top-level `status` command.
pub fn health_summary(conn: &mut Conn, recent_limit: usize) -> Result<HealthSummary, String> {
    let info = conn
        .send_recv(GetSelInfo)
        .map_err(|e| format!("Get SEL Info failed: {e:?}"))?;
    let entries = if info.entries == 0 {
        Vec::new()
    } else {
        read_entries(conn)?
    };
    let critical = entries
        .iter()
        .filter(|entry| entry_severity(entry) == SelSeverity::Critical)
        .count();
    let mut recent_critical = entries
        .iter()
        .rev()
        .filter(|entry| entry_severity(entry) == SelSeverity::Critical)
        .take(recent_limit)
        .map(|entry| {
            let description = entry_description(entry);
            if description.is_empty() {
                format!("entry 0x{:04X}", entry_id(entry).value())
            } else {
                format!("0x{:04X}: {description}", entry_id(entry).value())
            }
        })
        .collect::<Vec<_>>();
    recent_critical.reverse();
    Ok(HealthSummary {
        entries: info.entries,
        overflow: info.overflow,
        critical,
        recent_critical,
    })
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
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
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
    use ipmi_rs::connection::Channel;
    use ipmi_rs::storage::sdr::EventData;
    use ipmi_rs::storage::sel::{EventGenerator, EventMessageRevision};

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
        let entries = vec![oem(
            0x0001,
            [0x20, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7],
        )];
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

    fn system_event(direction: EventDirection, event_type: u8, offset: u8) -> Entry {
        Entry::System {
            record_id: RecordId::new(1).expect("valid record id"),
            timestamp: 1_700_000_000.into(),
            generator_id: EventGenerator::SoftwareId {
                software_id: 1,
                channel_number: Channel::Primary,
            },
            event_message_format: EventMessageRevision::V2_0,
            sensor_type: 0x01,
            sensor_number: 1,
            event_direction: direction,
            event_type,
            event_data: EventData::parse(&[offset, 0xFF, 0xFF]),
        }
    }

    #[test]
    fn infers_threshold_severity() {
        assert_eq!(
            entry_severity(&system_event(EventDirection::Assert, 0x01, 0)),
            SelSeverity::Warning
        );
        assert_eq!(
            entry_severity(&system_event(EventDirection::Assert, 0x01, 2)),
            SelSeverity::Critical
        );
        assert_eq!(
            entry_severity(&system_event(EventDirection::Deassert, 0x01, 2)),
            SelSeverity::Normal
        );
    }

    #[test]
    fn parses_rfc3339_and_unix_times() {
        assert_eq!(parse_time("1700000000"), Ok(1_700_000_000));
        assert_eq!(parse_time("2023-11-14T22:13:20Z"), Ok(1_700_000_000));
        assert!(parse_time("last Tuesday").is_err());
    }
}
