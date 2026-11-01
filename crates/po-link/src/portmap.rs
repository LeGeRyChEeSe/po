//! Asking the router to open Po's UDP port to the Internet: UPnP-IGD first (most home boxes),
//! then NAT-PMP (RFC 6886). A router that refuses, or speaks neither, only means the phone falls
//! back on IPv6 and on punching.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// Lease asked for; the service renews it well before.
pub const LEASE: u32 = 3600;

/// A port the router opened: the public address it gives, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub public: SocketAddr,
    pub via: &'static str,
}

/// Opens `port` toward `lan` (this machine's local address).
pub async fn open(lan: Ipv4Addr, port: u16) -> Option<Mapping> {
    if let Some(found) = upnp(lan, port).await {
        return Some(found);
    }
    nat_pmp(lan, port).await
}

async fn upnp(lan: Ipv4Addr, port: u16) -> Option<Mapping> {
    use igd_next::PortMappingProtocol::UDP;
    let options = igd_next::SearchOptions { timeout: Some(Duration::from_secs(3)), single_search_timeout: Some(Duration::from_secs(2)), ..Default::default() };
    let gateway = igd_next::aio::tokio::search_gateway(options).await.ok()?;
    let external = gateway.get_external_ip().await.ok()?;
    let local = SocketAddr::V4(SocketAddrV4::new(lan, port));
    let external_port = match gateway.add_port(UDP, port, local, LEASE, "Po remote control").await {
        Ok(()) => port,
        // The same port is taken on the router: another one, wherever it likes.
        Err(_) => gateway.add_any_port(UDP, local, LEASE, "Po remote control").await.ok()?,
    };
    Some(Mapping { public: SocketAddr::new(external, external_port), via: "UPnP" })
}

/// The routers a home network usually has: the first and last address of a /24.
fn gateways(lan: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = lan.octets();
    vec![Ipv4Addr::new(a, b, c, 1), Ipv4Addr::new(a, b, c, 254)]
}

async fn nat_pmp(lan: Ipv4Addr, port: u16) -> Option<Mapping> {
    let socket = tokio::net::UdpSocket::bind((lan, 0)).await.ok()?;
    for gateway in gateways(lan) {
        let to = SocketAddr::from((gateway, 5351));
        // The public address first (opcode 0), then the mapping (opcode 1, UDP).
        let mut buf = [0u8; 16];
        if socket.send_to(&[0, 0], to).await.is_err() {
            continue;
        }
        let Ok(Ok((n, _))) = tokio::time::timeout(Duration::from_millis(500), socket.recv_from(&mut buf)).await else { continue };
        if n < 12 || buf[1] != 128 || buf[2..4] != [0, 0] {
            continue;
        }
        let public = Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]);
        let mut request = vec![0, 1, 0, 0];
        request.extend_from_slice(&port.to_be_bytes());
        request.extend_from_slice(&port.to_be_bytes());
        request.extend_from_slice(&LEASE.to_be_bytes());
        if socket.send_to(&request, to).await.is_err() {
            continue;
        }
        let Ok(Ok((n, _))) = tokio::time::timeout(Duration::from_millis(800), socket.recv_from(&mut buf)).await else { continue };
        if n >= 16 && buf[1] == 129 && buf[2..4] == [0, 0] {
            let external = u16::from_be_bytes([buf[10], buf[11]]);
            return Some(Mapping { public: SocketAddr::new(IpAddr::V4(public), external), via: "NAT-PMP" });
        }
    }
    None
}
