//! Chassis boot-option inspection and override.

use ipmi_rs::connection::NetFn;

use crate::cli::{BootAction, BootArgs, BootDevice};
use crate::cmd::{confirm, NETFN_CHASSIS};
use crate::conn::Conn;
use crate::ui;

const CMD_SET_SYSTEM_BOOT_OPTIONS: u8 = 0x08;
const CMD_GET_SYSTEM_BOOT_OPTIONS: u8 = 0x09;
const PARAM_BOOT_FLAGS: u8 = 0x05;

pub fn run(conn: &mut Conn, args: &BootArgs) -> Result<(), String> {
    match args.action.unwrap_or(BootAction::Show) {
        BootAction::Show => show(conn),
        BootAction::Clear => set_flags(conn, None, false, false),
        BootAction::Set {
            boot_device,
            persistent,
            uefi,
            yes,
        } => {
            if persistent
                && !yes
                && !confirm(&format!(
                    "Persistently request {} as the BIOS boot device?",
                    device_name(boot_device)
                ))
            {
                println!("Aborted.");
                return Ok(());
            }
            set_flags(conn, Some(boot_device), persistent, uefi)
        }
    }
}

fn show(conn: &mut Conn) -> Result<(), String> {
    let response = conn
        .send_raw(
            NetFn::from(NETFN_CHASSIS),
            CMD_GET_SYSTEM_BOOT_OPTIONS,
            vec![PARAM_BOOT_FLAGS, 0, 0],
        )
        .map_err(|e| format!("Get System Boot Options failed: {e}"))?;
    check(response.cc(), "Get System Boot Options")?;
    let data = response.data();
    if data.len() < 7 {
        return Err("Get System Boot Options returned a short boot-flags response".to_string());
    }
    let parameter_valid = data[1] & 0x80 == 0;
    let flags1 = data[2];
    let flags2 = data[3];
    let valid = flags1 & 0x80 != 0;
    let selector = (flags2 >> 2) & 0x0F;

    ui::header("Boot Override");
    ui::kv(
        "Parameter",
        if parameter_valid {
            "available"
        } else {
            "locked/invalid"
        },
        18,
    );
    ui::kv("Active", if valid { "yes" } else { "no" }, 18);
    ui::kv("Device", boot_selector_name(selector), 18);
    ui::kv(
        "Scope",
        if flags1 & 0x40 != 0 {
            "persistent request"
        } else {
            "next boot only"
        },
        18,
    );
    ui::kv(
        "Boot type",
        if flags1 & 0x20 != 0 { "UEFI" } else { "legacy" },
        18,
    );
    println!();
    Ok(())
}

fn set_flags(
    conn: &mut Conn,
    device: Option<BootDevice>,
    persistent: bool,
    uefi: bool,
) -> Result<(), String> {
    let flags = encode_flags(device, persistent, uefi);
    let response = conn
        .send_raw(
            NetFn::from(NETFN_CHASSIS),
            CMD_SET_SYSTEM_BOOT_OPTIONS,
            [vec![PARAM_BOOT_FLAGS], flags.to_vec()].concat(),
        )
        .map_err(|e| format!("Set System Boot Options failed: {e}"))?;
    check(response.cc(), "Set System Boot Options")?;
    match device {
        Some(device) => println!(
            "{} {} boot override set for {}",
            ui::green("OK:"),
            device_name(device),
            if persistent {
                "future boots"
            } else {
                "the next boot"
            }
        ),
        None => println!("{} boot override cleared", ui::green("OK:")),
    }
    Ok(())
}

fn encode_flags(device: Option<BootDevice>, persistent: bool, uefi: bool) -> [u8; 5] {
    let flags1 = if device.is_some() {
        0x80 | if persistent { 0x40 } else { 0 } | if uefi { 0x20 } else { 0 }
    } else {
        0
    };
    [
        flags1,
        device.map(device_selector).unwrap_or(0) << 2,
        0,
        0,
        0,
    ]
}

fn device_selector(device: BootDevice) -> u8 {
    match device {
        BootDevice::Pxe => 0x01,
        BootDevice::Disk => 0x02,
        BootDevice::Optical => 0x05,
        BootDevice::Bios => 0x06,
    }
}

fn device_name(device: BootDevice) -> &'static str {
    match device {
        BootDevice::Pxe => "PXE/network",
        BootDevice::Disk => "default disk",
        BootDevice::Optical => "CD/DVD",
        BootDevice::Bios => "BIOS setup",
    }
}

fn boot_selector_name(selector: u8) -> &'static str {
    match selector {
        0x00 => "default/no override",
        0x01 => "PXE/network",
        0x02 => "default disk",
        0x03 => "default disk (safe mode)",
        0x04 => "diagnostic partition",
        0x05 => "CD/DVD",
        0x06 => "BIOS setup",
        0x0F => "removable media",
        _ => "reserved/unknown",
    }
}

fn check(code: u8, operation: &str) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("{operation}: completion code 0x{code:02X}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_boot_selectors() {
        assert_eq!(device_selector(BootDevice::Pxe), 1);
        assert_eq!(device_selector(BootDevice::Disk), 2);
        assert_eq!(device_selector(BootDevice::Optical), 5);
        assert_eq!(device_selector(BootDevice::Bios), 6);
        assert_eq!(
            encode_flags(Some(BootDevice::Pxe), true, true),
            [0xE0, 0x04, 0, 0, 0]
        );
        assert_eq!(encode_flags(None, false, false), [0; 5]);
    }
}
