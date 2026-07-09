//! `inventory` — hardware inventory: BMC identity, FRU data, detected devices.

use ipmi_rs::app::GetDeviceId;
use ipmi_rs::storage::sdr::record::RecordContents;

use crate::cmd::{self};
use crate::conn::Conn;
use crate::fru::{self, Fru};
use crate::ui::{self, Align, Cell, Table};

pub fn run(conn: &mut Conn) -> Result<(), String> {
    let dev = conn
        .send_recv(GetDeviceId)
        .map_err(|e| format!("Get Device ID failed: {e:?}"))?;

    ui::header("Management Controller");
    let vendor = cmd::manufacturer_name(dev.manufacturer_id)
        .map(|n| format!("{n} (0x{:06X})", dev.manufacturer_id))
        .unwrap_or_else(|| format!("0x{:06X}", dev.manufacturer_id));
    ui::kv("Manufacturer", &vendor, 18);
    ui::kv("Product ID", &format!("0x{:04X}", dev.product_id), 18);
    ui::kv("Device ID", &format!("0x{:02X}", dev.device_id), 18);
    ui::kv("Device revision", &dev.device_revision.to_string(), 18);
    ui::kv(
        "Firmware",
        &format!("{}.{:02}", dev.major_fw_revision, dev.minor_fw_revision),
        18,
    );
    if let Some(aux) = dev.aux_revision {
        ui::kv(
            "Aux firmware",
            &format!(
                "{:02X} {:02X} {:02X} {:02X}",
                aux[0], aux[1], aux[2], aux[3]
            ),
            18,
        );
    }

    // Primary FRU (device id 0).
    match fru::read(conn, 0) {
        Ok(f) if !f.is_empty() => print_fru(&f),
        Ok(_) => {
            ui::header("FRU (device 0)");
            println!("  {}", ui::dim("present but no decodable fields"));
        }
        Err(e) => {
            ui::header("FRU (device 0)");
            println!("  {}", ui::dim(&format!("unavailable: {e}")));
        }
    }

    // Devices discovered in the SDR repository.
    let records: Vec<_> = conn.sdrs().collect();
    let mut table = Table::new(&["KIND", "NAME"], &[Align::Left, Align::Left]);
    for record in &records {
        let (kind, name) = match &record.contents {
            RecordContents::FruDeviceLocator(fru) => ("FRU device", fru.id_string().to_string()),
            RecordContents::McDeviceLocator(mc) => ("Mgmt controller", mc.id_string().to_string()),
            RecordContents::GenericDeviceLocator(g) => ("Device", g.id_string().to_string()),
            _ => continue,
        };
        table.row(vec![Cell::new(kind), Cell::new(name)]);
    }

    ui::header("Detected devices");
    if table.is_empty() {
        println!("  {}", ui::dim("none reported in SDR"));
    } else {
        table.print();
    }
    println!();

    Ok(())
}

fn print_fru(f: &Fru) {
    ui::header("FRU — Product");
    field("Manufacturer", &f.product.manufacturer);
    field("Product name", &f.product.product_name);
    field("Part/model", &f.product.part_number);
    field("Version", &f.product.version);
    field("Serial number", &f.product.serial);
    field("Asset tag", &f.product.asset_tag);

    if has_board(f) {
        ui::header("FRU — Board");
        field("Manufacturer", &f.board.manufacturer);
        field("Product name", &f.board.product_name);
        field("Part number", &f.board.part_number);
        field("Serial number", &f.board.serial);
        field("Mfg date", &f.board.mfg_date);
    }

    if has_chassis(f) {
        ui::header("FRU — Chassis");
        field("Type", &f.chassis.chassis_type);
        field("Part number", &f.chassis.part_number);
        field("Serial number", &f.chassis.serial);
    }
}

fn has_board(f: &Fru) -> bool {
    f.board.manufacturer.is_some()
        || f.board.product_name.is_some()
        || f.board.serial.is_some()
        || f.board.part_number.is_some()
}

fn has_chassis(f: &Fru) -> bool {
    f.chassis.part_number.is_some() || f.chassis.serial.is_some()
}

fn field(label: &str, value: &Option<String>) {
    if let Some(v) = value {
        if !v.is_empty() {
            ui::kv(label, v, 16);
        }
    }
}
