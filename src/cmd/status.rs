//! `status` — a one-screen overview of BMC, chassis, sensors, and SEL health.

use ipmi_rs::app::GetDeviceId;

use crate::cmd::{self, chassis_status, restore_policy_name};
use crate::conn::Conn;
use crate::ui;

pub fn run(conn: &mut Conn) -> Result<(), String> {
    ui::header("System Overview");

    match chassis_status(conn) {
        Ok(status) => {
            let power = if status.power_on {
                ui::green("ON")
            } else {
                ui::red("OFF")
            };
            ui::kv("Power", &power, 22);
            if status.power_fault {
                ui::kv("Power fault", &ui::red("yes"), 22);
            }
            if status.power_overload {
                ui::kv("Power overload", &ui::red("yes"), 22);
            }
            if status.cooling_fault {
                ui::kv("Cooling fault", &ui::red("yes"), 22);
            }
            if status.intrusion {
                ui::kv("Chassis intrusion", &ui::yellow("detected"), 22);
            }
            ui::kv(
                "Power restore policy",
                restore_policy_name(status.restore_policy),
                22,
            );
        }
        Err(error) => ui::kv("Power", &ui::dim(&format!("unavailable ({error})")), 22),
    }

    match conn.send_recv(GetDeviceId) {
        Ok(device) => {
            let vendor = cmd::manufacturer_name(device.manufacturer_id)
                .map(|name| format!("{name} (0x{:06X})", device.manufacturer_id))
                .unwrap_or_else(|| format!("0x{:06X}", device.manufacturer_id));
            ui::kv("Manufacturer", &vendor, 22);
            ui::kv("Product ID", &format!("0x{:04X}", device.product_id), 22);
            ui::kv(
                "BMC firmware",
                &format!(
                    "{}.{:02}",
                    device.major_fw_revision, device.minor_fw_revision
                ),
                22,
            );
            ui::kv(
                "IPMI version",
                &format!("{}.{}", device.major_version, device.minor_version),
                22,
            );
            ui::kv(
                "Device available",
                &if device.device_available {
                    ui::green("yes")
                } else {
                    ui::yellow("no (initializing)")
                },
                22,
            );

            ui::header("Capabilities");
            let mut capabilities = Vec::new();
            if device.sensor_device_support {
                capabilities.push("Sensors");
            }
            if device.sdr_repository_support {
                capabilities.push("SDR repository");
            }
            if device.sel_device_support {
                capabilities.push("SEL");
            }
            if device.fru_inventory_support {
                capabilities.push("FRU inventory");
            }
            if device.chassis_support {
                capabilities.push("Chassis");
            }
            if device.bridge_support {
                capabilities.push("Bridge");
            }
            if capabilities.is_empty() {
                capabilities.push("none reported");
            }
            println!("  {}", capabilities.join(", "));
        }
        Err(error) => ui::kv(
            "BMC identity",
            &ui::dim(&format!("unavailable ({error:?})")),
            22,
        ),
    }

    ui::header("Monitoring Health");
    match crate::cmd::sensors::health_summary(conn) {
        Ok(sensors) if sensors.total() == 0 => {
            ui::kv("Sensors", &ui::dim("none reported"), 22);
        }
        Ok(sensors) => {
            let rendered = format!(
                "{} OK, {} WARN, {} CRIT, {} unknown",
                sensors.ok, sensors.warn, sensors.crit, sensors.unknown
            );
            let rendered = if sensors.crit > 0 {
                ui::red(&rendered)
            } else if sensors.warn > 0 {
                ui::yellow(&rendered)
            } else {
                ui::green(&rendered)
            };
            ui::kv("Sensors", &rendered, 22);
        }
        Err(error) => ui::kv("Sensors", &ui::dim(&format!("unavailable ({error})")), 22),
    }

    match crate::cmd::sel::health_summary(conn, 3) {
        Ok(sel) => {
            let mut rendered = format!("{} entries, {} critical", sel.entries, sel.critical);
            if sel.overflow {
                rendered.push_str(", OVERFLOW — events lost");
                rendered = ui::red(&rendered);
            } else if sel.critical > 0 {
                rendered = ui::red(&rendered);
            }
            ui::kv("SEL", &rendered, 22);
            for event in sel.recent_critical {
                ui::kv("Recent critical", &event, 22);
            }
        }
        Err(error) => ui::kv("SEL", &ui::dim(&format!("unavailable ({error})")), 22),
    }
    println!();
    Ok(())
}
