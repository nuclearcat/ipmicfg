# ipmicfg

An intuitive command-line IPMI/BMC tool for **initial configuration**, **monitoring**,
and **inventory**, built on the pure-Rust [`ipmi-rs`](https://github.com/datdenkikniet/ipmi-rs)
library. No `ipmitool`/OpenIPMI userspace required — it talks to the kernel device
directly or over the network.

## Features

| Pillar | Command | What it does |
| --- | --- | --- |
| Monitoring | `status` | One-screen overview: power, identity, capabilities |
| Monitoring | `sensors` | Live sensor readings with OK/WARN/CRIT coloring |
| Monitoring | `sel` | Read, summarize and clear the System Event Log |
| Diagnostics | `raw` | Send a guarded raw IPMI request and inspect its response |
| Inventory | `inventory` (alias `fru`) | BMC identity, all logical FRUs, raw export, detected devices |
| Configuration | `lan show` / `lan set` | View / change the BMC network configuration |
| Configuration | `boot` | Inspect or set one-shot/persistent boot overrides |
| Configuration | `user` | List users and administer names, access and passwords |
| Configuration | `power` | Query and control chassis power |
| Maintenance | `identify` (alias `blink`) | Blink or control the chassis identify LED |
| Maintenance | `bmc` (alias `mc`) | Run the BMC self-test or issue a warm/cold BMC reset |

Output is colored when writing to a terminal; it auto-disables for pipes, when
`NO_COLOR` is set, or with `--no-color`.

## Install

Release packages are built for 64-bit Ubuntu 24.04 LTS, Ubuntu 26.04 LTS, and
Fedora 44. Download the distro-specific `.deb` or `.rpm` from the matching
[GitHub release](https://github.com/nuclearcat/ipmicfg/releases), then install it:

```sh
# Ubuntu
sudo apt install ./ipmicfg_*_amd64.deb

# Fedora
sudo dnf install ./ipmicfg-*.x86_64.rpm
```

To build from source instead:

```sh
cargo build --release
# binary at ./target/release/ipmicfg
```

Maintainers should follow [RELEASING.md](RELEASING.md) to publish a release.

## Connecting

By default ipmicfg uses the **local** BMC via the kernel device `/dev/ipmi0`
(load the `ipmi_devintf` kernel module if it is missing). Local access usually
needs root.

To manage a **remote** BMC over the network (RMCP/RMCP+ / "IPMI over LAN"), use
`-H`:

```sh
ipmicfg -H root:calvin@10.0.0.5        sensors   # default UDP port 623
ipmicfg -H admin:secret@10.0.0.5:623   status
```

The remote password is currently supplied on the command line, so it may be
recorded in shell history or exposed in the process list. Use a dedicated,
least-privileged BMC account and avoid putting production credentials in shared
scripts. IPMI-over-LAN security depends on the authentication and cipher suites
supported and enabled by the BMC; use it only on a trusted management network.

Global options:

```
-H, --host <USER:PASS@HOST[:PORT]>  Remote BMC over LAN
-d, --device <PATH>                 Local device (default /dev/ipmi0)
    --timeout-ms <MS>               Response timeout (default 2000)
    --no-color                      Disable colored output
```

## Examples

```sh
# Overview
ipmicfg status

# All sensors, or just temperatures and fans
ipmicfg sensors
ipmicfg sensors --type Temp --type Fan
ipmicfg sensors --all              # discrete sensors include asserted states
ipmicfg sensors --name CPU --state critical
ipmicfg sensors --thresholds
ipmicfg sensors --watch 5

# System Event Log
ipmicfg sel                        # list
ipmicfg sel info                   # summary
ipmicfg sel --since 2026-07-01T00:00:00Z --severity critical --limit 20
ipmicfg sel --sensor PSU --follow
ipmicfg sel --no-oem-decode          # skip vendor queries and use local decoding only
ipmicfg sel delete 0x003A          # delete one entry when supported
ipmicfg sel clear                  # erase (asks for confirmation; --yes to skip)

# Ask Fujitsu iRMC firmware to translate its own OEM SEL records (F5 43)
ipmicfg sel decode 0x009E 0x00A5 0x00A6 0x00A7
ipmicfg sel decode 0x00A6 --debug  # also print each request/response frame

# Inventory (all logical FRUs + discovered devices)
ipmicfg inventory
ipmicfg inventory --fru-id 0 --raw primary-fru.bin

# Network configuration
ipmicfg lan show
ipmicfg lan show --channel 1
ipmicfg lan set --channel 1 --source static \
    --ip 10.0.0.5 --netmask 255.255.255.0 --gateway 10.0.0.1
ipmicfg lan set --channel 1 --vlan-id 100 --vlan-priority 3

# Boot override
ipmicfg boot
ipmicfg boot set pxe                 # one boot
ipmicfg boot set disk --persistent   # confirms first
ipmicfg boot clear

# BMC users (mutations confirm first)
ipmicfg user list --channel 1
ipmicfg user privilege 3 administrator --channel 1
ipmicfg user password 3              # hidden prompt; no password in argv

# Power control (destructive actions confirm first)
ipmicfg power                      # show power state
ipmicfg power on
ipmicfg power off                  # confirms first
ipmicfg power cycle
ipmicfg power soft                 # graceful ACPI shutdown

# Chassis identify LED
ipmicfg identify                   # blink for 15 seconds
ipmicfg identify on 60             # blink for 60 seconds
ipmicfg identify force             # keep on until explicitly disabled
ipmicfg identify off

# BMC maintenance (this does not reset the host)
ipmicfg bmc selftest
ipmicfg bmc reset                  # cold reset; confirms first
ipmicfg bmc reset --warm --yes

# Guarded low-level diagnostics (hexadecimal and decimal bytes are accepted)
ipmicfg raw 0x2e 0xf5 0x80 0x28 0x00 0x43 0xa6 0x00 0x00 0x38 --yes
```

Destructive or lockout-prone operations prompt for confirmation. `sel clear`,
`sel delete`, `lan set`, persistent boot overrides, user mutations, and `bmc
reset` accept `--yes` for deliberate automation. The disruptive power actions
(`off`, `cycle`, `reset`, and `diag`) always prompt; they do not currently have a
`--yes` option. The generic `raw` interface also always confirms unless `--yes`
is supplied because an arbitrary vendor command may change controller or host
state.

## Notes & limitations

- Chassis power control and FRU reads are issued as raw IPMI commands, since
  `ipmi-rs` does not yet model them as typed commands.
- `lan show` reports IPv4, VLAN, gateway MAC and supported IPv6 addresses.
  `lan set` configures IPv4, gateway MAC and VLAN parameters; IPv6 remains
  display-only.
- User passwords are prompted without echo or read from `--password-file` and
  are limited to the broadly compatible 16-byte IPMI password form.
- Logical FRUs are decoded through Get FRU Inventory commands. Physical I2C FRU
  locators are listed, but direct EEPROM access is not attempted.
- Threshold-sensor health is derived from IPMI threshold status bits. Discrete
  sensors are classified from their SDR event-reading type and decoded asserted
  states. Asserted OEM/vendor-specific states with unknown semantics are shown
  as neutral rather than guessed to be failures; their raw state mask remains
  visible with `sensors --all`.
- SEL System Firmware Progress records use the standard IPMI extension table to
  report checkpoints such as memory initialization and starting the operating
  system boot process. Progress checkpoints and ordinary power-off/down state
  transitions are informational rather than warnings; deasserted power events
  are shown as cleared conditions.
- Sensor scans print progress on an interactive terminal. `--verbose` identifies
  each request, and `--timeout-ms` can shorten delays caused by unsupported or
  unresponsive sensors.
- On detected Fujitsu controllers, normal `sel` output uses iRMC OEM command F5
  43 by default for vendor-defined sensor and event types. `--no-oem-decode`
  disables the additional requests and restores the generic local rendering.
- `sel decode` exposes the same Fujitsu command for explicit record inspection,
  including
  bounded response parsing, long-text pagination, vendor severity, and the CSS
  (customer-replaceable component) flag. It does not require `ipmitool` or
  FreeIPMI.
- Dell OEM diagnostic companion records are identified separately from Link
  Tuning events, shown as informational service context, and retain their raw
  diagnostic bytes in the description.

## License

MIT OR Apache-2.0
