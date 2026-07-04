//! Subcommand implementations.

pub mod bmc;
pub mod identify;
pub mod inventory;
pub mod lan;
pub mod power;
pub mod sel;
pub mod sensors;
pub mod status;

use ipmi_rs::connection::NetFn;

use crate::conn::Conn;

pub const NETFN_CHASSIS: u8 = 0x00;
const CMD_GET_CHASSIS_STATUS: u8 = 0x01;

/// Decoded "Get Chassis Status" response.
pub struct ChassisStatus {
    pub power_on: bool,
    pub power_overload: bool,
    pub power_fault: bool,
    pub restore_policy: u8,
    pub intrusion: bool,
    pub cooling_fault: bool,
}

/// Issue Get Chassis Status (Chassis netfn, cmd 0x01).
pub fn chassis_status(conn: &mut Conn) -> Result<ChassisStatus, String> {
    let resp = conn
        .send_raw(NetFn::from(NETFN_CHASSIS), CMD_GET_CHASSIS_STATUS, vec![])
        .map_err(|e| format!("Get Chassis Status failed: {e}"))?;
    if resp.cc() != 0 {
        return Err(format!(
            "Get Chassis Status: completion code 0x{:02X}",
            resp.cc()
        ));
    }
    let d = resp.data();
    let b0 = d.first().copied().unwrap_or(0);
    let b2 = d.get(2).copied().unwrap_or(0);
    Ok(ChassisStatus {
        power_on: b0 & 0x01 != 0,
        power_overload: b0 & 0x02 != 0,
        power_fault: b0 & 0x08 != 0,
        restore_policy: (b0 >> 5) & 0x03,
        intrusion: b2 & 0x01 != 0,
        cooling_fault: b2 & 0x08 != 0,
    })
}

/// Power restore policy as a human-readable string.
pub fn restore_policy_name(policy: u8) -> &'static str {
    match policy {
        0b00 => "always off",
        0b01 => "previous state",
        0b10 => "always on",
        _ => "unknown",
    }
}

/// Map an IANA enterprise number to a vendor name (common server vendors).
pub fn manufacturer_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0x000002 => "IBM",
        0x000009 => "Cisco",
        0x00000B => "HP",
        0x00000E => "Fujitsu Siemens",
        0x000028 => "Dell",
        0x00005D => "ASUS",
        0x0000B0 => "Lenovo",
        0x000137 => "Fujitsu",
        0x000157 => "Intel",
        0x0001AD => "Gigabyte",
        0x00019C => "Tyan",
        0x002A7C => "Supermicro",
        0x00A2B7 => "Quanta",
        _ => return None,
    })
}

/// Ask the user a yes/no question on the terminal.
pub fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
