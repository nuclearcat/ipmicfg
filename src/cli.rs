//! Command-line interface definition.

use std::time::Duration;

use clap::{Args, Parser, Subcommand};

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
    #[arg(short, long, global = true, default_value = "/dev/ipmi0", value_name = "PATH")]
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
    Inventory,

    /// View or change the BMC LAN (network) configuration.
    Lan(LanArgs),

    /// Query or control chassis power.
    Power(PowerArgs),
}

#[derive(Args)]
pub struct SensorsArgs {
    /// Only show sensors whose type name contains this string (e.g. "Temp", "Fan").
    #[arg(short, long, value_name = "TYPE")]
    pub r#type: Vec<String>,

    /// Also list discrete (non-analog) sensors.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct SelArgs {
    #[command(subcommand)]
    pub action: Option<SelAction>,

    /// Show every OEM record as a raw hex dump instead of reassembling
    /// multi-part OEM text messages into a single row.
    #[arg(long)]
    pub raw: bool,
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
}

#[derive(Args)]
pub struct LanArgs {
    #[command(subcommand)]
    pub action: Option<LanAction>,
}

#[derive(Subcommand)]
pub enum LanAction {
    /// Show current LAN configuration for all LAN channels (default).
    Show,
    /// Set LAN parameters on a channel.
    Set(LanSetArgs),
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

    /// Apply changes without the confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct PowerArgs {
    #[command(subcommand)]
    pub action: Option<PowerAction>,
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
