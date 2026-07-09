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
| Inventory | `inventory` (alias `fru`) | BMC identity, decoded FRU data, detected devices |
| Configuration | `lan show` / `lan set` | View / change the BMC network configuration |
| Configuration | `power` | Query and control chassis power |

Output is colored when writing to a terminal; it auto-disables for pipes, when
`NO_COLOR` is set, or with `--no-color`.

## Install

```sh
cargo build --release
# binary at ./target/release/ipmicfg
```

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
ipmicfg sel delete 0x003A          # delete one entry when supported
ipmicfg sel clear                  # erase (asks for confirmation; --yes to skip)

# Inventory (FRU + discovered devices)
ipmicfg inventory

# Network configuration
ipmicfg lan show
ipmicfg lan set --channel 1 --source static \
    --ip 10.0.0.5 --netmask 255.255.255.0 --gateway 10.0.0.1

# Power control (destructive actions confirm first; --... no, use the action's confirm)
ipmicfg power                      # show power state
ipmicfg power on
ipmicfg power off                  # confirms first
ipmicfg power cycle
ipmicfg power soft                 # graceful ACPI shutdown
```

Destructive operations (`power off/cycle/reset/diag`, `sel clear`, `lan set`)
prompt for confirmation; pass `--yes` (or answer `y`) to proceed in scripts.

## Notes & limitations

- Chassis power control and FRU reads are issued as raw IPMI commands, since
  `ipmi-rs` does not yet model them as typed commands.
- `lan set` writes IPv4 parameters (address source, IP, netmask, gateway). MAC and
  IPv6 are read-only here by design.
- Sensor health is derived from IPMI threshold status bits; sensors without
  configured thresholds are shown with a neutral state.

## License

MIT OR Apache-2.0
