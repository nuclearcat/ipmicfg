# TODO

This roadmap prioritizes features that make `ipmicfg` safer to use, easier to
automate, and more complete for day-to-day server management.

## P0 — Safety and automation

- [ ] Add secure remote credential handling.
  - Accept the username separately from the host.
  - Prompt for a password without echoing it.
  - Support a password file or file descriptor for automation.
  - Optionally support an environment variable, with appropriate warnings.
  - Keep `USER:PASS@HOST` compatibility temporarily, but deprecate passwords on
    the command line because they can leak through shell history and process
    listings.

- [ ] Add machine-readable output.
  - Provide a global `--output table|json|csv` option.
  - Define stable field names and avoid ANSI escapes outside table output.
  - Cover `status`, `sensors`, `sel`, `inventory`, `lan show`, and power status.

- [ ] Add monitoring-oriented exit codes.
  - Add `sensors --fail-on warn|critical`.
  - Return a distinct non-zero status for unhealthy readings versus transport or
    protocol errors.
  - Document the exit-code contract for scripts and monitoring systems.

- [ ] Make destructive power commands non-interactive when explicitly requested.
  - Add `--yes` to `power off`, `power cycle`, `power reset`, and `power diag`.
  - Keep confirmation as the default.
  - Fix the README claim that all destructive operations already accept
    `--yes`.

- [ ] Avoid interactive confirmation when stdin is not a terminal unless
  `--yes` was supplied; fail with a clear message instead.

## P1 — Monitoring completeness

- [x] Decode and display discrete sensor readings and asserted states.
  - [x] Include conditions such as PSU failure, drive fault, redundancy loss, and
    chassis intrusion.
  - [x] Include discrete sensors in health totals.
  - [x] Preserve raw event bits when no friendly decoder is available.

- [x] Improve sensor selection and inspection.
  - [x] Add `--name <PATTERN>` filtering.
  - [x] Add `--state ok|warn|critical|unknown` filtering.
  - [x] Add `--thresholds` to display configured limits.
  - [x] Add `--watch <INTERVAL>` for repeated readings.
  - [x] Report individual sensor read failures instead of silently rendering every
    error as `n/a`; optionally provide `--verbose` diagnostics.

- [x] Add useful SEL filters.
  - [x] Support `--since`, `--until`, `--sensor`, `--severity`, and `--limit`.
  - [x] Consider `--follow` where the BMC and transport make polling practical.
  - [x] Add optional deletion of an individual SEL entry when supported.
  - [x] Decode Fujitsu iRMC OEM records through the controller's F5 43 long-text
    interface by default, with a local-decoding opt-out and optional
    request/response diagnostics.
  - [x] Decode standard System Firmware Progress extension codes and classify
    progress checkpoints as informational.
  - [x] Distinguish Dell OEM diagnostic companion records from Link Tuning
    failures and preserve their service-data bytes.
  - Preserve raw fields in JSON output, including OEM data.

- [x] Expand `status` into a health summary.
  - [x] Include sensor OK/WARN/CRIT counts.
  - [x] Include SEL entry count, overflow state, and recent critical events.
  - [x] Keep partial results when one subsystem is unavailable.

## P1 — Inventory and configuration

- [x] Read every logical FRU device discovered through SDR records, not only FRU ID 0.
  - [x] Display each device's locator, ID, product, board, and chassis data.
  - [x] Add `inventory --fru-id <ID>`.
  - [x] Add raw FRU image export for troubleshooting and backup.

- [x] Add boot-device override commands.
  - [x] Support one-shot PXE, disk, optical media, and BIOS/setup boot.
  - [x] Support clearing an override.
  - [x] Clearly distinguish one-shot and persistent settings.
  - [x] Show the currently configured boot flags.

- [x] Complete LAN configuration support.
  - [x] Add `lan show --channel <N>` instead of always probing every channel.
  - [x] Display and configure VLAN ID and priority.
  - [x] Display and configure gateway MAC where supported.
  - [x] Display IPv6 information; configuration can follow once transport support is
    reliable.
  - [x] Verify values after writes and report mismatches.
  - [x] Improve transaction cleanup when a write fails after setting "Set In
    Progress".

- [x] Add BMC user management.
  - [x] List users and channel access.
  - [x] Enable or disable an account.
  - [x] Set privilege levels.
  - [x] Set or rotate passwords without exposing them on the command line.
  - [x] Require confirmation for lockout-prone changes.

## P2 — Advanced operations

- [x] Add a guarded raw IPMI command interface:
  `raw <netfn> <command> [DATA...]`.
  - [x] Parse hexadecimal and decimal bytes consistently.
  - [x] Print completion codes, response bytes, and an ASCII view.
  - [x] Require confirmation for every arbitrary request unless `--yes` is
    supplied, because unknown OEM commands cannot be classified safely.

- [ ] Add Serial over LAN support if `ipmi-rs` exposes the required session and
  payload APIs.

- [ ] Add watchdog inspection and configuration.

- [ ] Add retry and backoff controls for unreliable remote BMCs.
  - Keep the existing per-response timeout.
  - Add a bounded retry count and delay.
  - Do not retry destructive requests unless their semantics are known to be
    safe.

- [ ] Consider Platform Event Filtering and DCMI commands where supported by the
  library and real hardware.

## Testing and maintainability

- [ ] Introduce a mockable connection/transport interface so command handlers can
  be tested with captured IPMI responses.

- [ ] Add CLI parsing tests, especially for host/port syntax, IPv6, confirmation
  flags, and invalid combinations.

- [ ] Add FRU parser tests covering:
  - Truncated and malformed areas.
  - Checksums and area lengths.
  - 8-bit, 6-bit ASCII, BCD-plus, and binary fields.
  - Board manufacturing dates.
  - Multiple FRU devices.

- [x] Add sensor classification and discrete-event decoding tests.

- [ ] Add LAN parsing and partial-write failure tests.

- [ ] Add SEL pagination tests, including malformed next-record IDs and records
  added or removed during iteration.

- [ ] Add CI for formatting, strict Clippy, tests, and a release build.

## Documentation and packaging

- [ ] Add the existing `identify` and `bmc` commands to the README feature table
  and examples.

- [ ] Correct the README's IPv6 wording: IPv6 is currently neither configured nor
  displayed.

- [ ] Replace the unfinished power-control comment in the README example.

- [ ] Verify that the Cargo `repository` URL points to the `ipmicfg` repository
  rather than the upstream `ipmi-rs` project.

- [ ] Add shell completions for Bash, Zsh, Fish, and PowerShell.

- [ ] Add a man page and installation instructions for packaged releases.

- [ ] Document security considerations for RMCP/IPMI over LAN, credentials, and
  privilege levels.

## Suggested implementation order

1. Secure credential input.
2. Structured output and health-aware exit codes.
3. Discrete sensor decoding.
4. Power `--yes` and non-TTY confirmation behavior.
5. Multi-FRU inventory.
6. SEL filters and richer status output.
7. Boot overrides and expanded LAN configuration.
8. User management and advanced operations.
