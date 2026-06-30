//! `lan` — view and change the BMC LAN configuration.

use ipmi_rs::app::{ChannelMediumType, GetChannelInfo};
use ipmi_rs::connection::Channel;
use ipmi_rs::transport::{
    GetLanConfigParameters, IpAddressSource, Ipv4Address, LanConfigParameter,
    LanConfigParameterData, LanConfigParameterRequest, SetLanConfigParameters,
};

use crate::cli::{LanAction, LanArgs, LanSetArgs};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui;

pub fn run(conn: &mut Conn, args: &LanArgs) -> Result<(), String> {
    match args.action.as_ref().unwrap_or(&LanAction::Show) {
        LanAction::Show => show(conn),
        LanAction::Set(set_args) => set(conn, set_args),
    }
}

fn show(conn: &mut Conn) -> Result<(), String> {
    let mut found = false;
    for raw in 0x0..=0xFu8 {
        let channel = match Channel::new(raw) {
            Some(c) => c,
            None => continue,
        };
        let info = match conn.send_recv(GetChannelInfo::new(channel)) {
            Ok(i) => i,
            Err(_) => continue,
        };
        if !matches!(
            info.medium_type,
            ChannelMediumType::Lan802_3 | ChannelMediumType::OtherLan
        ) {
            continue;
        }

        found = true;
        ui::header(&format!("LAN channel {}", info.channel.value()));
        kv_opt(
            "Address source",
            fetch(conn, channel, LanConfigParameter::IpAddressSource),
        );
        kv_opt("IP address", fetch(conn, channel, LanConfigParameter::IpAddress));
        kv_opt(
            "Subnet mask",
            fetch(conn, channel, LanConfigParameter::SubnetMask),
        );
        kv_opt(
            "Gateway",
            fetch(conn, channel, LanConfigParameter::DefaultGatewayAddress),
        );
        kv_opt(
            "MAC address",
            fetch(conn, channel, LanConfigParameter::MacAddress),
        );
    }

    if !found {
        println!("  {}", ui::dim("no LAN channels detected"));
    }
    println!();
    Ok(())
}

fn set(conn: &mut Conn, args: &LanSetArgs) -> Result<(), String> {
    let channel = Channel::new(args.channel)
        .ok_or_else(|| format!("invalid channel number {}", args.channel))?;

    // Validate and collect the requested changes up front.
    let mut changes: Vec<(LanConfigParameter, LanConfigParameterRequest, String)> = Vec::new();

    if let Some(src) = &args.source {
        let source = parse_ip_source(src)
            .ok_or_else(|| format!("invalid --source '{src}' (use 'static' or 'dhcp')"))?;
        changes.push((
            LanConfigParameter::IpAddressSource,
            LanConfigParameterRequest::IpAddressSource(u8::from(source)),
            format!("address source = {source}"),
        ));
    }
    if let Some(ip) = &args.ip {
        let v = parse_ipv4(ip).ok_or_else(|| format!("invalid --ip '{ip}'"))?;
        changes.push((
            LanConfigParameter::IpAddress,
            LanConfigParameterRequest::IpAddress(v),
            format!("ip = {ip}"),
        ));
    }
    if let Some(mask) = &args.netmask {
        let v = parse_ipv4(mask).ok_or_else(|| format!("invalid --netmask '{mask}'"))?;
        changes.push((
            LanConfigParameter::SubnetMask,
            LanConfigParameterRequest::SubnetMask(v),
            format!("netmask = {mask}"),
        ));
    }
    if let Some(gw) = &args.gateway {
        let v = parse_ipv4(gw).ok_or_else(|| format!("invalid --gateway '{gw}'"))?;
        changes.push((
            LanConfigParameter::DefaultGatewayAddress,
            LanConfigParameterRequest::DefaultGatewayAddress(v),
            format!("gateway = {gw}"),
        ));
    }

    if changes.is_empty() {
        return Err("nothing to set; pass --source/--ip/--netmask/--gateway".to_string());
    }

    println!("About to set on channel {}:", args.channel);
    for (_, _, desc) in &changes {
        println!("  - {desc}");
    }
    if !args.yes && !confirm("Apply these changes?") {
        println!("Aborted.");
        return Ok(());
    }

    // Bracket the writes with Set In Progress markers (best effort).
    let _ = conn.send_recv(SetLanConfigParameters::from_request(
        channel,
        LanConfigParameter::SetInProgress,
        LanConfigParameterRequest::SetInProgress(0x01),
    ));

    let mut errors = Vec::new();
    for (param, request, desc) in changes {
        match conn.send_recv(SetLanConfigParameters::from_request(channel, param, request)) {
            Ok(_) => println!("  {} {desc}", ui::green("set")),
            Err(e) => {
                println!("  {} {desc}: {e:?}", ui::red("fail"));
                errors.push(desc);
            }
        }
    }

    let _ = conn.send_recv(SetLanConfigParameters::from_request(
        channel,
        LanConfigParameter::SetInProgress,
        LanConfigParameterRequest::SetInProgress(0x00),
    ));

    if errors.is_empty() {
        println!("{}", ui::green("Done. Run `ipmicfg lan show` to verify."));
        Ok(())
    } else {
        Err(format!("{} parameter(s) failed to apply", errors.len()))
    }
}

/// Fetch a single LAN parameter and render it as a string, if available.
fn fetch(conn: &mut Conn, channel: Channel, param: LanConfigParameter) -> Option<String> {
    let resp = conn.send_recv(GetLanConfigParameters::new(channel, param)).ok()?;
    match resp.parse(param).ok()? {
        LanConfigParameterData::IpAddress(v) => Some(v.to_string()),
        LanConfigParameterData::SubnetMask(v) => Some(v.to_string()),
        LanConfigParameterData::DefaultGatewayAddress(v) => Some(v.to_string()),
        LanConfigParameterData::MacAddress(v) => Some(v.to_string()),
        LanConfigParameterData::IpAddressSource(v) => Some(v.to_string()),
        _ => None,
    }
}

/// Print a key/value line, showing a dim "n/a" when the value is missing.
fn kv_opt(key: &str, value: Option<String>) {
    let rendered = value.unwrap_or_else(|| ui::dim("n/a"));
    ui::kv(key, &rendered, 16);
}

fn parse_ipv4(value: &str) -> Option<Ipv4Address> {
    let mut parts = [0u8; 4];
    let mut index = 0;
    for part in value.split('.') {
        if index >= 4 {
            return None;
        }
        parts[index] = part.parse::<u8>().ok()?;
        index += 1;
    }
    if index != 4 {
        return None;
    }
    Some(Ipv4Address(parts))
}

fn parse_ip_source(value: &str) -> Option<IpAddressSource> {
    match value.to_ascii_lowercase().as_str() {
        "static" => Some(IpAddressSource::Static),
        "dhcp" => Some(IpAddressSource::Dhcp),
        _ => None,
    }
}
