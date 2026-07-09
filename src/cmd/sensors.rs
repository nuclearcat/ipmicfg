//! `sensors` — read and display analog and discrete sensor health.

use std::io::IsTerminal;

use ipmi_rs::connection::{Address, Channel, NetFn};
use ipmi_rs::storage::sdr::event_reading_type_code::EventReadingTypeCodes;
use ipmi_rs::storage::sdr::record::{
    FullSensorRecord, IdentifiableSensor, InstancedSensor, RecordContents, SensorKey,
};
use ipmi_rs::storage::sdr::{EventData, SensorType};
use ipmi_rs::storage::sel::{
    Entry, EventDirection, EventGenerator, EventMessageRevision, RecordId,
};

use crate::cli::{SensorState, SensorsArgs};
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Status, Table};

const CMD_GET_SENSOR_READING: u8 = 0x2D;
const CMD_GET_SENSOR_THRESHOLDS: u8 = 0x27;

pub fn run(conn: &mut Conn, args: &SensorsArgs) -> Result<(), String> {
    loop {
        render(conn, args)?;
        let Some(seconds) = args.watch else {
            return Ok(());
        };
        println!(
            "  {}",
            ui::dim(&format!(
                "refreshing every {seconds}s; press Ctrl-C to stop"
            ))
        );
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}

fn render(conn: &mut Conn, args: &SensorsArgs) -> Result<(), String> {
    let interactive = std::io::stderr().is_terminal();
    if interactive {
        eprintln!("Reading SDR repository...");
    }
    let records = conn.collect_sdrs()?;
    let sensor_count = records
        .iter()
        .filter(|record| {
            matches!(
                &record.contents,
                RecordContents::FullSensor(_)
                    | RecordContents::CompactSensor(_)
                    | RecordContents::EventOnlySensor(_)
            )
        })
        .count();
    if interactive {
        eprintln!(
            "Reading {sensor_count} sensors (individual BMC timeouts may delay completion)..."
        );
    }
    let mut analog = Table::new(
        if args.thresholds {
            &["TYPE", "SENSOR", "READING", "STATE", "THRESHOLDS"]
        } else {
            &["TYPE", "SENSOR", "READING", "STATE"]
        },
        if args.thresholds {
            &[
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Left,
                Align::Left,
            ]
        } else {
            &[Align::Left, Align::Left, Align::Right, Align::Left]
        },
    );
    let mut discrete = Table::new(
        &["TYPE", "SENSOR", "STATE", "ASSERTED"],
        &[Align::Left, Align::Left, Align::Left, Align::Left],
    );
    let mut hidden_discrete = 0usize;
    let mut counts = Counts::default();

    for record in &records {
        match &record.contents {
            RecordContents::FullSensor(full) => {
                let ty = full.ty().to_string();
                let name = full.id_string().to_string();
                if !matches_text(args, &ty, &name) {
                    continue;
                }

                if !is_threshold(full.event_reading_type_codes()) {
                    add_discrete(
                        conn,
                        args,
                        &mut discrete,
                        &mut hidden_discrete,
                        &mut counts,
                        DiscreteSensor {
                            ty: *full.ty(),
                            type_name: ty,
                            name,
                            event_code: *full.event_reading_type_codes(),
                            key: full.key_data(),
                        },
                    );
                    continue;
                }

                verbose_read(args, &ty, &name, "threshold");
                let reading = read_analog(conn, full);
                if let Err(error) = &reading {
                    if args.verbose {
                        eprintln!("{} {name}: {error}", ui::yellow("warning:"));
                    }
                }
                let (text, status) = match reading {
                    Ok((text, status)) => (text, status),
                    Err(_) => ("n/a".to_string(), Status::Unknown),
                };
                counts.tally(status);
                if !matches_state(args, status) {
                    continue;
                }

                let mut row = vec![
                    Cell::new(ty),
                    Cell::new(name),
                    Cell::colored(text.clone(), colorize(&text, status)),
                    Cell::colored(state_label(status), status.badge()),
                ];
                if args.thresholds {
                    let thresholds = match read_thresholds(conn, full) {
                        Ok(value) => value,
                        Err(error) => {
                            if args.verbose {
                                eprintln!(
                                    "{} {} thresholds: {error}",
                                    ui::yellow("warning:"),
                                    full.id_string()
                                );
                            }
                            "n/a".to_string()
                        }
                    };
                    row.push(Cell::new(thresholds));
                }
                analog.row(row);
            }
            RecordContents::CompactSensor(sensor) => {
                let ty = sensor.ty().to_string();
                let name = sensor.id_string().to_string();
                if !matches_text(args, &ty, &name) {
                    continue;
                }
                add_discrete(
                    conn,
                    args,
                    &mut discrete,
                    &mut hidden_discrete,
                    &mut counts,
                    DiscreteSensor {
                        ty: *sensor.ty(),
                        type_name: ty,
                        name,
                        event_code: *sensor.event_reading_type_codes(),
                        key: sensor.key_data(),
                    },
                );
            }
            RecordContents::EventOnlySensor(sensor) => {
                let ty = sensor.ty.to_string();
                let name = sensor.id_string.to_string();
                if !matches_text(args, &ty, &name) {
                    continue;
                }
                add_discrete(
                    conn,
                    args,
                    &mut discrete,
                    &mut hidden_discrete,
                    &mut counts,
                    DiscreteSensor {
                        ty: sensor.ty,
                        type_name: ty,
                        name,
                        event_code: sensor.event_reading_type_code,
                        key: &sensor.key,
                    },
                );
            }
            _ => {}
        }
    }

    ui::header("Sensors");
    if analog.is_empty() {
        println!("  {}", ui::dim("no analog sensors matched"));
    } else {
        analog.print();
    }

    if args.all {
        ui::header("Discrete sensors");
        if discrete.is_empty() {
            println!("  {}", ui::dim("no discrete sensors matched"));
        } else {
            discrete.print();
        }
    } else if hidden_discrete > 0 {
        println!(
            "  {}",
            ui::dim(&format!(
                "({hidden_discrete} discrete sensors hidden; use --all to show)"
            ))
        );
    }

    println!();
    print_counts(counts);
    println!();
    Ok(())
}

struct DiscreteSensor<'a> {
    ty: SensorType,
    type_name: String,
    name: String,
    event_code: EventReadingTypeCodes,
    key: &'a SensorKey,
}

fn add_discrete(
    conn: &mut Conn,
    args: &SensorsArgs,
    table: &mut Table,
    hidden: &mut usize,
    counts: &mut Counts,
    sensor: DiscreteSensor<'_>,
) {
    verbose_read(args, &sensor.type_name, &sensor.name, "discrete");
    let reading = read_discrete(conn, &sensor);
    if let Err(error) = &reading {
        if args.verbose {
            eprintln!("{} {}: {error}", ui::yellow("warning:"), sensor.name);
        }
    }
    let (status, asserted) = reading.unwrap_or((Status::Unknown, "n/a".to_string()));
    counts.tally(status);
    if !matches_state(args, status) {
        return;
    }
    *hidden += 1;
    if args.all {
        table.row(vec![
            Cell::new(sensor.type_name),
            Cell::new(sensor.name),
            Cell::colored(state_label(status), status.badge()),
            Cell::new(asserted),
        ]);
    }
}

fn read_analog(conn: &mut Conn, full: &FullSensorRecord) -> Result<(String, Status), String> {
    let response = send_sensor_raw(conn, full.key_data(), CMD_GET_SENSOR_READING)?;
    if response.cc() != 0 {
        return Err(format!("completion code 0x{:02X}", response.cc()));
    }
    let data = response.data();
    if data.len() < 2 {
        return Err("short response".to_string());
    }
    let unavailable = data[1] & 0x20 != 0;
    let status = if unavailable {
        Status::Unknown
    } else {
        classify_threshold_bits(data.get(2).copied().unwrap_or(0))
    };
    let text = if unavailable {
        "n/a".to_string()
    } else {
        full.display_reading(data[0])
            .unwrap_or_else(|| format!("raw 0x{:02X}", data[0]))
    };
    Ok((text, status))
}

fn read_discrete(conn: &mut Conn, sensor: &DiscreteSensor<'_>) -> Result<(Status, String), String> {
    let response = send_sensor_raw(conn, sensor.key, CMD_GET_SENSOR_READING)?;
    if response.cc() != 0 {
        return Err(format!("completion code 0x{:02X}", response.cc()));
    }
    let data = response.data();
    if data.len() < 2 {
        return Err("short response".to_string());
    }
    if data[1] & 0x20 != 0 {
        return Ok((Status::Unknown, "state unavailable".to_string()));
    }

    let raw = u16::from_le_bytes([
        data.get(2).copied().unwrap_or(0),
        data.get(3).copied().unwrap_or(0) & 0x7F,
    ]);
    if raw == 0 {
        return Ok((Status::Ok, "none (raw 0x0000)".to_string()));
    }

    if has_unknown_semantics(sensor.event_code) {
        return Ok((
            Status::Unknown,
            format!("vendor/unspecified state (raw 0x{raw:04X})"),
        ));
    }

    let mut descriptions = Vec::new();
    let mut status = Status::Ok;
    for offset in 0..15u8 {
        if raw & (1 << offset) == 0 {
            continue;
        }
        let description = discrete_description(sensor.ty, sensor.event_code, offset);
        status = worst(status, discrete_state_status(&description));
        descriptions.push(description);
    }
    Ok((
        status,
        format!("{} (raw 0x{raw:04X})", descriptions.join(", ")),
    ))
}

fn discrete_description(
    sensor_type: SensorType,
    event_code: EventReadingTypeCodes,
    offset: u8,
) -> String {
    let event_type = event_code_value(event_code);
    let entry = Entry::System {
        record_id: RecordId::new(1).expect("one is a valid SEL record ID"),
        timestamp: 0.into(),
        generator_id: EventGenerator::SoftwareId {
            software_id: 0,
            channel_number: Channel::Primary,
        },
        event_message_format: EventMessageRevision::V2_0,
        sensor_type: sensor_type.into(),
        sensor_number: 0,
        event_direction: EventDirection::Assert,
        event_type,
        event_data: EventData::parse(&[offset, 0xFF, 0xFF]),
    };
    let text = crate::cmd::sel::entry_description(&entry);
    if text.is_empty() {
        format!("offset {offset}")
    } else {
        text
    }
}

fn event_code_value(code: EventReadingTypeCodes) -> u8 {
    match code {
        EventReadingTypeCodes::Unspecified => 0x00,
        EventReadingTypeCodes::Threshold => 0x01,
        EventReadingTypeCodes::DiscreteGeneric(value) => value,
        EventReadingTypeCodes::SensorSpecific => 0x6F,
        EventReadingTypeCodes::Oem(value) | EventReadingTypeCodes::Reserved(value) => value,
    }
}

fn is_threshold(code: &EventReadingTypeCodes) -> bool {
    matches!(code, EventReadingTypeCodes::Threshold)
}

fn has_unknown_semantics(code: EventReadingTypeCodes) -> bool {
    matches!(
        code,
        EventReadingTypeCodes::Unspecified
            | EventReadingTypeCodes::Oem(_)
            | EventReadingTypeCodes::Reserved(_)
    )
}

fn discrete_state_status(description: &str) -> Status {
    let value = description.to_ascii_lowercase();
    if [
        "predictive failure",
        "warning",
        "degraded",
        "not present",
        "absent",
    ]
    .iter()
    .any(|word| value.contains(word))
    {
        Status::Warn
    } else if [
        "failure",
        "failed",
        "fault",
        "critical",
        "non-recoverable",
        "nonrecoverable",
        "lost",
        "unavailable",
        "thermal trip",
        "limit exceeded",
        "uncorrectable",
    ]
    .iter()
    .any(|word| value.contains(word))
    {
        Status::Crit
    } else if [
        "present",
        "enabled",
        "fully redundant",
        "power on",
        "running",
    ]
    .iter()
    .any(|word| value.contains(word))
    {
        Status::Ok
    } else {
        Status::Warn
    }
}

fn read_thresholds(conn: &mut Conn, full: &FullSensorRecord) -> Result<String, String> {
    let response = send_sensor_raw(conn, full.key_data(), CMD_GET_SENSOR_THRESHOLDS)?;
    if response.cc() != 0 {
        return Err(format!("completion code 0x{:02X}", response.cc()));
    }
    let data = response.data();
    if data.len() < 7 {
        return Err("short response".to_string());
    }
    let mask = data[0];
    let definitions = [
        (0, "LNC", data[1]),
        (1, "LC", data[2]),
        (2, "LNR", data[3]),
        (3, "UNC", data[4]),
        (4, "UC", data[5]),
        (5, "UNR", data[6]),
    ];
    let values = definitions
        .into_iter()
        .filter(|(bit, _, _)| mask & (1 << bit) != 0)
        .filter_map(|(_, label, raw)| {
            full.display_reading(raw)
                .map(|value| format!("{label}={value}"))
        })
        .collect::<Vec<_>>();
    Ok(if values.is_empty() {
        "none configured".to_string()
    } else {
        values.join("  ")
    })
}

fn send_sensor_raw(
    conn: &mut Conn,
    key: &SensorKey,
    command: u8,
) -> Result<ipmi_rs::connection::Response, String> {
    conn.send_raw_to(
        NetFn::SensorEvent,
        command,
        vec![key.sensor_number.get()],
        Address(key.owner_id.into()),
        key.owner_channel,
        key.owner_lun,
    )
    .map_err(|e| format!("sensor command failed: {e}"))
}

fn matches_text(args: &SensorsArgs, type_name: &str, name: &str) -> bool {
    let type_matches = args.r#type.is_empty()
        || args
            .r#type
            .iter()
            .any(|value| contains_case_insensitive(type_name, value));
    let name_matches = args.name.is_empty()
        || args
            .name
            .iter()
            .any(|value| contains_case_insensitive(name, value));
    type_matches && name_matches
}

fn matches_state(args: &SensorsArgs, status: Status) -> bool {
    args.state.is_empty()
        || args.state.iter().any(|wanted| {
            matches!(
                (wanted, status),
                (SensorState::Ok, Status::Ok)
                    | (SensorState::Warn, Status::Warn)
                    | (SensorState::Critical, Status::Crit)
                    | (SensorState::Unknown, Status::Unknown)
            )
        })
}

fn contains_case_insensitive(value: &str, pattern: &str) -> bool {
    value
        .to_ascii_lowercase()
        .contains(&pattern.to_ascii_lowercase())
}

fn verbose_read(args: &SensorsArgs, sensor_type: &str, name: &str, kind: &str) {
    if args.verbose {
        eprintln!(
            "{} reading {kind} sensor {sensor_type} / {name}",
            ui::dim("debug:")
        );
    }
}

#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub ok: usize,
    pub warn: usize,
    pub crit: usize,
    pub unknown: usize,
}

impl Counts {
    fn tally(&mut self, status: Status) {
        match status {
            Status::Ok => self.ok += 1,
            Status::Warn => self.warn += 1,
            Status::Crit => self.crit += 1,
            Status::Unknown => self.unknown += 1,
        }
    }

    pub fn total(self) -> usize {
        self.ok + self.warn + self.crit + self.unknown
    }
}

/// Collect health counts without producing terminal output.
pub fn health_summary(conn: &mut Conn) -> Result<Counts, String> {
    let records = conn.collect_sdrs()?;
    let mut counts = Counts::default();
    for record in &records {
        match &record.contents {
            RecordContents::FullSensor(full) => {
                let status = if is_threshold(full.event_reading_type_codes()) {
                    read_analog(conn, full)
                        .map(|(_, status)| status)
                        .unwrap_or(Status::Unknown)
                } else {
                    let item = DiscreteSensor {
                        ty: *full.ty(),
                        type_name: String::new(),
                        name: String::new(),
                        event_code: *full.event_reading_type_codes(),
                        key: full.key_data(),
                    };
                    read_discrete(conn, &item)
                        .map(|(status, _)| status)
                        .unwrap_or(Status::Unknown)
                };
                counts.tally(status);
            }
            RecordContents::CompactSensor(sensor) => {
                let item = DiscreteSensor {
                    ty: *sensor.ty(),
                    type_name: String::new(),
                    name: String::new(),
                    event_code: *sensor.event_reading_type_codes(),
                    key: sensor.key_data(),
                };
                counts.tally(
                    read_discrete(conn, &item)
                        .map(|(status, _)| status)
                        .unwrap_or(Status::Unknown),
                );
            }
            RecordContents::EventOnlySensor(sensor) => {
                let item = DiscreteSensor {
                    ty: sensor.ty,
                    type_name: String::new(),
                    name: String::new(),
                    event_code: sensor.event_reading_type_code,
                    key: &sensor.key,
                };
                counts.tally(
                    read_discrete(conn, &item)
                        .map(|(status, _)| status)
                        .unwrap_or(Status::Unknown),
                );
            }
            _ => {}
        }
    }
    Ok(counts)
}

fn print_counts(counts: Counts) {
    println!(
        "  {}  {}   {}  {}   {}  {}   {}  {}",
        Status::Ok.badge(),
        counts.ok,
        Status::Warn.badge(),
        ui::yellow(&counts.warn.to_string()),
        Status::Crit.badge(),
        ui::red(&counts.crit.to_string()),
        Status::Unknown.badge(),
        counts.unknown,
    );
}

fn classify_threshold_bits(bits: u8) -> Status {
    // Bits 0..5 are LNC, LC, LNR, UNC, UC, UNR respectively.
    if bits & 0b0011_0110 != 0 {
        Status::Crit
    } else if bits & 0b0000_1001 != 0 {
        Status::Warn
    } else {
        Status::Ok
    }
}

fn worst(left: Status, right: Status) -> Status {
    fn rank(status: Status) -> u8 {
        match status {
            Status::Ok => 0,
            Status::Unknown => 1,
            Status::Warn => 2,
            Status::Crit => 3,
        }
    }
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

fn colorize(text: &str, status: Status) -> String {
    match status {
        Status::Ok => ui::green(text),
        Status::Warn => ui::yellow(text),
        Status::Crit => ui::red(text),
        Status::Unknown => ui::dim(text),
    }
}

fn state_label(status: Status) -> &'static str {
    match status {
        Status::Ok => "OK  ",
        Status::Warn => "WARN",
        Status::Crit => "CRIT",
        Status::Unknown => " -- ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_discrete_descriptions() {
        assert_eq!(discrete_state_status("Power supply failure"), Status::Crit);
        assert_eq!(discrete_state_status("Predictive failure"), Status::Warn);
        assert_eq!(discrete_state_status("Redundancy degraded"), Status::Warn);
        assert_eq!(discrete_state_status("Device Present"), Status::Ok);
        assert_eq!(discrete_state_status("vendor state"), Status::Warn);
    }

    #[test]
    fn worst_status_uses_health_order() {
        assert_eq!(worst(Status::Ok, Status::Warn), Status::Warn);
        assert_eq!(worst(Status::Unknown, Status::Crit), Status::Crit);
        assert_eq!(worst(Status::Warn, Status::Unknown), Status::Warn);
    }

    #[test]
    fn threshold_bits_include_lower_critical() {
        assert_eq!(classify_threshold_bits(0b0000_0010), Status::Crit);
        assert_eq!(classify_threshold_bits(0b0000_1000), Status::Warn);
        assert_eq!(classify_threshold_bits(0), Status::Ok);
    }

    #[test]
    fn classifies_records_by_event_reading_type() {
        assert!(is_threshold(&EventReadingTypeCodes::Threshold));
        assert!(!is_threshold(&EventReadingTypeCodes::SensorSpecific));
        assert!(!is_threshold(&EventReadingTypeCodes::Oem(0x70)));
        assert!(has_unknown_semantics(EventReadingTypeCodes::Oem(0x70)));
    }
}
