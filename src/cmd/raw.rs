//! `raw` — guarded arbitrary IPMI requests for protocol diagnostics.

use ipmi_rs::connection::NetFn;

use crate::cli::RawArgs;
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui;

pub fn run(conn: &mut Conn, args: &RawArgs) -> Result<(), String> {
    if !args.yes
        && !confirm(&format!(
            "Send raw IPMI request netfn 0x{:02X}, command 0x{:02X}? Arbitrary commands may change BMC state.",
            args.netfn, args.command
        ))
    {
        return Err("cancelled".to_string());
    }

    ui::header("Raw IPMI Request");
    ui::kv("NetFn", &format!("0x{:02X}", args.netfn), 16);
    ui::kv("Command", &format!("0x{:02X}", args.command), 16);
    ui::kv("Request data", &hex_bytes(&args.data), 16);

    let response = conn
        .send_raw(NetFn::from(args.netfn), args.command, args.data.clone())
        .map_err(|e| format!("raw IPMI request failed: {e}"))?;

    ui::header("Raw IPMI Response");
    let completion = if response.cc() == 0 {
        ui::green("0x00 (success)")
    } else {
        ui::red(&format!("0x{:02X}", response.cc()))
    };
    ui::kv("Completion code", &completion, 16);
    ui::kv("Response data", &hex_bytes(response.data()), 16);
    ui::kv("ASCII", &ascii_bytes(response.data()), 16);
    println!();

    if response.cc() == 0 {
        Ok(())
    } else {
        Err(format!(
            "raw command returned completion code 0x{:02X}",
            response.cc()
        ))
    }
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty)".to_string();
    }
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ascii_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty)".to_string();
    }
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_empty_and_nonempty_hex() {
        assert_eq!(hex_bytes(&[]), "(empty)");
        assert_eq!(hex_bytes(&[0x00, 0x2E, 0xF5]), "00 2E F5");
    }

    #[test]
    fn renders_nonprinting_ascii_as_dots() {
        assert_eq!(ascii_bytes(&[b'A', 0, b' ', 0xFF]), "A. .");
    }
}
