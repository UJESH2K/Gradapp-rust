//! The GradApp 2 LAN beacon: a small JSON datagram ([`BeaconPacket`]) a host
//! multicasts every ~2 s so other machines find it in under a second, without
//! sweeping the subnet and without assuming the network is a /24.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use gm_types::{BeaconPacket, BEACON_GROUP, BEACON_PORT};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::lan::{lan_addresses, LanAddress};

/// `BeaconPacket::magic`.
pub const BEACON_MAGIC: &str = "gradmesh";
/// Datagrams bigger than this are not beacons.
pub const MAX_BEACON_BYTES: usize = 2048;
/// What a listener sends to make beacon hosts announce now instead of at
/// their next tick. Not a [`BeaconPacket`], so old listeners ignore it.
pub const BEACON_QUERY: &str = r#"{"magic":"gradmesh-query"}"#;
/// A host answers queries at most this often.
const QUERY_HOLDOFF: Duration = Duration::from_millis(250);
/// Default send interval.
pub const BEACON_INTERVAL: Duration = Duration::from_secs(2);

/// Decode and validate one datagram.
pub fn parse_beacon(bytes: &[u8]) -> Option<BeaconPacket> {
    if bytes.len() > MAX_BEACON_BYTES {
        return None;
    }
    let packet: BeaconPacket = serde_json::from_slice(bytes).ok()?;
    (packet.magic == BEACON_MAGIC && !packet.mesh_id.is_empty() && packet.coordinator_port != 0).then_some(packet)
}

/// Build a packet with the magic filled in.
pub fn beacon_packet(
    mesh_id: &str,
    mesh_name: Option<&str>,
    version: &str,
    nodes_active: u32,
    coordinator_port: u16,
    web_port: u16,
) -> BeaconPacket {
    BeaconPacket {
        magic: BEACON_MAGIC.into(),
        mesh_id: mesh_id.into(),
        mesh_name: mesh_name.map(str::to_string),
        coordinator_port,
        web_port,
        version: version.into(),
        protocol: gm_types::MESH_PROTOCOL,
        nodes_active,
    }
}

/// A socket listening for beacons on `port`: joined to `group` on every LAN
/// interface (and the default one), and also receiving unicast and broadcast.
/// SO_REUSEADDR/SO_REUSEPORT so several listeners on one machine coexist.
pub fn bind_beacon_listener(port: u16, group: Ipv4Addr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    let mut joined = socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED).is_ok();
    for lan in lan_addresses() {
        joined |= socket.join_multicast_v4(&group, &lan.address).is_ok();
    }
    if !joined {
        tracing::debug!("beacon listener: could not join {group}; unicast/broadcast only");
    }
    UdpSocket::from_std(socket.into())
}

/// How a [`Beacon`] sends.
#[derive(Debug, Clone)]
pub struct BeaconConfig {
    pub interval: Duration,
    pub port: u16,
    pub group: Ipv4Addr,
    /// Multicast to `group` on every real LAN interface.
    pub multicast: bool,
    /// Also send to 255.255.255.255 and each interface's directed broadcast.
    pub broadcast: bool,
    /// Extra unicast destinations (tests, or peers across a router).
    pub extra_targets: Vec<SocketAddr>,
    /// Listen on `port` for [`BEACON_QUERY`] and announce at once. Off when a
    /// unicast-only setup shares the port with a listener on the same host
    /// (SO_REUSEPORT would split unicast datagrams between the two sockets).
    pub answer_queries: bool,
}

impl Default for BeaconConfig {
    fn default() -> Self {
        Self {
            interval: BEACON_INTERVAL,
            port: BEACON_PORT,
            group: Ipv4Addr::from(BEACON_GROUP),
            multicast: true,
            broadcast: true,
            extra_targets: Vec::new(),
            answer_queries: true,
        }
    }
}

/// A running beacon. Stops when dropped.
pub struct Beacon {
    cancel: CancellationToken,
    packet: watch::Sender<BeaconPacket>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Beacon {
    /// Multicast `packet` every `interval` on every real LAN interface and to
    /// the broadcast address. Must be called inside a Tokio runtime.
    pub fn start(packet: BeaconPacket, interval: Duration) -> Self {
        Self::start_with(packet, BeaconConfig { interval, ..BeaconConfig::default() })
    }

    pub fn start_with(packet: BeaconPacket, config: BeaconConfig) -> Self {
        let cancel = CancellationToken::new();
        let (tx, rx) = watch::channel(packet);
        let task = tokio::spawn(run_beacon(config, rx, cancel.clone()));
        Self { cancel, packet: tx, task: Some(task) }
    }

    /// Change what is announced (e.g. `nodes_active`); sent immediately.
    pub fn update(&self, packet: BeaconPacket) {
        self.packet.send_replace(packet);
    }

    /// Stop and wait for the sender task to finish.
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Beacon {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn sender_socket(bind: Ipv4Addr, multicast_if: Option<Ipv4Addr>) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_broadcast(true)?;
    if let Some(iface) = multicast_if {
        socket.set_multicast_if_v4(&iface)?;
        socket.set_multicast_ttl_v4(1)?;
        socket.set_multicast_loop_v4(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(bind, 0).into())?;
    UdpSocket::from_std(socket.into())
}

fn directed_broadcast(lan: &LanAddress) -> Option<Ipv4Addr> {
    (lan.prefix <= 30).then(|| Ipv4Addr::from(u32::from(lan.address) | !(u32::MAX << (32 - lan.prefix as u32))))
}

async fn run_beacon(config: BeaconConfig, mut packet: watch::Receiver<BeaconPacket>, cancel: CancellationToken) {
    let mut per_iface: HashMap<Ipv4Addr, UdpSocket> = HashMap::new();
    let any = sender_socket(Ipv4Addr::UNSPECIFIED, None).ok();
    let mut ticker = tokio::time::interval(config.interval.max(Duration::from_millis(50)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut warned = false;
    // Hear queries so a scanning peer finds us at once, not at the next tick.
    let queries = if config.answer_queries { bind_beacon_listener(config.port, config.group).ok() } else { None };
    let mut buf = [0u8; 256];
    let mut last_sent: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {}
            changed = packet.changed() => if changed.is_err() { return },
            got = async { match &queries { Some(q) => q.recv_from(&mut buf).await, None => std::future::pending().await } } => {
                let Ok((n, _)) = got else { continue };
                if buf[..n].trim_ascii() != BEACON_QUERY.as_bytes() {
                    continue;
                }
                if last_sent.is_some_and(|t| t.elapsed() < QUERY_HOLDOFF) {
                    continue;
                }
            }
        }
        last_sent = Some(tokio::time::Instant::now());
        let bytes = match serde_json::to_vec(&*packet.borrow_and_update()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let lans: Vec<LanAddress> = if config.multicast || config.broadcast {
            lan_addresses().into_iter().filter(|a| !a.is_virtual).collect()
        } else {
            Vec::new()
        };
        per_iface.retain(|ip, _| lans.iter().any(|l| l.address == *ip));
        let mut errors = Vec::new();
        for lan in &lans {
            if let std::collections::hash_map::Entry::Vacant(slot) = per_iface.entry(lan.address) {
                match sender_socket(lan.address, Some(lan.address)) {
                    Ok(s) => {
                        slot.insert(s);
                    }
                    Err(e) => errors.push(format!("{}: {e}", lan.name)),
                }
            }
            let Some(socket) = per_iface.get(&lan.address) else { continue };
            if config.multicast {
                if let Err(e) = socket.send_to(&bytes, SocketAddrV4::new(config.group, config.port)).await {
                    errors.push(format!("multicast {}: {e}", lan.name));
                }
            }
            if config.broadcast {
                if let Some(bcast) = directed_broadcast(lan) {
                    let _ = socket.send_to(&bytes, SocketAddrV4::new(bcast, config.port)).await;
                }
            }
        }
        if let Some(any) = &any {
            if config.broadcast {
                if let Err(e) = any.send_to(&bytes, SocketAddrV4::new(Ipv4Addr::BROADCAST, config.port)).await {
                    errors.push(format!("broadcast: {e}"));
                }
            }
            for target in &config.extra_targets {
                if let Err(e) = any.send_to(&bytes, target).await {
                    errors.push(format!("{target}: {e}"));
                }
            }
        }
        if !errors.is_empty() && !warned {
            warned = true;
            tracing::debug!("beacon send errors (reported once): {}", errors.join("; "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_validates() {
        let p = beacon_packet("m1", Some("Lab"), "5.1.1", 2, 8000, 3000);
        let bytes = serde_json::to_vec(&p).unwrap();
        assert_eq!(parse_beacon(&bytes), Some(p.clone()));
        let mut bad = p.clone();
        bad.magic = "other".into();
        assert_eq!(parse_beacon(&serde_json::to_vec(&bad).unwrap()), None);
        assert_eq!(parse_beacon(b"not json"), None);
        assert_eq!(parse_beacon(&vec![b' '; MAX_BEACON_BYTES + 1]), None);
    }

    #[test]
    fn directed_broadcast_address() {
        let lan = LanAddress {
            name: "wlan0".into(),
            address: Ipv4Addr::new(192, 168, 1, 20),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            prefix: 24,
            is_virtual: false,
        };
        assert_eq!(directed_broadcast(&lan), Some(Ipv4Addr::new(192, 168, 1, 255)));
    }
}
