//! View and change BMC LAN configuration.

use ipmi_rs::app::{ChannelMediumType, GetChannelInfo};
use ipmi_rs::connection::Channel;
use ipmi_rs::transport::{
    GetLanConfigParameters, IpAddressSource, Ipv4Address, Ipv6Status, LanConfigParameter,
    LanConfigParameterData, LanConfigParameterRequest, MacAddress, SetLanConfigParameters,
};

use crate::cli::{LanAction, LanArgs, LanSetArgs, LanShowArgs};
use crate::cmd::confirm;
use crate::conn::Conn;
use crate::ui;

const PARAM_VLAN_ID: u8 = 20;
const PARAM_VLAN_PRIORITY: u8 = 21;

pub fn run(conn: &mut Conn, args: &LanArgs) -> Result<(), String> {
    match &args.action {
        None => show(conn, &LanShowArgs::default()),
        Some(LanAction::Show(args)) => show(conn, args),
        Some(LanAction::Set(args)) => set(conn, args),
    }
}

fn show(conn: &mut Conn, args: &LanShowArgs) -> Result<(), String> {
    if let Some(raw) = args.channel {
        let channel = channel(raw)?;
        ensure_lan_channel(conn, channel)?;
        show_channel(conn, channel);
        return Ok(());
    }

    let mut found = false;
    for raw in 0x0..=0x0F {
        let Ok(channel) = channel(raw) else { continue };
        if ensure_lan_channel(conn, channel).is_err() {
            continue;
        }
        found = true;
        show_channel(conn, channel);
    }
    if !found {
        println!("  {}", ui::dim("no LAN channels detected"));
    }
    println!();
    Ok(())
}

fn show_channel(conn: &mut Conn, channel: Channel) {
    ui::header(&format!("LAN channel {}", channel.value()));
    kv_opt(
        "Address source",
        fetch(conn, channel, LanConfigParameter::IpAddressSource),
    );
    kv_opt(
        "IP address",
        fetch(conn, channel, LanConfigParameter::IpAddress),
    );
    kv_opt(
        "Subnet mask",
        fetch(conn, channel, LanConfigParameter::SubnetMask),
    );
    kv_opt(
        "Gateway",
        fetch(conn, channel, LanConfigParameter::DefaultGatewayAddress),
    );
    kv_opt(
        "Gateway MAC",
        fetch(conn, channel, LanConfigParameter::DefaultGatewayMacAddress),
    );
    kv_opt(
        "BMC MAC",
        fetch(conn, channel, LanConfigParameter::MacAddress),
    );
    kv_opt("VLAN", fetch_vlan(conn, channel));
    kv_opt("VLAN priority", fetch_vlan_priority(conn, channel));
    kv_opt(
        "IP versions",
        fetch(conn, channel, LanConfigParameter::Ipv6Ipv4Support),
    );
    kv_opt(
        "IPv6 mode",
        fetch(conn, channel, LanConfigParameter::Ipv6Ipv4AddressingEnables),
    );

    if let Some(status) = ipv6_status(conn, channel) {
        ui::kv(
            "IPv6 capabilities",
            &format!(
                "{} static, {} dynamic; SLAAC {}, DHCPv6 {}",
                status.static_address_max,
                status.dynamic_address_max,
                yes_no(status.slaac_supported),
                yes_no(status.dhcpv6_supported)
            ),
            18,
        );
        for selector in 0..status.static_address_max.min(16) {
            if let Some(value) = fetch_selected(
                conn,
                channel,
                LanConfigParameter::Ipv6StaticAddresses,
                selector,
            ) {
                ui::kv(&format!("IPv6 static {selector}"), &value, 18);
            }
        }
        for selector in 0..status.dynamic_address_max.min(16) {
            if let Some(value) = fetch_selected(
                conn,
                channel,
                LanConfigParameter::Ipv6DynamicAddress,
                selector,
            ) {
                ui::kv(&format!("IPv6 dynamic {selector}"), &value, 18);
            }
        }
    }
}

struct Change {
    parameter: LanConfigParameter,
    request: LanConfigParameterRequest,
    description: String,
}

fn set(conn: &mut Conn, args: &LanSetArgs) -> Result<(), String> {
    let channel = channel(args.channel)?;
    ensure_lan_channel(conn, channel)?;
    let mut changes = Vec::new();

    if let Some(source) = &args.source {
        let value = parse_ip_source(source)
            .ok_or_else(|| format!("invalid --source '{source}' (use static or dhcp)"))?;
        changes.push(change(
            LanConfigParameter::IpAddressSource,
            LanConfigParameterRequest::IpAddressSource(value.into()),
            format!("address source = {value}"),
        ));
    }
    if let Some(value) = &args.ip {
        changes.push(change(
            LanConfigParameter::IpAddress,
            LanConfigParameterRequest::IpAddress(parse_ipv4(value, "--ip")?),
            format!("ip = {value}"),
        ));
    }
    if let Some(value) = &args.netmask {
        changes.push(change(
            LanConfigParameter::SubnetMask,
            LanConfigParameterRequest::SubnetMask(parse_ipv4(value, "--netmask")?),
            format!("netmask = {value}"),
        ));
    }
    if let Some(value) = &args.gateway {
        changes.push(change(
            LanConfigParameter::DefaultGatewayAddress,
            LanConfigParameterRequest::DefaultGatewayAddress(parse_ipv4(value, "--gateway")?),
            format!("gateway = {value}"),
        ));
    }
    if let Some(value) = &args.gateway_mac {
        changes.push(change(
            LanConfigParameter::DefaultGatewayMacAddress,
            LanConfigParameterRequest::DefaultGatewayMacAddress(parse_mac(value)?),
            format!("gateway MAC = {value}"),
        ));
    }
    if let Some(id) = args.vlan_id {
        let enabled = id != 0;
        changes.push(change(
            LanConfigParameter::Other(PARAM_VLAN_ID),
            LanConfigParameterRequest::Raw(encode_vlan(id).to_vec()),
            if enabled {
                format!("VLAN ID = {id}")
            } else {
                "VLAN disabled".to_string()
            },
        ));
    }
    if let Some(priority) = args.vlan_priority {
        changes.push(change(
            LanConfigParameter::Other(PARAM_VLAN_PRIORITY),
            LanConfigParameterRequest::Raw(vec![priority]),
            format!("VLAN priority = {priority}"),
        ));
    }
    if changes.is_empty() {
        return Err("nothing to set; pass a LAN setting option".to_string());
    }

    println!("About to set on channel {}:", args.channel);
    for item in &changes {
        println!("  - {}", item.description);
    }
    if !args.yes && !confirm("Apply these changes?") {
        println!("Aborted.");
        return Ok(());
    }

    let begin = set_in_progress(conn, channel, 0x01);
    if let Err(error) = &begin {
        println!(
            "  {} transaction marker unavailable: {error}",
            ui::yellow("note:")
        );
    }

    let mut failures = Vec::new();
    let mut applied = Vec::new();
    for item in changes {
        let expected = item.request.to_bytes();
        match conn.send_recv(SetLanConfigParameters::from_request(
            channel,
            item.parameter,
            item.request,
        )) {
            Ok(()) => {
                println!("  {} {}", ui::green("set"), item.description);
                applied.push((item.parameter, expected, item.description));
            }
            Err(error) => {
                println!("  {} {}: {error:?}", ui::red("fail"), item.description);
                failures.push(item.description);
            }
        }
    }

    if failures.is_empty() && begin.is_ok() {
        let _ = set_in_progress(conn, channel, 0x02); // optional commit-write
    }
    if begin.is_ok() {
        if let Err(error) = set_in_progress(conn, channel, 0x00) {
            failures.push(format!("could not release Set In Progress: {error}"));
        }
    }

    for (parameter, expected, description) in applied {
        match raw_parameter(conn, channel, parameter, 0) {
            Some(actual) if actual.starts_with(&expected) => {
                println!("  {} {description}", ui::green("verified"));
            }
            Some(actual) => failures.push(format!(
                "verification mismatch for {description}: expected {}, got {}",
                hex(&expected),
                hex(&actual)
            )),
            None => failures.push(format!("could not verify {description}")),
        }
    }

    if failures.is_empty() {
        println!(
            "{} LAN configuration applied and verified",
            ui::green("OK:")
        );
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn change(
    parameter: LanConfigParameter,
    request: LanConfigParameterRequest,
    description: String,
) -> Change {
    Change {
        parameter,
        request,
        description,
    }
}

fn set_in_progress(conn: &mut Conn, channel: Channel, value: u8) -> Result<(), String> {
    conn.send_recv(SetLanConfigParameters::from_request(
        channel,
        LanConfigParameter::SetInProgress,
        LanConfigParameterRequest::SetInProgress(value),
    ))
    .map_err(|error| format!("{error:?}"))
}

fn ensure_lan_channel(conn: &mut Conn, channel: Channel) -> Result<(), String> {
    let info = conn.send_recv(GetChannelInfo::new(channel)).map_err(|e| {
        format!(
            "Get Channel Info for channel {} failed: {e:?}",
            channel.value()
        )
    })?;
    if matches!(
        info.medium_type,
        ChannelMediumType::Lan802_3 | ChannelMediumType::OtherLan
    ) {
        Ok(())
    } else {
        Err(format!("channel {} is not a LAN channel", channel.value()))
    }
}

fn channel(value: u8) -> Result<Channel, String> {
    Channel::new(value).ok_or_else(|| format!("invalid channel number {value}"))
}

fn raw_parameter(
    conn: &mut Conn,
    channel: Channel,
    parameter: LanConfigParameter,
    selector: u8,
) -> Option<Vec<u8>> {
    conn.send_recv(GetLanConfigParameters::new(channel, parameter).with_set_selector(selector))
        .ok()
        .map(|response| response.data)
}

fn fetch(conn: &mut Conn, channel: Channel, parameter: LanConfigParameter) -> Option<String> {
    let response = conn
        .send_recv(GetLanConfigParameters::new(channel, parameter))
        .ok()?;
    render_parameter(response.parse(parameter).ok()?)
}

fn fetch_selected(
    conn: &mut Conn,
    channel: Channel,
    parameter: LanConfigParameter,
    selector: u8,
) -> Option<String> {
    let response = conn
        .send_recv(GetLanConfigParameters::new(channel, parameter).with_set_selector(selector))
        .ok()?;
    render_parameter(response.parse(parameter).ok()?)
}

fn render_parameter(data: LanConfigParameterData) -> Option<String> {
    match data {
        LanConfigParameterData::IpAddress(value)
        | LanConfigParameterData::SubnetMask(value)
        | LanConfigParameterData::DefaultGatewayAddress(value)
        | LanConfigParameterData::BackupGatewayAddress(value) => Some(value.to_string()),
        LanConfigParameterData::MacAddress(value)
        | LanConfigParameterData::DefaultGatewayMacAddress(value)
        | LanConfigParameterData::BackupGatewayMacAddress(value) => Some(value.to_string()),
        LanConfigParameterData::IpAddressSource(value) => Some(value.to_string()),
        LanConfigParameterData::Ipv6Ipv4Support(value) => Some(format!(
            "IPv4; IPv6-only {}, dual-stack {}, IPv6 alerts {}",
            yes_no(value.ipv6_only_supported),
            yes_no(value.dual_stack_supported),
            yes_no(value.ipv6_alerting_supported)
        )),
        LanConfigParameterData::Ipv6Ipv4AddressingEnables(value) => Some(value.to_string()),
        LanConfigParameterData::Ipv6StaticAddresses(value) => Some(format!(
            "{}/{} (enabled {}, source {}, status 0x{:02X})",
            value.address,
            value.prefix_length,
            yes_no(value.enabled),
            value.source_type,
            value.status
        )),
        LanConfigParameterData::Ipv6DynamicAddress(value) => Some(format!(
            "{}/{} (source {}, status 0x{:02X})",
            value.address, value.prefix_length, value.source_type, value.status
        )),
        _ => None,
    }
}

fn ipv6_status(conn: &mut Conn, channel: Channel) -> Option<Ipv6Status> {
    let response = conn
        .send_recv(GetLanConfigParameters::new(
            channel,
            LanConfigParameter::Ipv6Status,
        ))
        .ok()?;
    match response.parse(LanConfigParameter::Ipv6Status).ok()? {
        LanConfigParameterData::Ipv6Status(status) => Some(status),
        _ => None,
    }
}

fn fetch_vlan(conn: &mut Conn, channel: Channel) -> Option<String> {
    let data = raw_parameter(conn, channel, LanConfigParameter::Other(PARAM_VLAN_ID), 0)?;
    let (&low, &high) = (data.first()?, data.get(1)?);
    if high & 0x80 == 0 {
        Some("disabled".to_string())
    } else {
        Some((((high as u16 & 0x0F) << 8) | low as u16).to_string())
    }
}

fn fetch_vlan_priority(conn: &mut Conn, channel: Channel) -> Option<String> {
    raw_parameter(
        conn,
        channel,
        LanConfigParameter::Other(PARAM_VLAN_PRIORITY),
        0,
    )
    .and_then(|data| data.first().map(|value| (value & 0x07).to_string()))
}

fn kv_opt(key: &str, value: Option<String>) {
    ui::kv(key, &value.unwrap_or_else(|| ui::dim("n/a")), 18);
}

fn parse_ipv4(value: &str, option: &str) -> Result<Ipv4Address, String> {
    value
        .parse::<std::net::Ipv4Addr>()
        .map(|address| Ipv4Address(address.octets()))
        .map_err(|_| format!("invalid {option} '{value}'"))
}

fn parse_mac(value: &str) -> Result<MacAddress, String> {
    let parts = value
        .split([':', '-'])
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid --gateway-mac '{value}'"))?;
    let bytes: [u8; 6] = parts
        .try_into()
        .map_err(|_| format!("invalid --gateway-mac '{value}'"))?;
    Ok(MacAddress(bytes))
}

fn encode_vlan(id: u16) -> [u8; 2] {
    [
        id as u8,
        ((id >> 8) as u8 & 0x0F) | if id != 0 { 0x80 } else { 0 },
    ]
}

fn parse_ip_source(value: &str) -> Option<IpAddressSource> {
    match value.to_ascii_lowercase().as_str() {
        "static" => Some(IpAddressSource::Static),
        "dhcp" => Some(IpAddressSource::Dhcp),
        _ => None,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mac_addresses() {
        assert_eq!(
            parse_mac("00:11:22:AA:BB:CC").unwrap().0,
            [0, 0x11, 0x22, 0xAA, 0xBB, 0xCC]
        );
        assert!(parse_mac("00:11:22").is_err());
    }

    #[test]
    fn parses_ipv4_addresses() {
        assert_eq!(parse_ipv4("192.0.2.1", "--ip").unwrap().0, [192, 0, 2, 1]);
        assert!(parse_ipv4("999.1.1.1", "--ip").is_err());
    }

    #[test]
    fn encodes_vlan_id_and_enable_bit() {
        assert_eq!(encode_vlan(100), [100, 0x80]);
        assert_eq!(encode_vlan(0x0ABC), [0xBC, 0x8A]);
        assert_eq!(encode_vlan(0), [0, 0]);
    }
}
