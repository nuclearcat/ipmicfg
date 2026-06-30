//! ipmicfg — an intuitive IPMI/BMC command-line tool.
//!
//! Built on top of the pure-Rust [`ipmi-rs`](https://github.com/datdenkikniet/ipmi-rs)
//! library. Provides three pillars of day-to-day BMC work:
//!
//! * **Initial configuration** — `lan show` / `lan set`, `power`
//! * **Monitoring** — `status`, `sensors`, `sel`
//! * **Inventory** — `inventory` (FRU + device discovery)

mod cli;
mod cmd;
mod conn;
mod fru;
mod ui;

use clap::Parser;

use cli::{Cli, Command};
use conn::Conn;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    ui::init_color(cli.no_color);

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {}", ui::red("error:"), e);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let target = cli.conn.target()?;
    let mut conn = Conn::connect(&target, cli.conn.timeout())
        .map_err(|e| format!("failed to connect: {e}"))?;

    match &cli.command {
        Command::Status => cmd::status::run(&mut conn),
        Command::Sensors(args) => cmd::sensors::run(&mut conn, args),
        Command::Sel(args) => cmd::sel::run(&mut conn, args),
        Command::Inventory => cmd::inventory::run(&mut conn),
        Command::Lan(args) => cmd::lan::run(&mut conn, args),
        Command::Power(args) => cmd::power::run(&mut conn, args),
    }
}
