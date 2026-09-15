//! ipmicfg — an intuitive IPMI/BMC command-line tool.
//!
//! Built on top of the pure-Rust [`ipmi-rs`](https://github.com/datdenkikniet/ipmi-rs)
//! library. Provides three pillars of day-to-day BMC work:
//!
//! * **Initial configuration** — `lan`, `boot`, `user`, `power`, `identify`, `bmc`
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
    restore_default_sigpipe();
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

/// Rust masks `SIGPIPE` at startup, which turns a closed stdout (`ipmicfg sensors | head`)
/// into a panic on the next print. Restore the default disposition so the process exits
/// quietly the way every other command-line tool does.
fn restore_default_sigpipe() {
    // SAFETY: `signal` with `SIG_DFL` is async-signal-safe and runs before any threads
    // are spawned; it only resets a disposition Rust's runtime set for us.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
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
        Command::Inventory(args) => cmd::inventory::run(&mut conn, args),
        Command::Lan(args) => cmd::lan::run(&mut conn, args),
        Command::Boot(args) => cmd::boot::run(&mut conn, args),
        Command::User(args) => cmd::user::run(&mut conn, args),
        Command::Power(args) => cmd::power::run(&mut conn, args),
        Command::Identify(args) => cmd::identify::run(&mut conn, args),
        Command::Bmc(args) => cmd::bmc::run(&mut conn, args),
        Command::Raw(args) => cmd::raw::run(&mut conn, args),
    }
}
