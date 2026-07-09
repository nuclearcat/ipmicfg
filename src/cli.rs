//! Command-line interface definition.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::conn::Target;

/// ipmicfg — an intuitive IPMI/BMC tool for configuration, monitoring and inventory.
#[derive(Parser)]
#[command(name = "ipmicfg", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub conn: ConnOpts,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args)]
pub struct ConnOpts {
    /// Remote BMC over LAN, as `user:password@host[:port]` (uses RMCP/RMCP+).
    /// When omitted, the local device is used.
    #[arg(short = 'H', long, global = true, value_name = "USER:PASS@HOST")]
    pub host: Option<String>,

    /// Local IPMI device path (used when --host is not given).
    #[arg(
        short,
        long,
        global = true,
        default_value = "/dev/ipmi0",
        value_name = "PATH"
    )]
    pub device: String,

    /// Response timeout in milliseconds.
    #[arg(long, global = true, default_value_t = 2000, value_name = "MS")]
    pub timeout_ms: u64,
}

impl ConnOpts {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// Resolve the connection target from the provided options.
    pub fn target(&self) -> Result<Target, String> {
        match &self.host {
            None => Ok(Target::Device(self.device.clone())),
            Some(spec) => {
                let (creds, address) = spec.split_once('@').ok_or_else(|| {
                    "invalid --host: expected `user:password@host[:port]`".to_string()
                })?;
                let (username, password) = creds.split_once(':').ok_or_else(|| {
                    "invalid --host: missing password (expected `user:password@host`)".to_string()
                })?;
                if address.is_empty() {
                    return Err("invalid --host: empty address".to_string());
                }
                Ok(Target::Lan {
                    address: with_default_port(address),
                    username: username.to_string(),
                    password: password.to_string(),
                })
            }
        }
    }
}

/// The IPMI RMCP default UDP port.
const DEFAULT_RMCP_PORT: u16 = 623;

/// Append `:623` to an address that does not already carry a port.
///
/// Handles bare IPv4/hostnames (`10.0.0.5`), already-ported addresses
/// (`10.0.0.5:623`), and bracketed IPv6 (`[::1]` / `[::1]:623`).
fn with_default_port(address: &str) -> String {
    if address.starts_with('[') {
        // Bracketed IPv6: a port is present only if `]:` appears.
        if address.contains("]:") {
            address.to_string()
        } else {
            format!("{address}:{DEFAULT_RMCP_PORT}")
        }
    } else if address.matches(':').count() == 1 {
        // Exactly one colon => host:port already.
        address.to_string()
    } else {
        // No colon (IPv4/hostname) or many colons (unbracketed IPv6 literal).
        format!("{address}:{DEFAULT_RMCP_PORT}")
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Show a one-screen overview: identity, power state and capabilities.
    Status,

    /// Read and display sensor values (temperatures, fans, voltages, ...).
    Sensors(SensorsArgs),

    /// Inspect or clear the System Event Log (SEL).
    Sel(SelArgs),

    /// Show hardware inventory: BMC identity, FRU data and detected devices.
    #[command(alias = "fru")]
    Inventory(InventoryArgs),

    /// View or change the BMC LAN (network) configuration.
    Lan(LanArgs),

    /// Inspect or override the host boot device.
    Boot(BootArgs),

    /// List and administer BMC users.
    #[command(alias = "users")]
    User(UserArgs),

    /// Query or control chassis power.
    Power(PowerArgs),

    /// Blink the chassis identify (locate) LED.
    #[command(alias = "blink")]
    Identify(IdentifyArgs),

    /// Maintain the BMC itself: reset it or run its self test.
    #[command(alias = "mc")]
    Bmc(BmcArgs),
}

#[derive(Args)]
pub struct InventoryArgs {
    /// Read only this logical FRU device ID.
    #[arg(long, value_name = "ID")]
    pub fru_id: Option<u8>,

    /// Export the selected FRU's raw image to this file (requires --fru-id).
    #[arg(long, value_name = "PATH", requires = "fru_id")]
    pub raw: Option<PathBuf>,
}

#[derive(Args)]
pub struct SensorsArgs {
    /// Only show sensors whose type name contains this string (e.g. "Temp", "Fan").
    #[arg(short, long, value_name = "TYPE")]
    pub r#type: Vec<String>,

    /// Only show sensors whose name contains this string.
    #[arg(short, long, value_name = "PATTERN")]
    pub name: Vec<String>,

    /// Only show sensors with this health state.
    #[arg(long, value_enum, value_name = "STATE")]
    pub state: Vec<SensorState>,

    /// Also list discrete (non-analog) sensors.
    #[arg(long)]
    pub all: bool,

    /// Display configured threshold values for analog sensors.
    #[arg(long)]
    pub thresholds: bool,

    /// Refresh readings every N seconds until interrupted.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    pub watch: Option<u64>,

    /// Show individual sensor read errors.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SensorState {
    Ok,
    Warn,
    Critical,
    Unknown,
}

#[derive(Args)]
pub struct SelArgs {
    #[command(subcommand)]
    pub action: Option<SelAction>,

    /// Show every OEM record as a raw hex dump instead of reassembling
    /// multi-part OEM text messages into a single row.
    #[arg(long)]
    pub raw: bool,

    /// Only show entries at or after this RFC 3339 or Unix timestamp.
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,

    /// Only show entries at or before this RFC 3339 or Unix timestamp.
    #[arg(long, value_name = "TIME")]
    pub until: Option<String>,

    /// Only show entries whose sensor name/type contains this string.
    #[arg(long, value_name = "PATTERN")]
    pub sensor: Option<String>,

    /// Only show entries with the inferred severity.
    #[arg(long, value_enum, value_name = "SEVERITY")]
    pub severity: Option<SelSeverity>,

    /// Show at most the newest N matching entries.
    #[arg(long, value_name = "N", value_parser = parse_positive_usize)]
    pub limit: Option<usize>,

    /// Keep polling and print newly added matching entries.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SelSeverity {
    Normal,
    Warning,
    Critical,
}

#[derive(Subcommand)]
pub enum SelAction {
    /// List SEL entries (default).
    List,
    /// Show SEL summary information.
    Info,
    /// Erase all SEL entries.
    Clear {
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Delete one SEL entry (when supported by the BMC).
    Delete {
        /// Record ID, in decimal or hexadecimal (for example `0x003A`).
        #[arg(value_parser = parse_u16)]
        record_id: u16,

        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

fn parse_u16(value: &str) -> Result<u16, String> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).map_err(|_| format!("invalid record ID '{value}'"))
    } else {
        value
            .parse::<u16>()
            .map_err(|_| format!("invalid record ID '{value}'"))
    }
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid positive integer '{value}'"))?;
    if parsed == 0 {
        Err("value must be greater than zero".to_string())
    } else {
        Ok(parsed)
    }
}

#[derive(Args)]
pub struct LanArgs {
    #[command(subcommand)]
    pub action: Option<LanAction>,
}

#[derive(Subcommand)]
pub enum LanAction {
    /// Show current LAN configuration for all LAN channels (default).
    Show(LanShowArgs),
    /// Set LAN parameters on a channel.
    Set(LanSetArgs),
}

#[derive(Args, Default)]
pub struct LanShowArgs {
    /// Show only this LAN channel.
    #[arg(short, long)]
    pub channel: Option<u8>,
}

#[derive(Args)]
pub struct LanSetArgs {
    /// LAN channel number to configure.
    #[arg(short, long, default_value_t = 1)]
    pub channel: u8,

    /// Address source: `static` or `dhcp`.
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,

    /// Static IPv4 address.
    #[arg(long, value_name = "A.B.C.D")]
    pub ip: Option<String>,

    /// Subnet mask.
    #[arg(long, value_name = "A.B.C.D")]
    pub netmask: Option<String>,

    /// Default gateway address.
    #[arg(long, value_name = "A.B.C.D")]
    pub gateway: Option<String>,

    /// Default gateway MAC address.
    #[arg(long, value_name = "XX:XX:XX:XX:XX:XX")]
    pub gateway_mac: Option<String>,

    /// 802.1Q VLAN ID (1-4094); 0 disables VLAN tagging.
    #[arg(long, value_name = "ID", value_parser = clap::value_parser!(u16).range(..=4094))]
    pub vlan_id: Option<u16>,

    /// 802.1Q VLAN priority (0-7).
    #[arg(long, value_name = "PRIORITY", value_parser = clap::value_parser!(u8).range(..=7))]
    pub vlan_priority: Option<u8>,

    /// Apply changes without the confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct BootArgs {
    #[command(subcommand)]
    pub action: Option<BootAction>,
}

#[derive(Subcommand, Clone, Copy)]
pub enum BootAction {
    /// Show the current boot override flags (default).
    Show,
    /// Set a boot-device override.
    Set {
        /// Device to select for the next boot.
        #[arg(value_enum)]
        boot_device: BootDevice,

        /// Request that BIOS retain this selection for future boots.
        #[arg(long)]
        persistent: bool,

        /// Request an EFI/UEFI boot rather than legacy BIOS boot.
        #[arg(long)]
        uefi: bool,

        /// Skip the confirmation prompt for a persistent override.
        #[arg(long)]
        yes: bool,
    },
    /// Clear the active boot override.
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BootDevice {
    Pxe,
    Disk,
    Optical,
    Bios,
}

#[derive(Args)]
pub struct UserArgs {
    #[command(subcommand)]
    pub action: UserAction,
}

#[derive(Subcommand)]
pub enum UserAction {
    /// List users and their access on a channel.
    List {
        #[arg(short, long, default_value_t = 1)]
        channel: u8,
    },
    /// Enable a user ID.
    Enable {
        /// User ID (1-63).
        #[arg(value_parser = parse_user_id)]
        user_id: u8,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Disable a user ID.
    Disable {
        /// User ID (1-63).
        #[arg(value_parser = parse_user_id)]
        user_id: u8,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Set a user's maximum privilege on a channel.
    Privilege {
        /// User ID (1-63).
        #[arg(value_parser = parse_user_id)]
        user_id: u8,
        #[arg(value_enum)]
        level: UserPrivilege,
        #[arg(short, long, default_value_t = 1)]
        channel: u8,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Set a user's login name.
    Name {
        /// User ID (1-63).
        #[arg(value_parser = parse_user_id)]
        user_id: u8,
        #[arg(value_name = "NAME")]
        name: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Rotate a user's password without placing it on the command line.
    Password {
        /// User ID (1-63).
        #[arg(value_parser = parse_user_id)]
        user_id: u8,
        /// Read the password from a file instead of prompting.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UserPrivilege {
    Callback,
    User,
    Operator,
    Administrator,
    Oem,
    NoAccess,
}

fn parse_user_id(value: &str) -> Result<u8, String> {
    let id = value
        .parse::<u8>()
        .map_err(|_| format!("invalid user ID '{value}'"))?;
    if (1..=63).contains(&id) {
        Ok(id)
    } else {
        Err("user ID must be between 1 and 63".to_string())
    }
}

#[derive(Args)]
pub struct BmcArgs {
    #[command(subcommand)]
    pub action: BmcAction,
}

#[derive(Subcommand, Clone, Copy)]
pub enum BmcAction {
    /// Restart the BMC. The host keeps running, but the BMC (and this
    /// connection) goes offline for a minute or two.
    Reset {
        /// Warm reset: restart firmware without fully reinitializing
        /// interfaces (cold is the default and works on more BMCs).
        #[arg(long)]
        warm: bool,

        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Report the BMC self test result.
    Selftest,
}

#[derive(Args)]
pub struct PowerArgs {
    #[command(subcommand)]
    pub action: Option<PowerAction>,
}

#[derive(Args)]
pub struct IdentifyArgs {
    #[command(subcommand)]
    pub action: Option<IdentifyAction>,
}

#[derive(Subcommand, Clone, Copy)]
pub enum IdentifyAction {
    /// Blink the LED for a number of seconds (default).
    On {
        /// Duration in seconds (0 turns the LED off).
        #[arg(default_value_t = 15, value_name = "SECONDS")]
        seconds: u8,
    },
    /// Keep the LED on until explicitly turned off.
    Force,
    /// Turn the LED off.
    Off,
}

#[derive(Subcommand, Clone, Copy)]
pub enum PowerAction {
    /// Show current power state (default).
    Status,
    /// Power on.
    On,
    /// Hard power off.
    Off,
    /// Power cycle (off, then on).
    Cycle,
    /// Hard reset.
    Reset,
    /// Graceful (ACPI) shutdown request.
    Soft,
    /// Pulse a diagnostic interrupt (NMI).
    Diag,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sensor_monitoring_options() {
        let cli = Cli::try_parse_from([
            "ipmicfg",
            "sensors",
            "--name",
            "CPU",
            "--state",
            "critical",
            "--thresholds",
            "--watch",
            "5",
        ])
        .expect("valid sensor arguments");
        let Command::Sensors(args) = cli.command else {
            panic!("expected sensors command");
        };
        assert_eq!(args.name, ["CPU"]);
        assert_eq!(args.state, [SensorState::Critical]);
        assert!(args.thresholds);
        assert_eq!(args.watch, Some(5));
    }

    #[test]
    fn parses_hex_sel_record_id() {
        let cli = Cli::try_parse_from(["ipmicfg", "sel", "delete", "0x003A", "--yes"])
            .expect("valid SEL delete arguments");
        let Command::Sel(args) = cli.command else {
            panic!("expected sel command");
        };
        assert!(matches!(
            args.action,
            Some(SelAction::Delete {
                record_id: 0x003A,
                yes: true
            })
        ));
    }

    #[test]
    fn parses_inventory_boot_lan_and_user_options() {
        let inventory =
            Cli::try_parse_from(["ipmicfg", "inventory", "--fru-id", "2", "--raw", "fru.bin"])
                .expect("valid inventory arguments");
        assert!(matches!(
            inventory.command,
            Command::Inventory(InventoryArgs {
                fru_id: Some(2),
                ..
            })
        ));

        let boot = Cli::try_parse_from(["ipmicfg", "boot", "set", "pxe", "--persistent", "--yes"])
            .expect("valid boot arguments");
        assert!(matches!(
            boot.command,
            Command::Boot(BootArgs {
                action: Some(BootAction::Set {
                    boot_device: BootDevice::Pxe,
                    persistent: true,
                    yes: true,
                    ..
                })
            })
        ));

        let lan = Cli::try_parse_from([
            "ipmicfg",
            "lan",
            "set",
            "--vlan-id",
            "100",
            "--gateway-mac",
            "00:11:22:33:44:55",
        ])
        .expect("valid LAN arguments");
        assert!(matches!(lan.command, Command::Lan(_)));

        let user = Cli::try_parse_from([
            "ipmicfg",
            "user",
            "privilege",
            "3",
            "administrator",
            "--yes",
        ])
        .expect("valid user arguments");
        assert!(matches!(user.command, Command::User(_)));
    }
}
