//! `identify` — blink the chassis identify LED via Chassis Identify (cmd 0x04).

use ipmi_rs::connection::NetFn;

use crate::cli::{IdentifyAction, IdentifyArgs};
use crate::cmd::NETFN_CHASSIS;
use crate::conn::Conn;
use crate::ui;

const CMD_CHASSIS_IDENTIFY: u8 = 0x04;
const CC_INVALID_DATA_LENGTH: u8 = 0xC7;

pub fn run(conn: &mut Conn, args: &IdentifyArgs) -> Result<(), String> {
    let action = args.action.unwrap_or(IdentifyAction::On { seconds: 15 });

    match action {
        IdentifyAction::On { seconds: 0 } | IdentifyAction::Off => {
            check(send(conn, vec![0x00])?)?;
            println!("{} identify LED off", ui::green("OK:"));
        }
        IdentifyAction::On { seconds } => {
            check(send(conn, vec![seconds])?)?;
            println!(
                "{} identify LED blinking for {seconds} seconds",
                ui::green("OK:")
            );
        }
        IdentifyAction::Force => {
            // The "force on" byte is an IPMI 2.0 addition; older BMCs reject
            // the two-byte form, so fall back to the longest timed blink.
            match send(conn, vec![0x00, 0x01])? {
                CC_INVALID_DATA_LENGTH => {
                    check(send(conn, vec![0xFF])?)?;
                    println!(
                        "{} BMC does not support force-on; blinking for 255 seconds instead",
                        ui::yellow("note:")
                    );
                }
                cc => {
                    check(cc)?;
                    println!("{} identify LED on until `identify off`", ui::green("OK:"));
                }
            }
        }
    }
    Ok(())
}

/// Issue Chassis Identify and return the completion code.
fn send(conn: &mut Conn, data: Vec<u8>) -> Result<u8, String> {
    conn.send_raw(NetFn::from(NETFN_CHASSIS), CMD_CHASSIS_IDENTIFY, data)
        .map(|resp| resp.cc())
        .map_err(|e| format!("Chassis Identify failed: {e}"))
}

fn check(cc: u8) -> Result<(), String> {
    if cc != 0 {
        return Err(format!(
            "Chassis Identify rejected: completion code 0x{cc:02X}"
        ));
    }
    Ok(())
}
