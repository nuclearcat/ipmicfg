//! `power` — query and control chassis power via Chassis Control (cmd 0x02).

use ipmi_rs::connection::NetFn;

use crate::cli::{PowerAction, PowerArgs};
use crate::cmd::{chassis_status, confirm, restore_policy_name, NETFN_CHASSIS};
use crate::conn::Conn;
use crate::ui;

const CMD_CHASSIS_CONTROL: u8 = 0x02;

pub fn run(conn: &mut Conn, args: &PowerArgs) -> Result<(), String> {
    let action = args.action.unwrap_or(PowerAction::Status);

    if matches!(action, PowerAction::Status) {
        return show_status(conn);
    }

    let (code, verb, disruptive) = match action {
        PowerAction::On => (0x01u8, "power on", false),
        PowerAction::Off => (0x00, "power off (hard)", true),
        PowerAction::Cycle => (0x02, "power cycle", true),
        PowerAction::Reset => (0x03, "hard reset", true),
        PowerAction::Diag => (0x04, "pulse diagnostic interrupt", true),
        PowerAction::Soft => (0x05, "request soft shutdown", false),
        PowerAction::Status => unreachable!(),
    };

    if disruptive && !confirm(&format!("This will {verb} the host. Continue?")) {
        println!("Aborted.");
        return Ok(());
    }

    let resp = conn
        .send_raw(NetFn::from(NETFN_CHASSIS), CMD_CHASSIS_CONTROL, vec![code])
        .map_err(|e| format!("Chassis Control failed: {e}"))?;
    if resp.cc() != 0 {
        return Err(format!(
            "Chassis Control rejected: completion code 0x{:02X}",
            resp.cc()
        ));
    }

    println!("{} {}", ui::green("OK:"), verb);
    Ok(())
}

fn show_status(conn: &mut Conn) -> Result<(), String> {
    let cs = chassis_status(conn)?;
    ui::header("Power");
    let power = if cs.power_on {
        ui::green("ON")
    } else {
        ui::red("OFF")
    };
    ui::kv("State", &power, 18);
    ui::kv("Restore policy", restore_policy_name(cs.restore_policy), 18);
    if cs.power_fault {
        ui::kv("Power fault", &ui::red("yes"), 18);
    }
    if cs.power_overload {
        ui::kv("Power overload", &ui::red("yes"), 18);
    }
    if cs.cooling_fault {
        ui::kv("Cooling fault", &ui::red("yes"), 18);
    }
    if cs.intrusion {
        ui::kv("Chassis intrusion", &ui::yellow("detected"), 18);
    }
    println!();
    Ok(())
}
