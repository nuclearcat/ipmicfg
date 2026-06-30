//! `status` — a one-screen overview of the BMC and chassis.

use ipmi_rs::app::GetDeviceId;

use crate::cmd::{self, chassis_status, restore_policy_name};
use crate::conn::Conn;
use crate::ui;

pub fn run(conn: &mut Conn) -> Result<(), String> {
    let dev = conn
        .send_recv(GetDeviceId)
        .map_err(|e| format!("Get Device ID failed: {e:?}"))?;

    ui::header("System Overview");

    let vendor = cmd::manufacturer_name(dev.manufacturer_id)
        .map(|n| format!("{n} (0x{:06X})", dev.manufacturer_id))
        .unwrap_or_else(|| format!("0x{:06X}", dev.manufacturer_id));

    // Power state first — it is what people look for.
    match chassis_status(conn) {
        Ok(cs) => {
            let power = if cs.power_on {
                ui::green("ON")
            } else {
                ui::red("OFF")
            };
            ui::kv("Power", &power, 22);
            if cs.power_fault {
                ui::kv("Power fault", &ui::red("yes"), 22);
            }
            if cs.power_overload {
                ui::kv("Power overload", &ui::red("yes"), 22);
            }
            if cs.cooling_fault {
                ui::kv("Cooling fault", &ui::red("yes"), 22);
            }
            if cs.intrusion {
                ui::kv("Chassis intrusion", &ui::yellow("detected"), 22);
            }
            ui::kv(
                "Power restore policy",
                restore_policy_name(cs.restore_policy),
                22,
            );
        }
        Err(e) => ui::kv("Power", &ui::dim(&format!("unavailable ({e})")), 22),
    }

    ui::kv("Manufacturer", &vendor, 22);
    ui::kv("Product ID", &format!("0x{:04X}", dev.product_id), 22);
    ui::kv(
        "BMC firmware",
        &format!("{}.{:02}", dev.major_fw_revision, dev.minor_fw_revision),
        22,
    );
    ui::kv(
        "IPMI version",
        &format!("{}.{}", dev.major_version, dev.minor_version),
        22,
    );
    ui::kv(
        "Device available",
        &if dev.device_available {
            ui::green("yes")
        } else {
            ui::yellow("no (initializing)")
        },
        22,
    );

    ui::header("Capabilities");
    let mut caps = Vec::new();
    if dev.sensor_device_support {
        caps.push("Sensors");
    }
    if dev.sdr_repository_support {
        caps.push("SDR repository");
    }
    if dev.sel_device_support {
        caps.push("SEL");
    }
    if dev.fru_inventory_support {
        caps.push("FRU inventory");
    }
    if dev.chassis_support {
        caps.push("Chassis");
    }
    if dev.bridge_support {
        caps.push("Bridge");
    }
    if caps.is_empty() {
        caps.push("none reported");
    }
    println!("  {}", caps.join(", "));
    println!();

    Ok(())
}
