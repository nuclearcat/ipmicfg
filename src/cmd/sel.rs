//! `sel` — System Event Log: list, summarize, clear.

use std::collections::{HashMap, HashSet};
use std::fmt;

use ipmi_rs::app::GetDeviceId;
use ipmi_rs::connection::NetFn;
use ipmi_rs::storage::sdr::record::{IdentifiableSensor, InstancedSensor, RecordContents};
use ipmi_rs::storage::sdr::{EventData2Type, EventData3Type, SensorType};
use ipmi_rs::storage::sel::{
    ClearSel, Entry, ErasureProgress, EventDirection, GetSelInfo, RecordId, ReserveSel, SelCommand,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::{SelAction, SelArgs, SelSeverity};
use crate::cmd::{confirm, fujitsu};
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Table};

type SensorNames = HashMap<(u8, u8), String>;
type OemDecodedEntries = HashMap<u16, fujitsu::DecodedSelEntry>;

const DELL_MANUFACTURER_IDS: [u32; 2] = [0x000028, 0x0002A2];

struct RenderContext<'a> {
    decoded: &'a OemDecodedEntries,
    manufacturer_id: Option<u32>,
    args: &'a SelArgs,
    names: &'a SensorNames,
    since: Option<i64>,
    until: Option<i64>,
}

pub fn run(conn: &mut Conn, args: &SelArgs) -> Result<(), String> {
    match args.action.as_ref().unwrap_or(&SelAction::List) {
        SelAction::List => list(conn, args),
        SelAction::Info => info(conn),
        SelAction::Clear { yes } => clear(conn, *yes),
        SelAction::Delete { record_id, yes } => delete(conn, *record_id, *yes),
        SelAction::Decode { record_ids, debug } => {
            fujitsu::decode_sel_entries(conn, record_ids, *debug)
        }
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
    let manufacturer_id = bmc_manufacturer_id(conn);
    let decode_fujitsu_oem =
        !args.no_oem_decode && manufacturer_id.is_some_and(fujitsu::is_fujitsu_manufacturer);
    let decoded = decode_oem_entries(conn, &entries, decode_fujitsu_oem);
    let mut known: HashSet<u16> = entries
        .iter()
        .map(|entry| entry_id(entry).value())
        .collect();
    let render = RenderContext {
        decoded: &decoded,
        manufacturer_id,
        args,
        names: &names,
        since,
        until,
    };
    print_entries(entries, &render, Some((info.entries, info.overflow)));

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
            let decoded = decode_oem_entries(conn, &fresh, decode_fujitsu_oem);
            let render = RenderContext {
                decoded: &decoded,
                manufacturer_id,
                args,
                names: &names,
                since,
                until,
            };
            print_entries(fresh, &render, None);
        }
    }
}

fn read_entries(conn: &mut Conn) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut record_id = RecordId::FIRST;
    let mut seen = HashSet::new();
    loop {
        let mut request = vec![0, 0]; // reservation ID 0: no reservation
        request.extend_from_slice(&record_id.value().to_le_bytes());
        request.extend_from_slice(&[0, 0xFF]); // offset 0, read the complete record
        let response = conn
            .send_raw(NetFn::Storage, 0x43, request)
            .map_err(|e| format!("Get SEL Entry 0x{:04X} failed: {e}", record_id.value()))?;
        if response.cc() != 0 {
            return Err(format!(
                "Get SEL Entry 0x{:04X}: completion code 0x{:02X}",
                record_id.value(),
                response.cc()
            ));
        }
        if response.data().len() < 18 {
            return Err(format!(
                "Get SEL Entry 0x{:04X}: short response ({} bytes)",
                record_id.value(),
                response.data().len()
            ));
        }

        let next_raw = u16::from_le_bytes([response.data()[0], response.data()[1]]);
        let next = record_id_from_raw(next_raw);
        let mut raw = [0u8; 16];
        raw.copy_from_slice(&response.data()[2..18]);
        if raw[2] == 0x02 {
            // IPMI 2.0 section 29.7 assigns bits 7:6 to Event Data 2 and
            // bits 5:4 to Event Data 3. ipmi-rs currently interprets those two
            // selectors in the opposite order, so normalize the selector bits
            // before asking it to parse the record. The event offset is intact.
            raw[13] = swap_event_data_selectors(raw[13]);
        }
        let entry = Entry::parse(&raw).map_err(|error| {
            format!(
                "Get SEL Entry 0x{:04X}: invalid record: {error:?}",
                record_id.value()
            )
        })?;
        entries.push(entry);
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

fn record_id_from_raw(value: u16) -> RecordId {
    match value {
        0x0000 => RecordId::FIRST,
        0xFFFF => RecordId::LAST,
        value => RecordId::new(value).expect("non-selector record ID"),
    }
}

fn swap_event_data_selectors(value: u8) -> u8 {
    (value & 0x0F) | ((value & 0xC0) >> 2) | ((value & 0x30) << 2)
}

fn print_entries(entries: Vec<Entry>, render: &RenderContext<'_>, heading: Option<(u16, bool)>) {
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
        .filter(|entry| {
            matches_entry(
                entry,
                render.decoded,
                render.manufacturer_id,
                render.args,
                render.names,
                render.since,
                render.until,
            )
        })
        .collect::<Vec<_>>();
    if let Some(limit) = render.args.limit {
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
        if !render.args.raw {
            if let Some(group) = oem_text_group(&entries[i..]) {
                let label = if group.count > 1 {
                    format!("OEM 0x{:02X} ×{}", group.ty, group.count)
                } else {
                    format!("OEM 0x{:02X}", group.ty)
                };
                let text = format!("\"{}\"", group.text);
                let inferred =
                    severity_from_description(&group.text).unwrap_or(SelSeverity::Unknown);
                let (severity, rendered_severity) = severity_label(inferred);
                table.row(vec![
                    Cell::new(format!("0x{:04X}", group.first_id.value())),
                    Cell::colored("—", ui::dim("—")),
                    Cell::colored(severity, rendered_severity),
                    Cell::new(label),
                    Cell::colored(text.clone(), ui::cyan(&text)),
                ]);
                i += group.count;
                continue;
            }
        }
        push_entry(
            &mut table,
            &entries[i],
            render.names,
            render.decoded,
            render.manufacturer_id,
        );
        i += 1;
    }

    table.print();
    println!();
}

fn sensor_names(conn: &mut Conn) -> Result<SensorNames, String> {
    Ok(conn
        .collect_sdrs()?
        .iter()
        .filter_map(|record| match &record.contents {
            RecordContents::FullSensor(sensor) => Some((
                ((*sensor.ty()).into(), sensor.key_data().sensor_number.get()),
                sensor.id_string().to_string(),
            )),
            RecordContents::CompactSensor(sensor) => Some((
                ((*sensor.ty()).into(), sensor.key_data().sensor_number.get()),
                sensor.id_string().to_string(),
            )),
            RecordContents::EventOnlySensor(sensor) => Some((
                (sensor.ty.into(), sensor.key.sensor_number.get()),
                sensor.id_string.to_string(),
            )),
            _ => None,
        })
        .collect())
}

fn bmc_manufacturer_id(conn: &mut Conn) -> Option<u32> {
    conn.send_recv(GetDeviceId)
        .ok()
        .map(|device| device.manufacturer_id)
}

/// Decode only records whose sensor or event type is in an OEM-defined range.
/// Standard IPMI records already have local descriptions and decoding every SEL
/// row would add unnecessary controller traffic.
fn should_decode_oem(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::System {
            sensor_type,
            event_type,
            ..
        } if *sensor_type >= 0xC0 || *event_type >= 0x70
    )
}

fn decode_oem_entries(conn: &mut Conn, entries: &[Entry], enabled: bool) -> OemDecodedEntries {
    if !enabled {
        return HashMap::new();
    }

    let mut decoded = HashMap::new();
    let mut had_success = false;
    for entry in entries.iter().filter(|entry| should_decode_oem(entry)) {
        let record_id = entry_id(entry).value();
        match fujitsu::fetch_long_text(conn, record_id, false) {
            Ok(value) => {
                decoded.insert(record_id, value);
                had_success = true;
            }
            // A Fujitsu-branded controller without F5 43 support should incur
            // only one failed probe. Once decoding has worked, an isolated bad
            // record falls back to the generic rendering without hiding later
            // records.
            Err(_) if !had_success => break,
            Err(_) => {}
        }
    }
    decoded
}

fn displayed_severity(
    entry: &Entry,
    decoded: &OemDecodedEntries,
    manufacturer_id: Option<u32>,
) -> SelSeverity {
    match decoded
        .get(&entry_id(entry).value())
        .map(|entry| entry.severity)
    {
        Some(fujitsu::Severity::Informational) => SelSeverity::Normal,
        Some(fujitsu::Severity::Minor) => SelSeverity::Warning,
        Some(fujitsu::Severity::Major | fujitsu::Severity::Critical) => SelSeverity::Critical,
        Some(fujitsu::Severity::Unknown) => SelSeverity::Unknown,
        None if dell_oem_diagnostic_description(entry, manufacturer_id).is_some() => {
            SelSeverity::Normal
        }
        None => entry_severity(entry),
    }
}

fn matches_entry(
    entry: &Entry,
    decoded: &OemDecodedEntries,
    manufacturer_id: Option<u32>,
    args: &SelArgs,
    names: &SensorNames,
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
        let mut haystack = entry_sensor(entry, names);
        if let Some(oem) = decoded.get(&entry_id(entry).value()) {
            haystack.push(' ');
            haystack.push_str(&oem.text);
        }
        if let Some(dell) = dell_oem_diagnostic_description(entry, manufacturer_id) {
            haystack.push(' ');
            haystack.push_str(&dell);
        }
        if !haystack
            .to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
        {
            return false;
        }
    }
    args.severity
        .is_none_or(|severity| displayed_severity(entry, decoded, manufacturer_id) == severity)
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

fn entry_sensor(entry: &Entry, names: &SensorNames) -> String {
    match entry {
        Entry::System {
            sensor_type,
            sensor_number,
            ..
        } => {
            let ty = SensorType::from(*sensor_type);
            match names.get(&(*sensor_type, *sensor_number)) {
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
        sensor_type,
        event_direction,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return SelSeverity::Unknown;
    };
    if *event_direction == EventDirection::Deassert {
        return SelSeverity::Normal;
    }
    if *sensor_type == 0x0F && *event_type == 0x6F {
        return match event_data.offset {
            0x02 => SelSeverity::Normal,
            0x00 | 0x01 => SelSeverity::Critical,
            _ => SelSeverity::Unknown,
        };
    }
    if *sensor_type == 0x09 && *event_type == 0x6F && event_data.offset == 0x00 {
        // Power-off/down is an operational state transition. It commonly
        // occurs as an asserted/deasserted pair during a normal restart and is
        // not, by itself, a power fault.
        return SelSeverity::Normal;
    }
    if *event_type == 0x01 {
        return match event_data.offset {
            0 | 1 | 6 | 7 => SelSeverity::Warning,
            2..=5 | 8..=11 => SelSeverity::Critical,
            _ => SelSeverity::Unknown,
        };
    }

    let description = entry_description(entry).to_ascii_lowercase();
    if description.is_empty() {
        return SelSeverity::Unknown;
    }
    severity_from_description(&description).unwrap_or(SelSeverity::Warning)
}

fn severity_from_description(description: &str) -> Option<SelSeverity> {
    let description = description.to_ascii_lowercase();
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
        "softlockup",
        "hung task",
        "kernel panic",
        "fatal",
    ]
    .iter()
    .any(|word| description.contains(word))
    {
        Some(SelSeverity::Critical)
    } else if [
        "present",
        "enabled",
        "fully redundant",
        "power on",
        "running",
        "recovered",
        "restored",
    ]
    .iter()
    .any(|word| description.contains(word))
    {
        Some(SelSeverity::Normal)
    } else if ["warning", "degraded", "predictive"]
        .iter()
        .any(|word| description.contains(word))
    {
        Some(SelSeverity::Warning)
    } else {
        None
    }
}

fn severity_label(severity: SelSeverity) -> (&'static str, String) {
    match severity {
        SelSeverity::Normal => ("normal", ui::green("normal")),
        SelSeverity::Warning => ("warning", ui::yellow("warning")),
        SelSeverity::Critical => ("critical", ui::red("critical")),
        SelSeverity::Unknown => ("unknown", ui::dim("unknown")),
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

fn push_entry(
    table: &mut Table,
    entry: &Entry,
    names: &SensorNames,
    decoded: &OemDecodedEntries,
    manufacturer_id: Option<u32>,
) {
    match entry {
        Entry::System {
            record_id,
            timestamp,
            ..
        } => {
            let sensor = entry_sensor(entry, names);
            let oem = decoded.get(&record_id.value());
            let desc = match oem {
                Some(oem) if oem.css => format!("{} (CSS component)", oem.text),
                Some(oem) => oem.text.clone(),
                None => dell_oem_diagnostic_description(entry, manufacturer_id)
                    .unwrap_or_else(|| display_entry_description(entry)),
            };
            let (severity, rendered_severity) =
                severity_label(displayed_severity(entry, decoded, manufacturer_id));
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
                Cell::colored("—", ui::dim("—")),
                Cell::colored(severity, rendered_severity),
                Cell::new(format!("OEM 0x{ty:02X}")),
                Cell::new(oem_dump(data)),
            ]);
        }
    }
}

pub fn entry_description(entry: &Entry) -> String {
    if let Some(description) = power_unit_transition_description(entry) {
        return description;
    }
    if let Some(description) = system_firmware_progress_description(entry) {
        return description;
    }
    Desc(entry).to_string()
}

fn dell_oem_diagnostic_description(entry: &Entry, manufacturer_id: Option<u32>) -> Option<String> {
    if !manufacturer_id.is_some_and(|id| DELL_MANUFACTURER_IDS.contains(&id)) {
        return None;
    }
    let Entry::System {
        sensor_type,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return None;
    };
    // Dell calls this combination "Err Reg Pointer / OEM Diagnostic data".
    // It is an informational companion record used by service tooling, not
    // the Link Tuning event that shares sensor type C1 with event type 6F.
    if *sensor_type != 0xC1 || *event_type != 0x72 || event_data.offset != 0x02 {
        return None;
    }

    let data2 = match event_data.data2_type {
        EventData2Type::OemCode(value) => Some(value.get()),
        _ => None,
    };
    let data3 = match event_data.data3_type {
        EventData3Type::OemCode(value) => Some(value.get()),
        _ => None,
    };
    let suffix = match (data2, data3) {
        (Some(data2), Some(data3)) => format!("; data {data2:02X} {data3:02X}"),
        (Some(data2), None) => format!("; data {data2:02X}"),
        _ => String::new(),
    };
    Some(format!(
        "Dell OEM diagnostic data (additional context for hardware failure{suffix})"
    ))
}

fn power_unit_transition_description(entry: &Entry) -> Option<String> {
    let Entry::System {
        sensor_type,
        event_direction,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return None;
    };
    if *sensor_type != 0x09 || *event_type != 0x6F || event_data.offset != 0x00 {
        return None;
    }
    Some(match event_direction {
        EventDirection::Assert => "Power off / power down initiated".to_string(),
        EventDirection::Deassert => "Power off / power down condition cleared".to_string(),
    })
}

fn system_firmware_progress_description(entry: &Entry) -> Option<String> {
    let Entry::System {
        sensor_type,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return None;
    };
    if *sensor_type != 0x0F || *event_type != 0x6F || event_data.offset != 0x02 {
        return None;
    }

    let code = match event_data.data2_type {
        EventData2Type::SensorSpecific(value) => value.get(),
        _ => {
            return Some(
                "Firmware progress checkpoint (detail not supplied by firmware)".to_string(),
            )
        }
    };
    let detail = match code {
        0x00 => "Firmware progress checkpoint (detail not supplied by firmware)",
        0x01 => "Memory initialization",
        0x02 => "Hard-disk initialization",
        0x03 => "Secondary processor initialization",
        0x04 => "User authentication",
        0x05 => "User-initiated system setup",
        0x06 => "USB resource configuration",
        0x07 => "PCI resource configuration",
        0x08 => "Option ROM initialization",
        0x09 => "Video initialization",
        0x0A => "Cache initialization",
        0x0B => "SMBus initialization",
        0x0C => "Keyboard controller initialization",
        0x0D => "Embedded controller initialization",
        0x0E => "Docking station attachment",
        0x0F => "Enabling docking station",
        0x10 => "Docking station ejection",
        0x11 => "Disabling docking station",
        0x12 => "Calling operating system wake-up vector",
        0x13 => "Starting operating system boot process",
        0x14 => "Baseboard or motherboard initialization",
        0x16 => "Floppy initialization",
        0x17 => "Keyboard test",
        0x18 => "Pointing device test",
        0x19 => "Primary processor initialization",
        _ => return Some(format!("System firmware progress code 0x{code:02X}")),
    };
    Some(detail.to_string())
}

fn display_entry_description(entry: &Entry) -> String {
    let decoded = entry_description(entry);
    if !decoded.is_empty() {
        return decoded;
    }
    let Entry::System {
        event_direction,
        event_type,
        event_data,
        ..
    } = entry
    else {
        return "undecoded OEM record".to_string();
    };
    let direction = match event_direction {
        EventDirection::Assert => "asserted",
        EventDirection::Deassert => "deasserted",
    };
    let kind = match event_type {
        0x01 => "threshold",
        0x02..=0x0C => "generic discrete",
        0x6F => "sensor-specific",
        0x70..=0x7F => "OEM",
        _ => "unknown",
    };
    let extra = event_data.to_string();
    if extra.is_empty() {
        format!(
            "{direction} {kind} event (type 0x{event_type:02X}, offset 0x{:02X})",
            event_data.offset
        )
    } else {
        format!(
            "{direction} {kind} event (type 0x{event_type:02X}, offset 0x{:02X}; {extra})",
            event_data.offset
        )
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
        // Initiating the erase changes the repository and therefore cancels
        // the reservation used for that command. Some BMCs tolerate reusing
        // it for status polling, but conforming implementations such as Cisco
        // CIMC reject it with 0xC5. Acquire a fresh reservation for every
        // status request because a Clear SEL command may cancel it again.
        let status_reservation = if info.supported_cmds.contains(&SelCommand::Reserve) {
            Some(
                conn.send_recv(ReserveSel)
                    .map_err(|e| format!("Reserve SEL for status failed: {e:?}"))?,
            )
        } else {
            None
        };
        progress = conn
            .send_recv(ClearSel::get_status(status_reservation))
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
            format!(
                "0x{:04X}: {}",
                entry_id(entry).value(),
                display_entry_description(entry)
            )
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
    fn decodes_standard_system_firmware_progress_extension() {
        let mut starting_os = system_event(EventDirection::Assert, 0x6F, 0x02);
        // Selector bits have already been normalized by read_entries: Data 2
        // is a sensor-specific extension and carries progress code 0x13.
        if let Entry::System {
            sensor_type,
            event_data,
            ..
        } = &mut starting_os
        {
            *sensor_type = 0x0F;
            *event_data = EventData::parse(&[0x32, 0x13, 0xFF]);
        }
        assert_eq!(
            entry_description(&starting_os),
            "Starting operating system boot process"
        );
        assert_eq!(entry_severity(&starting_os), SelSeverity::Normal);

        if let Entry::System { event_data, .. } = &mut starting_os {
            *event_data = EventData::parse(&[0x32, 0x00, 0xFF]);
        }
        assert_eq!(
            entry_description(&starting_os),
            "Firmware progress checkpoint (detail not supplied by firmware)"
        );
    }

    #[test]
    fn distinguishes_power_down_assertion_and_clear() {
        let mut asserted = system_event(EventDirection::Assert, 0x6F, 0x00);
        if let Entry::System { sensor_type, .. } = &mut asserted {
            *sensor_type = 0x09;
        }
        let mut cleared = asserted.clone();
        if let Entry::System {
            event_direction, ..
        } = &mut cleared
        {
            *event_direction = EventDirection::Deassert;
        }

        assert_eq!(
            entry_description(&asserted),
            "Power off / power down initiated"
        );
        assert_eq!(
            entry_description(&cleared),
            "Power off / power down condition cleared"
        );
        assert_eq!(entry_severity(&asserted), SelSeverity::Normal);
        assert_eq!(entry_severity(&cleared), SelSeverity::Normal);
    }

    #[test]
    fn normalizes_ipmi_event_data_selector_bit_order() {
        assert_eq!(swap_event_data_selectors(0xC2), 0x32);
        assert_eq!(swap_event_data_selectors(0x32), 0xC2);
        assert_eq!(swap_event_data_selectors(0xA0), 0xA0);
    }

    #[test]
    fn selects_only_oem_defined_system_events_for_firmware_decoding() {
        let standard = system_event(EventDirection::Assert, 0x6F, 0);
        assert!(!should_decode_oem(&standard));

        let mut oem_sensor = system_event(EventDirection::Assert, 0x6F, 0);
        let Entry::System { sensor_type, .. } = &mut oem_sensor else {
            unreachable!();
        };
        *sensor_type = 0xE1;
        assert!(should_decode_oem(&oem_sensor));

        let oem_event = system_event(EventDirection::Assert, 0x7F, 6);
        assert!(should_decode_oem(&oem_event));
    }

    #[test]
    fn uses_fujitsu_firmware_severity_when_available() {
        let entry = system_event(EventDirection::Assert, 0x7F, 6);
        let mut decoded = OemDecodedEntries::new();
        decoded.insert(
            1,
            fujitsu::DecodedSelEntry {
                record_id: 1,
                record_type: 2,
                timestamp: 1_700_000_000,
                severity: fujitsu::Severity::Informational,
                css: false,
                text: "BBU relearn required".to_string(),
            },
        );
        assert_eq!(
            displayed_severity(&entry, &decoded, None),
            SelSeverity::Normal
        );
    }

    #[test]
    fn decodes_dell_oem_diagnostic_companion_without_link_tuning_guess() {
        let mut entry = system_event(EventDirection::Assert, 0x72, 0x02);
        if let Entry::System {
            sensor_type,
            event_data,
            ..
        } = &mut entry
        {
            *sensor_type = 0xC1;
            *event_data = EventData::parse(&[0xA2, 0x02, 0x00]);
        }

        let description = dell_oem_diagnostic_description(&entry, Some(0x0002A2)).unwrap();
        assert_eq!(
            description,
            "Dell OEM diagnostic data (additional context for hardware failure; data 02 00)"
        );
        assert_eq!(
            displayed_severity(&entry, &OemDecodedEntries::new(), Some(0x0002A2)),
            SelSeverity::Normal
        );
        assert!(dell_oem_diagnostic_description(&entry, None).is_none());
        assert!(dell_oem_diagnostic_description(&entry, Some(0x000157)).is_none());
    }

    #[test]
    fn preserves_undecoded_event_context_without_guessing_severity() {
        let entry = system_event(EventDirection::Assert, 0x6F, 0x0E);
        assert_eq!(entry_severity(&entry), SelSeverity::Unknown);
        let description = display_entry_description(&entry);
        assert!(description.contains("asserted sensor-specific event"));
        assert!(description.contains("offset 0x0E"));
    }

    #[test]
    fn classifies_running_and_oem_crash_text() {
        assert_eq!(
            severity_from_description("transition to Running"),
            Some(SelSeverity::Normal)
        );
        assert_eq!(
            severity_from_description("softlockup: hung tasks"),
            Some(SelSeverity::Critical)
        );
        assert_eq!(severity_from_description("vendor state 42"), None);
    }

    #[test]
    fn sensor_names_are_keyed_by_type_and_number() {
        let mut names = SensorNames::new();
        names.insert((0x01, 7), "Temperature 7".to_string());
        names.insert((0x04, 7), "Fan 7".to_string());
        let entry = system_event(EventDirection::Assert, 0x01, 0);
        let Entry::System { sensor_number, .. } = &entry else {
            unreachable!()
        };
        assert_eq!(*sensor_number, 1);
        names.insert((0x01, 1), "CPU Temp".to_string());
        assert!(entry_sensor(&entry, &names).starts_with("CPU Temp"));
    }

    #[test]
    fn parses_rfc3339_and_unix_times() {
        assert_eq!(parse_time("1700000000"), Ok(1_700_000_000));
        assert_eq!(parse_time("2023-11-14T22:13:20Z"), Ok(1_700_000_000));
        assert!(parse_time("last Tuesday").is_err());
    }
}
