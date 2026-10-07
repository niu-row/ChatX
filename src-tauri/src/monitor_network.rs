use if_addrs::get_if_addrs;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv6Addr},
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorEndpoint {
    pub kind: &'static str,
    pub family: &'static str,
    pub interface: String,
    pub host: String,
    pub url: String,
}

pub fn discover_monitor_endpoints(port: u16) -> Vec<MonitorEndpoint> {
    let mut seen = BTreeSet::new();
    let mut endpoints = Vec::new();
    let Ok(interfaces) = get_if_addrs() else {
        return endpoints;
    };

    for interface in interfaces {
        if interface.is_loopback()
            || interface.is_link_local()
            || is_virtual_interface_name(&interface.name)
        {
            continue;
        }
        let ip = interface.ip();
        let Some(kind) = classify_ip(ip) else {
            continue;
        };
        let host = ip.to_string();
        if !seen.insert((kind, host.clone())) {
            continue;
        }
        let (family, url) = match ip {
            IpAddr::V4(_) => (
                "ipv4",
                format!("wss://{host}:{port}/v1/ws/monitor"),
            ),
            IpAddr::V6(_) => (
                "ipv6",
                format!("wss://[{host}]:{port}/v1/ws/monitor"),
            ),
        };
        endpoints.push(MonitorEndpoint {
            kind,
            family,
            interface: interface.name,
            host,
            url,
        });
    }

    endpoints.sort_by(|a, b| {
        endpoint_rank(a.kind)
            .cmp(&endpoint_rank(b.kind))
            .then_with(|| a.host.cmp(&b.host))
    });
    endpoints
}

fn endpoint_rank(kind: &str) -> u8 {
    match kind {
        "lan" => 0,
        "ipv6" => 1,
        _ => 2,
    }
}

fn is_virtual_interface_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    [
        "vethernet",
        "wsl",
        "docker",
        "virtualbox host-only",
        "vmware network adapter",
        "hyper-v",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn classify_ip(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(ip) if ip.is_private() => Some("lan"),
        IpAddr::V6(ip) if is_global_v6(ip) => Some("ipv6"),
        _ => None,
    }
}

fn is_global_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && first & 0xfe00 != 0xfc00
        && first & 0xffc0 != 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::Ipv4Addr,
        str::FromStr,
    };

    #[test]
    fn ignores_virtual_host_only_interfaces() {
        assert!(is_virtual_interface_name("vEthernet (WSL)"));
        assert!(is_virtual_interface_name("VMware Network Adapter VMnet8"));
        assert!(!is_virtual_interface_name("以太网"));
        assert!(!is_virtual_interface_name("Wi-Fi"));
    }

    #[test]
    fn keeps_private_lan_and_ignores_non_lan_ipv4() {
        assert_eq!(
            classify_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            Some("lan"),
        );
        assert_eq!(
            classify_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))),
            Some("lan"),
        );
        assert_eq!(
            classify_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))),
            Some("lan"),
        );
        assert_eq!(
            classify_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
            None,
        );
    }

    #[test]
    fn keeps_global_ipv6_and_ignores_local_ipv6() {
        let public =
            Ipv6Addr::from_str("2406:da1c:abcd::1").unwrap();
        let ula = Ipv6Addr::from_str("fd00::1").unwrap();
        let link_local = Ipv6Addr::from_str("fe80::1").unwrap();

        assert_eq!(
            classify_ip(IpAddr::V6(public)),
            Some("ipv6"),
        );
        assert_eq!(classify_ip(IpAddr::V6(ula)), None);
        assert_eq!(classify_ip(IpAddr::V6(link_local)), None);
    }
}
