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
        if interface.is_loopback() || interface.is_link_local() {
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
    fn keeps_private_lan_but_rejects_tailscale_ipv4() {
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
            classify_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2))),
            None,
        );
    }

    #[test]
    fn keeps_global_ipv6_but_rejects_tailscale_and_local_ipv6() {
        let tailscale =
            Ipv6Addr::from_str("fd7a:115c:a1e0::1234").unwrap();
        let public =
            Ipv6Addr::from_str("2406:da1c:abcd::1").unwrap();
        let ula = Ipv6Addr::from_str("fd00::1").unwrap();
        let link_local = Ipv6Addr::from_str("fe80::1").unwrap();

        assert_eq!(classify_ip(IpAddr::V6(tailscale)), None);
        assert_eq!(
            classify_ip(IpAddr::V6(public)),
            Some("ipv6"),
        );
        assert_eq!(classify_ip(IpAddr::V6(ula)), None);
        assert_eq!(classify_ip(IpAddr::V6(link_local)), None);
    }
}
