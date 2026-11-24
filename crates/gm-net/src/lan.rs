//! The addresses this machine is reachable at, and the hosts worth sweeping.
//! Port of v1 `network.js` `lanAddresses` / `sweepTargets`, netmask-aware.

use std::collections::HashSet;
use std::net::Ipv4Addr;

/// One IPv4 address on a non-loopback interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAddress {
    /// Interface name, e.g. "en0", "Wi-Fi", "wlan0".
    pub name: String,
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub prefix: u8,
    /// VPN, hypervisor, container or tunnel adapter (v1 `VIRTUAL_ADAPTER`).
    pub is_virtual: bool,
}

/// v1: /vethernet|virtualbox|vbox|vmware|vmnet|hyper-v|wsl|docker|br-|veth|tailscale|
/// zerotier|wireguard|utun|tun\d|tap\d|vpn|npcap|loopback|bluetooth/i
pub fn is_virtual_adapter(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "vethernet",
        "virtualbox",
        "vbox",
        "vmware",
        "vmnet",
        "hyper-v",
        "wsl",
        "docker",
        "br-",
        "veth",
        "tailscale",
        "zerotier",
        "wireguard",
        "utun",
        "vpn",
        "npcap",
        "loopback",
        "bluetooth",
    ];
    let lower = name.to_ascii_lowercase();
    WORDS.iter().any(|w| lower.contains(w))
        || has_word_digit(&lower, "tun")
        || has_word_digit(&lower, "tap")
}

/// `prefix` immediately followed by an ASCII digit, anywhere in `s`.
fn has_word_digit(s: &str, prefix: &str) -> bool {
    s.match_indices(prefix).any(|(i, _)| {
        s.as_bytes()
            .get(i + prefix.len())
            .is_some_and(|b| b.is_ascii_digit())
    })
}

/// v1: /wi-?fi|wlan|wireless|en0|eth|ethernet/i → 0 (preferred), else 1.
fn wireless_rank(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    let hit = ["wifi", "wi-fi", "wlan", "wireless", "en0", "eth"]
        .iter()
        .any(|w| lower.contains(w));
    if hit {
        0
    } else {
        1
    }
}

/// v1: 192.168.* → 0, 10.* → 1, 172.* → 2, else 3.
fn range_rank(a: Ipv4Addr) -> u8 {
    match a.octets() {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        [172, ..] => 2,
        _ => 3,
    }
}

/// Sort like v1: real before virtual, Wi-Fi/Ethernet first, then private range.
pub fn sort_lan_addresses(found: &mut [LanAddress]) {
    found.sort_by_key(|a| (a.is_virtual, wireless_rank(&a.name), range_rank(a.address)));
}

/// Every non-loopback, non-link-local IPv4 address, best first.
pub fn lan_addresses() -> Vec<LanAddress> {
    let mut found: Vec<LanAddress> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| {
            let if_addrs::IfAddr::V4(v4) = &iface.addr else {
                return None;
            };
            if iface.is_loopback() || v4.ip.is_loopback() || v4.ip.is_link_local() {
                return None;
            }
            Some(LanAddress {
                is_virtual: is_virtual_adapter(&iface.name),
                name: iface.name.clone(),
                address: v4.ip,
                netmask: v4.netmask,
                prefix: v4.prefixlen,
            })
        })
        .collect();
    sort_lan_addresses(&mut found);
    found
}

/// The address to advertise, or 127.0.0.1 with no LAN.
pub fn primary_lan_address() -> Ipv4Addr {
    lan_addresses()
        .first()
        .map(|a| a.address)
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

/// The hosts on one adapter's subnet: the real subnet when it is /24 or
/// smaller, otherwise the /24 around the address (a /16 campus network would
/// be 65k probes). Network and broadcast addresses are skipped.
pub fn subnet_hosts(address: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    let prefix = prefix.clamp(24, 32);
    if prefix >= 31 {
        // /31 point-to-point has two usable hosts; /32 has only itself.
        let mask = if prefix == 32 { u32::MAX } else { !1u32 };
        let base = u32::from(address) & mask;
        return (base..=base | !mask).map(Ipv4Addr::from).collect();
    }
    let mask = u32::MAX << (32 - prefix);
    let network = u32::from(address) & mask;
    let broadcast = network | !mask;
    ((network + 1)..broadcast).map(Ipv4Addr::from).collect()
}

/// Every host worth probing around each real (non-virtual) adapter, deduped,
/// in adapter order.
pub fn sweep_targets(addresses: &[LanAddress]) -> Vec<Ipv4Addr> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in addresses.iter().filter(|a| !a.is_virtual) {
        for host in subnet_hosts(entry.address, entry.prefix) {
            if seen.insert(host) {
                out.push(host);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: &str, ip: [u8; 4], prefix: u8) -> LanAddress {
        LanAddress {
            name: name.into(),
            address: Ipv4Addr::from(ip),
            netmask: Ipv4Addr::from(u32::MAX << (32 - prefix as u32)),
            prefix,
            is_virtual: is_virtual_adapter(name),
        }
    }

    #[test]
    fn virtual_filter_matches_v1() {
        for name in [
            "vEthernet (WSL)",
            "VirtualBox Host-Only",
            "vboxnet0",
            "VMware Network Adapter VMnet8",
            "vmnet1",
            "docker0",
            "br-1a2b",
            "veth12ab",
            "tailscale0",
            "ZeroTier One",
            "utun3",
            "tun0",
            "tap1",
            "Npcap Loopback",
            "Bluetooth Network Connection",
            "NordVPN",
            "wsl",
            "Hyper-V",
        ] {
            assert!(is_virtual_adapter(name), "{name} should be virtual");
        }
        for name in [
            "en0",
            "Wi-Fi",
            "wlan0",
            "eth0",
            "Ethernet 2",
            "enp3s0",
            "wlp2s0",
            "tunnel",
        ] {
            assert!(!is_virtual_adapter(name), "{name} should be real");
        }
    }

    #[test]
    fn ordering_matches_v1() {
        let mut list = vec![
            addr("docker0", [172, 17, 0, 1], 16),
            addr("enp3s0", [10, 0, 0, 5], 24),
            addr("wlan0", [10, 1, 2, 3], 24),
            addr("eth1", [192, 168, 1, 9], 24),
        ];
        sort_lan_addresses(&mut list);
        let names: Vec<_> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["eth1", "wlan0", "enp3s0", "docker0"]);
    }

    #[test]
    fn subnet_capping() {
        let a = subnet_hosts(Ipv4Addr::new(10, 20, 30, 40), 16);
        assert_eq!(a.len(), 254);
        assert_eq!(a[0], Ipv4Addr::new(10, 20, 30, 1));
        assert_eq!(a[253], Ipv4Addr::new(10, 20, 30, 254));
        let b = subnet_hosts(Ipv4Addr::new(192, 168, 1, 70), 26);
        assert_eq!(b.len(), 62);
        assert_eq!(b[0], Ipv4Addr::new(192, 168, 1, 65));
        assert_eq!(
            subnet_hosts(Ipv4Addr::new(1, 2, 3, 4), 32),
            vec![Ipv4Addr::new(1, 2, 3, 4)]
        );
        assert_eq!(subnet_hosts(Ipv4Addr::new(1, 2, 3, 5), 31).len(), 2);
    }

    #[test]
    fn sweep_skips_virtual_and_dedupes() {
        let list = vec![
            addr("wlan0", [192, 168, 1, 9], 24),
            addr("eth0", [192, 168, 1, 10], 24),
            addr("docker0", [172, 17, 0, 1], 16),
        ];
        assert_eq!(sweep_targets(&list).len(), 254);
    }
}
