//! BMC user discovery and guarded account administration.

use std::path::Path;

use ipmi_rs::connection::NetFn;

use crate::cli::{Toggle, UserAction, UserArgs, UserPrivilege};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui::{self, Align, Cell, Table};

const CMD_SET_USER_ACCESS: u8 = 0x43;
const CMD_GET_USER_ACCESS: u8 = 0x44;
const CMD_SET_USER_NAME: u8 = 0x45;
const CMD_GET_USER_NAME: u8 = 0x46;
const CMD_SET_USER_PASSWORD: u8 = 0x47;

pub fn run(conn: &mut Conn, args: &UserArgs) -> Result<(), String> {
    match &args.action {
        UserAction::List { channel } => list(conn, *channel),
        UserAction::Enable { user_id, yes } => enable(conn, *user_id, true, *yes),
        UserAction::Disable { user_id, yes } => enable(conn, *user_id, false, *yes),
        UserAction::Privilege {
            user_id,
            level,
            channel,
            ipmi_messaging,
            link_auth,
            yes,
        } => privilege(
            conn,
            *user_id,
            *level,
            *channel,
            *ipmi_messaging,
            *link_auth,
            *yes,
        ),
        UserAction::Name { user_id, name, yes } => set_name(conn, *user_id, name, *yes),
        UserAction::Password {
            user_id,
            password_file,
            yes,
        } => password(conn, *user_id, password_file.as_deref(), *yes),
    }
}

#[derive(Clone, Copy)]
struct Access {
    max_users: u8,
    enabled_users: u8,
    fixed_names: u8,
    callback_only: bool,
    link_auth: bool,
    ipmi_messaging: bool,
    privilege: u8,
}

impl Access {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        Some(Self {
            max_users: data[0] & 0x3F,
            enabled_users: data[1] & 0x3F,
            fixed_names: data[2] & 0x3F,
            callback_only: data[3] & 0x40 != 0,
            link_auth: data[3] & 0x20 != 0,
            ipmi_messaging: data[3] & 0x10 != 0,
            privilege: data[3] & 0x0F,
        })
    }
}

fn list(conn: &mut Conn, channel: u8) -> Result<(), String> {
    validate_channel(channel)?;
    let first = get_access(conn, channel, 1)?;
    let mut table = Table::new(
        &[
            "ID",
            "NAME",
            "ENABLED",
            "IPMI",
            "LINK",
            "PRIVILEGE",
            "NAME TYPE",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Left,
        ],
    );
    for id in 1..=first.max_users {
        let access = get_access(conn, channel, id)?;
        let name = get_name(conn, id).unwrap_or_else(|_| "<unavailable>".to_string());
        table.row(vec![
            Cell::new(id.to_string()),
            Cell::new(if id == 1 && name.is_empty() {
                "<null user>".to_string()
            } else if name.is_empty() {
                "<empty>".to_string()
            } else {
                name
            }),
            Cell::new(if access.link_auth || access.ipmi_messaging {
                "yes"
            } else {
                "no"
            }),
            Cell::new(yes_no(access.ipmi_messaging)),
            Cell::new(yes_no(access.link_auth)),
            Cell::new(privilege_name(access.privilege)),
            Cell::new(if id <= first.fixed_names {
                "fixed"
            } else {
                "configurable"
            }),
        ]);
    }
    ui::header(&format!("BMC Users — channel {channel}"));
    table.print();
    ui::kv("Maximum users", &first.max_users.to_string(), 18);
    ui::kv("Enabled users", &first.enabled_users.to_string(), 18);
    println!();
    Ok(())
}

fn get_access(conn: &mut Conn, channel: u8, user_id: u8) -> Result<Access, String> {
    let response = conn
        .send_raw(
            NetFn::App,
            CMD_GET_USER_ACCESS,
            vec![channel & 0x0F, user_id],
        )
        .map_err(|e| format!("Get User Access for user {user_id} failed: {e}"))?;
    check(response.cc(), "Get User Access")?;
    Access::parse(response.data())
        .ok_or_else(|| "Get User Access returned a short response".to_string())
}

fn get_name(conn: &mut Conn, user_id: u8) -> Result<String, String> {
    let response = conn
        .send_raw(NetFn::App, CMD_GET_USER_NAME, vec![user_id])
        .map_err(|e| format!("Get User Name for user {user_id} failed: {e}"))?;
    check(response.cc(), "Get User Name")?;
    let bytes = response.data();
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).to_string())
}

fn enable(conn: &mut Conn, user_id: u8, enabled: bool, yes: bool) -> Result<(), String> {
    let verb = if enabled { "enable" } else { "disable" };
    if !yes && !confirm(&format!("{verb} BMC user ID {user_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let operation = if enabled { 0x01 } else { 0x00 };
    let response = conn
        .send_raw(NetFn::App, CMD_SET_USER_PASSWORD, vec![user_id, operation])
        .map_err(|e| format!("Set User Password ({verb}) failed: {e}"))?;
    check(response.cc(), "Set User Password")?;
    println!("{} user {user_id} {verb}d", ui::green("OK:"));
    Ok(())
}

fn privilege(
    conn: &mut Conn,
    user_id: u8,
    level: UserPrivilege,
    channel: u8,
    ipmi_messaging: Option<Toggle>,
    link_auth: Option<Toggle>,
    yes: bool,
) -> Result<(), String> {
    validate_channel(channel)?;
    let current = get_access(conn, channel, user_id)?;
    // Bits the caller did not mention keep whatever the BMC already has.
    let want_messaging = ipmi_messaging.map_or(current.ipmi_messaging, Toggle::is_on);
    let want_link_auth = link_auth.map_or(current.link_auth, Toggle::is_on);

    if !yes {
        let mut prompt = format!(
            "Set user {user_id} privilege on channel {channel} to {}",
            privilege_name(privilege_value(level))
        );
        if want_messaging != current.ipmi_messaging {
            prompt.push_str(&format!(", IPMI messaging {}", on_off(want_messaging)));
        }
        if want_link_auth != current.link_auth {
            prompt.push_str(&format!(", link auth {}", on_off(want_link_auth)));
        }
        prompt.push('?');
        if !confirm(&prompt) {
            println!("Aborted.");
            return Ok(());
        }
    }

    let flags = access_flags(
        channel,
        current.callback_only,
        want_link_auth,
        want_messaging,
    );
    set_user_access(conn, flags, user_id, privilege_value(level))?;
    println!("{} user privilege updated", ui::green("OK:"));
    if want_messaging != current.ipmi_messaging {
        println!("  IPMI messaging {}", on_off(want_messaging));
    }
    if want_link_auth != current.link_auth {
        println!("  link authentication {}", on_off(want_link_auth));
    }
    Ok(())
}

/// Send Set User Access, falling back to the four-byte form when the BMC insists on it.
///
/// The spec makes the trailing user session limit byte optional, but some controllers
/// (Intel's S2600 family among them) reject the three-byte request with `0xC7`, request
/// data length invalid. Retry with an explicit "no session limit" byte rather than failing.
fn set_user_access(conn: &mut Conn, flags: u8, user_id: u8, privilege: u8) -> Result<(), String> {
    const CC_REQUEST_DATA_LENGTH_INVALID: u8 = 0xC7;
    const NO_SESSION_LIMIT: u8 = 0x00;

    let response = conn
        .send_raw(
            NetFn::App,
            CMD_SET_USER_ACCESS,
            vec![flags, user_id, privilege],
        )
        .map_err(|e| format!("Set User Access failed: {e}"))?;
    if response.cc() != CC_REQUEST_DATA_LENGTH_INVALID {
        return check(response.cc(), "Set User Access");
    }

    let response = conn
        .send_raw(
            NetFn::App,
            CMD_SET_USER_ACCESS,
            vec![flags, user_id, privilege, NO_SESSION_LIMIT],
        )
        .map_err(|e| format!("Set User Access failed: {e}"))?;
    check(response.cc(), "Set User Access")
}

/// Byte 1 of Set User Access: bit 7 requests that the remaining bits be applied,
/// bits 6-4 carry the access flags, and the low nibble selects the channel.
fn access_flags(channel: u8, callback_only: bool, link_auth: bool, ipmi_messaging: bool) -> u8 {
    0x80 | if callback_only { 0x40 } else { 0 }
        | if link_auth { 0x20 } else { 0 }
        | if ipmi_messaging { 0x10 } else { 0 }
        | (channel & 0x0F)
}

fn on_off(value: bool) -> &'static str {
    if value {
        "enabled"
    } else {
        "disabled"
    }
}

fn set_name(conn: &mut Conn, user_id: u8, name: &str, yes: bool) -> Result<(), String> {
    if user_id == 1 {
        return Err("user ID 1 has a fixed null username".to_string());
    }
    if !name.is_ascii() || name.as_bytes().contains(&0) || name.len() > 16 {
        return Err("username must be 1-16 ASCII characters without NUL bytes".to_string());
    }
    if name.is_empty() {
        return Err("username must not be empty".to_string());
    }
    if !yes && !confirm(&format!("Rename BMC user ID {user_id} to '{name}'?")) {
        println!("Aborted.");
        return Ok(());
    }
    let mut data = vec![user_id];
    data.extend_from_slice(&padded_16(name.as_bytes()));
    let response = conn
        .send_raw(NetFn::App, CMD_SET_USER_NAME, data)
        .map_err(|e| format!("Set User Name failed: {e}"))?;
    check(response.cc(), "Set User Name")?;
    println!("{} username updated", ui::green("OK:"));
    Ok(())
}

fn password(
    conn: &mut Conn,
    user_id: u8,
    password_file: Option<&Path>,
    yes: bool,
) -> Result<(), String> {
    if !yes && !confirm(&format!("Rotate the password for BMC user ID {user_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let password = match password_file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read password file '{}': {e}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string(),
        None => {
            let first = rpassword::prompt_password("New password: ")
                .map_err(|e| format!("cannot read password: {e}"))?;
            let second = rpassword::prompt_password("Confirm password: ")
                .map_err(|e| format!("cannot read password confirmation: {e}"))?;
            if first != second {
                return Err("passwords do not match".to_string());
            }
            first
        }
    };
    if password.is_empty() || password.len() > 16 || !password.is_ascii() {
        return Err("password must be 1-16 ASCII characters".to_string());
    }
    let mut data = vec![user_id, 0x02];
    data.extend_from_slice(&padded_16(password.as_bytes()));
    let response = conn
        .send_raw(NetFn::App, CMD_SET_USER_PASSWORD, data)
        .map_err(|e| format!("Set User Password failed: {e}"))?;
    check(response.cc(), "Set User Password")?;
    println!("{} password updated", ui::green("OK:"));
    Ok(())
}

fn padded_16(value: &[u8]) -> [u8; 16] {
    let mut output = [0u8; 16];
    output[..value.len().min(16)].copy_from_slice(&value[..value.len().min(16)]);
    output
}

fn privilege_value(level: UserPrivilege) -> u8 {
    match level {
        UserPrivilege::Callback => 0x01,
        UserPrivilege::User => 0x02,
        UserPrivilege::Operator => 0x03,
        UserPrivilege::Administrator => 0x04,
        UserPrivilege::Oem => 0x05,
        UserPrivilege::NoAccess => 0x0F,
    }
}

fn privilege_name(value: u8) -> &'static str {
    match value {
        0x01 => "Callback",
        0x02 => "User",
        0x03 => "Operator",
        0x04 => "Administrator",
        0x05 => "OEM",
        0x0F => "No access",
        _ => "Reserved/unknown",
    }
}

fn validate_channel(channel: u8) -> Result<(), String> {
    if matches!(channel, 0x00..=0x0B | 0x0E..=0x0F) {
        Ok(())
    } else {
        Err(format!("invalid IPMI channel {channel}"))
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
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
    fn pads_passwords_without_exposing_extra_data() {
        let value = padded_16(b"secret");
        assert_eq!(&value[..6], b"secret");
        assert!(value[6..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn maps_privileges_to_ipmi_values() {
        assert_eq!(privilege_value(UserPrivilege::Administrator), 4);
        assert_eq!(privilege_value(UserPrivilege::NoAccess), 15);
    }

    #[test]
    fn enables_ipmi_messaging_without_disturbing_other_access_bits() {
        let current = Access::parse(&[16, 4, 2, 0x04]).unwrap();
        assert!(!current.ipmi_messaging);
        assert!(!current.link_auth);

        // `--ipmi-messaging on` with link auth left unspecified.
        let want_messaging = Some(Toggle::On).map_or(current.ipmi_messaging, Toggle::is_on);
        let want_link_auth = None.map_or(current.link_auth, Toggle::is_on);
        assert!(want_messaging);
        assert!(!want_link_auth);

        let flags = access_flags(3, current.callback_only, want_link_auth, want_messaging);
        assert_eq!(flags, 0x93);
    }

    #[test]
    fn unspecified_toggles_preserve_the_current_access_bits() {
        let current = Access::parse(&[16, 4, 2, 0x34]).unwrap();
        let want_messaging = None.map_or(current.ipmi_messaging, Toggle::is_on);
        let want_link_auth = None.map_or(current.link_auth, Toggle::is_on);
        let flags = access_flags(1, current.callback_only, want_link_auth, want_messaging);
        assert_eq!(flags, 0xB1);
    }

    #[test]
    fn parses_user_access_bitfields() {
        let access = Access::parse(&[16, 4, 2, 0x34]).unwrap();
        assert_eq!(access.max_users, 16);
        assert!(access.link_auth);
        assert!(access.ipmi_messaging);
        assert_eq!(access.privilege, 4);
    }
}
