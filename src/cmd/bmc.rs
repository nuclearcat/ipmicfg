//! `bmc` — management-controller maintenance: Cold/Warm Reset (cmds
//! 0x02/0x03) and Get Self Test Results (cmd 0x04) on the App netfn.

use ipmi_rs::connection::NetFn;

use crate::cli::{BmcAction, BmcArgs};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui;

const CMD_COLD_RESET: u8 = 0x02;
const CMD_WARM_RESET: u8 = 0x03;
const CMD_GET_SELF_TEST_RESULTS: u8 = 0x04;

pub fn run(conn: &mut Conn, args: &BmcArgs) -> Result<(), String> {
    match args.action {
        BmcAction::Reset { warm, yes } => reset(conn, warm, yes),
        BmcAction::Selftest => selftest(conn),
    }
}

fn reset(conn: &mut Conn, warm: bool, yes: bool) -> Result<(), String> {
    let (cmd, kind) = if warm {
        (CMD_WARM_RESET, "warm")
    } else {
        (CMD_COLD_RESET, "cold")
    };

    if !yes
        && !confirm(&format!(
            "This will {kind}-reset the BMC. The host keeps running, but the BMC \
             goes offline for a minute or two. Continue?"
        ))
    {
        println!("Aborted.");
        return Ok(());
    }

    match conn.send_raw(NetFn::App, cmd, vec![]) {
        Ok(resp) if resp.cc() == 0 => {
            println!(
                "{} {kind} reset issued; BMC is restarting",
                ui::green("OK:")
            );
            Ok(())
        }
        Ok(resp) => Err(format!(
            "{kind} reset rejected: completion code 0x{:02X}",
            resp.cc()
        )),
        // Many BMCs reset before answering, so a timeout on a command that
        // just worked for the confirmation round-trip means it took effect.
        Err(e) => {
            println!(
                "{} no response ({e}); the BMC most likely reset before replying",
                ui::yellow("note:")
            );
            Ok(())
        }
    }
}

/// Failure bits of the self-test detail byte for result code 0x57.
const SELF_TEST_FAULTS: [&str; 8] = [
    "cannot access SEL device",
    "cannot access SDR repository",
    "cannot access BMC FRU device",
    "IPMB signal lines do not respond",
    "SDR repository is empty",
    "internal use area of BMC FRU corrupted",
    "controller update (boot block) firmware corrupted",
    "controller operational firmware corrupted",
];

fn selftest(conn: &mut Conn) -> Result<(), String> {
    let resp = conn
        .send_raw(NetFn::App, CMD_GET_SELF_TEST_RESULTS, vec![])
        .map_err(|e| format!("Get Self Test Results failed: {e}"))?;
    if resp.cc() != 0 {
        return Err(format!(
            "Get Self Test Results: completion code 0x{:02X}",
            resp.cc()
        ));
    }

    let d = resp.data();
    let result = d.first().copied().unwrap_or(0);
    let detail = d.get(1).copied().unwrap_or(0);

    match result {
        0x55 => {
            println!("{} self test passed", ui::green("OK:"));
            Ok(())
        }
        0x56 => {
            println!("self test not implemented on this BMC");
            Ok(())
        }
        0x57 => {
            println!("{} self test found problems:", ui::red("FAIL:"));
            for (bit, fault) in SELF_TEST_FAULTS.iter().enumerate() {
                if detail & (1 << bit) != 0 {
                    println!("  - {fault}");
                }
            }
            Err("BMC self test failed".to_string())
        }
        0x58 => Err(format!(
            "self test: fatal hardware error (device-specific code 0x{detail:02X})"
        )),
        other => Err(format!(
            "self test: device-specific failure (0x{other:02X} 0x{detail:02X})"
        )),
    }
}
