//! `inventory` — BMC identity and all discoverable FRU inventory areas.

use std::collections::HashSet;

use ipmi_rs::app::GetDeviceId;
use ipmi_rs::connection::{Address, Channel};
use ipmi_rs::storage::sdr::record::RecordContents;

use crate::cli::InventoryArgs;
use crate::cmd;
use crate::conn::Conn;
use crate::fru::{self, Fru, FruTarget};
use crate::ui::{self, Align, Cell, Table};

struct LocatedFru {
    id: u8,
    name: String,
    target: Option<FruTarget>,
    location: String,
}

pub fn run(conn: &mut Conn, args: &InventoryArgs) -> Result<(), String> {
    let device = conn
        .send_recv(GetDeviceId)
        .map_err(|e| format!("Get Device ID failed: {e:?}"))?;

    ui::header("Management Controller");
    let vendor = cmd::manufacturer_name(device.manufacturer_id)
        .map(|name| format!("{name} (0x{:06X})", device.manufacturer_id))
        .unwrap_or_else(|| format!("0x{:06X}", device.manufacturer_id));
    ui::kv("Manufacturer", &vendor, 18);
    ui::kv("Product ID", &format!("0x{:04X}", device.product_id), 18);
    ui::kv("Device ID", &format!("0x{:02X}", device.device_id), 18);
    ui::kv("Device revision", &device.device_revision.to_string(), 18);
    ui::kv(
        "Firmware",
        &format!(
            "{}.{:02}",
            device.major_fw_revision, device.minor_fw_revision
        ),
        18,
    );

    let records = conn.collect_sdrs()?;
    let mut frus = vec![LocatedFru {
        id: 0,
        name: "Primary FRU".to_string(),
        target: None,
        location: "BMC".to_string(),
    }];
    let mut physical = Vec::new();
    let mut seen = HashSet::new();
    seen.insert((None, 0u8, 0u8));

    for record in &records {
        let RecordContents::FruDeviceLocator(locator) = &record.contents else {
            continue;
        };
        match locator_device(&format!("{:?}", locator.record_key.fru_device)) {
            Some(LocatorDevice::Logical(fru_device_id)) => {
                let channel = Channel::new(locator.record_key.channel_number & 0x0F)
                    .ok_or_else(|| "FRU locator contains an invalid channel".to_string())?;
                let address = locator.record_key.device_access_address << 1;
                if address == 0x20 && channel.value() == 0 && fru_device_id == 0 {
                    frus[0].name = locator.id_string().to_string();
                    continue;
                }
                let key = (Some(address), channel.value(), fru_device_id);
                if seen.insert(key) {
                    frus.push(LocatedFru {
                        id: fru_device_id,
                        name: locator.id_string().to_string(),
                        target: Some(FruTarget {
                            address: Address(address),
                            channel,
                            lun: locator.record_key.lun,
                        }),
                        location: format!("IPMB 0x{address:02X}, channel {}", channel.value()),
                    });
                }
            }
            Some(LocatorDevice::Physical(i2c_address)) => physical.push((
                locator.id_string().to_string(),
                i2c_address,
                locator.record_key.channel_number,
            )),
            None => physical.push((
                format!("{} (unrecognized locator)", locator.id_string()),
                0,
                locator.record_key.channel_number,
            )),
        }
    }

    if let Some(id) = args.fru_id {
        if !frus.iter().any(|fru| fru.id == id) {
            frus.push(LocatedFru {
                id,
                name: "Explicitly selected FRU".to_string(),
                target: None,
                location: "BMC (not listed in SDR)".to_string(),
            });
        }
    }
    let selected = match args.fru_id {
        Some(id) => frus
            .iter()
            .find(|fru| fru.id == id)
            .into_iter()
            .collect::<Vec<_>>(),
        None => frus.iter().collect::<Vec<_>>(),
    };
    if args.raw.is_some() && selected.len() != 1 {
        return Err("raw export requires a single selected FRU".to_string());
    }

    for located in selected {
        let title = format!(
            "FRU {} — {} ({})",
            located.id, located.name, located.location
        );
        ui::header(&title);
        if let Some(path) = &args.raw {
            match fru::read_raw_at(conn, located.id, located.target) {
                Ok(raw) => {
                    std::fs::write(path, &raw)
                        .map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
                    println!(
                        "{} exported {} bytes to {}",
                        ui::green("OK:"),
                        raw.len(),
                        path.display()
                    );
                    print_fru(&fru::parse_image(&raw));
                }
                Err(error) => println!("  {}", ui::dim(&format!("unavailable: {error}"))),
            }
        } else {
            match fru::read_decoded_at(conn, located.id, located.target) {
                Ok(parsed) => print_fru(&parsed),
                Err(error) => println!("  {}", ui::dim(&format!("unavailable: {error}"))),
            }
        }
    }

    let mut table = Table::new(
        &["KIND", "ID/ADDRESS", "NAME", "LOCATION"],
        &[Align::Left, Align::Left, Align::Left, Align::Left],
    );
    for fru in &frus {
        table.row(vec![
            Cell::new("Logical FRU"),
            Cell::new(fru.id.to_string()),
            Cell::new(fru.name.clone()),
            Cell::new(fru.location.clone()),
        ]);
    }
    for (name, address, channel) in physical {
        table.row(vec![
            Cell::new("Physical FRU"),
            Cell::new(format!("I2C 0x{address:02X}")),
            Cell::new(name),
            Cell::new(format!("channel {channel}; direct read unsupported")),
        ]);
    }
    for record in &records {
        let (kind, name) = match &record.contents {
            RecordContents::McDeviceLocator(mc) => ("Mgmt controller", mc.id_string().to_string()),
            RecordContents::GenericDeviceLocator(device) => {
                ("Device", device.id_string().to_string())
            }
            _ => continue,
        };
        table.row(vec![
            Cell::new(kind),
            Cell::new("—"),
            Cell::new(name),
            Cell::new("SDR"),
        ]);
    }
    ui::header("Detected devices");
    table.print();
    println!();
    Ok(())
}

enum LocatorDevice {
    Logical(u8),
    Physical(u8),
}

// ipmi-rs 0.5 exposes FruRecordKey::fru_device publicly but does not re-export
// its enum type. Keep this pinned-version adapter isolated until upstream adds
// an accessor or re-export.
fn locator_device(debug: &str) -> Option<LocatorDevice> {
    let (kind, marker) = if debug.starts_with("Logical(") {
        (true, "fru_device_id: ")
    } else if debug.starts_with("Physical(") {
        (false, "i2c_address: ")
    } else {
        return None;
    };
    let value = debug.split_once(marker)?.1.split([' ', '}']).next()?;
    let value = value.parse::<u8>().ok()?;
    Some(if kind {
        LocatorDevice::Logical(value)
    } else {
        LocatorDevice::Physical(value)
    })
}

fn print_fru(fru: &Fru) {
    field("Product manufacturer", &fru.product.manufacturer);
    field("Product name", &fru.product.product_name);
    field("Part/model", &fru.product.part_number);
    field("Version", &fru.product.version);
    field("Product serial", &fru.product.serial);
    field("Asset tag", &fru.product.asset_tag);
    field("Board manufacturer", &fru.board.manufacturer);
    field("Board product", &fru.board.product_name);
    field("Board part number", &fru.board.part_number);
    field("Board serial", &fru.board.serial);
    field("Board mfg date", &fru.board.mfg_date);
    field("Chassis type", &fru.chassis.chassis_type);
    field("Chassis part number", &fru.chassis.part_number);
    field("Chassis serial", &fru.chassis.serial);
    if fru.is_empty() {
        println!("  {}", ui::dim("present but no decodable fields"));
    }
}

fn field(label: &str, value: &Option<String>) {
    if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
        ui::kv(label, value, 22);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pinned_fru_locator_debug_shape() {
        assert!(matches!(
            locator_device("Logical(LogicalFruDevice { fru_device_id: 3 })"),
            Some(LocatorDevice::Logical(3))
        ));
        assert!(matches!(
            locator_device("Physical(PhysicalFruDevice { i2c_address: 82 })"),
            Some(LocatorDevice::Physical(82))
        ));
    }
}
