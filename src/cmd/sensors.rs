//! `sensors` — read and display sensor values with health coloring.

use ipmi_rs::sensor_event::{GetSensorReading, ThresholdReading, ThresholdStatus};
use ipmi_rs::storage::sdr::record::{IdentifiableSensor, InstancedSensor, RecordContents};

use crate::cli::SensorsArgs;
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Status, Table};

pub fn run(conn: &mut Conn, args: &SensorsArgs) -> Result<(), String> {
    // The SDR iterator borrows the connection; collect records first so we can
    // issue Get Sensor Reading commands afterwards.
    let records: Vec<_> = conn.sdrs().collect();

    let filter = |type_name: &str| -> bool {
        args.r#type.is_empty()
            || args
                .r#type
                .iter()
                .any(|t| type_name.to_ascii_lowercase().contains(&t.to_ascii_lowercase()))
    };

    let mut analog = Table::new(
        &["TYPE", "SENSOR", "READING", "STATE"],
        &[Align::Left, Align::Left, Align::Right, Align::Left],
    );
    let mut discrete: Vec<(String, String)> = Vec::new();
    let mut counts = Counts::default();

    for record in &records {
        match &record.contents {
            RecordContents::FullSensor(full) => {
                let type_name = full.ty().to_string();
                if !filter(&type_name) {
                    continue;
                }
                let name = full.id_string().to_string();

                let (reading_cell, status) = match conn
                    .send_recv(GetSensorReading::for_sensor_key(full.key_data()))
                {
                    Ok(raw) => {
                        let tr: ThresholdReading = (&raw).into();
                        let status = status_of(&tr);
                        let text = tr
                            .reading
                            .and_then(|r| full.display_reading(r))
                            .unwrap_or_else(|| "n/a".to_string());
                        let rendered = colorize(&text, status);
                        (Cell::colored(text, rendered), status)
                    }
                    Err(_) => (
                        Cell::colored("n/a", ui::dim("n/a")),
                        Status::Unknown,
                    ),
                };

                counts.tally(status);
                analog.row(vec![
                    Cell::new(type_name),
                    Cell::new(name),
                    reading_cell,
                    Cell::colored(state_label(status), status.badge()),
                ]);
            }
            RecordContents::CompactSensor(compact) => {
                let type_name = compact.ty().to_string();
                if filter(&type_name) {
                    discrete.push((type_name, compact.id_string().to_string()));
                }
            }
            RecordContents::EventOnlySensor(event) => {
                let type_name = event.ty.to_string();
                if filter(&type_name) {
                    discrete.push((type_name, event.id_string.to_string()));
                }
            }
            _ => {}
        }
    }

    ui::header("Sensors");
    if analog.is_empty() {
        println!("  {}", ui::dim("no analog sensors matched"));
    } else {
        analog.print();
        println!();
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

    if args.all && !discrete.is_empty() {
        ui::header("Discrete sensors");
        let mut t = Table::new(&["TYPE", "SENSOR"], &[Align::Left, Align::Left]);
        for (ty, name) in &discrete {
            t.row(vec![Cell::new(ty.clone()), Cell::new(name.clone())]);
        }
        t.print();
    } else if !discrete.is_empty() {
        println!(
            "  {}",
            ui::dim(&format!(
                "({} discrete sensors hidden; use --all to show)",
                discrete.len()
            ))
        );
    }
    println!();

    Ok(())
}

#[derive(Default)]
struct Counts {
    ok: usize,
    warn: usize,
    crit: usize,
    unknown: usize,
}

impl Counts {
    fn tally(&mut self, s: Status) {
        match s {
            Status::Ok => self.ok += 1,
            Status::Warn => self.warn += 1,
            Status::Crit => self.crit += 1,
            Status::Unknown => self.unknown += 1,
        }
    }
}

fn status_of(tr: &ThresholdReading) -> Status {
    match &tr.threshold_status {
        None => Status::Unknown,
        Some(s) => classify(s),
    }
}

fn classify(s: &ThresholdStatus) -> Status {
    if s.at_or_above_non_recoverable
        || s.at_or_below_lower_non_recoverable
        || s.at_or_above_upper_critical
        || s.at_or_below_lower_critical
    {
        Status::Crit
    } else if s.at_or_above_upper_non_critical || s.at_or_below_lower_non_critical {
        Status::Warn
    } else {
        Status::Ok
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
