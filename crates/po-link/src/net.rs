//! The UDP side of a link: one socket per device, shared by QUIC, the address discovery (STUN)
//! and the hole punching, so that what opens a firewall opens it for the very port QUIC uses.
//!
//! quinn owns the socket's reading. [`LinkSocket`] stands between them: the few datagrams that
//! are not QUIC (a STUN answer, a punch) are taken out of each batch before quinn sees it.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::identity::random;

const STUN_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
/// A punch: nothing to read, it only opens the way. It starts with a zero byte, which no QUIC
/// packet does (the fixed bit is set, its greasing is turned off).
const PUNCH_MAGIC: &[u8; 8] = b"\0POLINK\0";

/// Public STUN servers, asked only for the address a NAT gives us: no data goes through them.
pub const STUN_SERVERS: &[&str] = &["stun.l.google.com:19302", "stun.cloudflare.com:3478", "stun1.l.google.com:19302"];

/// The socket quinn reads through.
#[derive(Debug)]
pub struct LinkSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    raw: std::net::UdpSocket,
    ipv6: bool,
    stun: Mutex<HashMap<[u8; 12], oneshot::Sender<SocketAddr>>>,
    punched_by: Mutex<Vec<(SocketAddr, Instant)>>,
}

fn is_stun(data: &[u8]) -> bool {
    data.len() >= 20 && data[0] & 0xc0 == 0 && data[4..8] == STUN_COOKIE
}

fn is_punch(data: &[u8]) -> bool {
    data.len() >= PUNCH_MAGIC.len() && data.starts_with(PUNCH_MAGIC)
}

/// The address a STUN answer reports (XOR-MAPPED-ADDRESS, else MAPPED-ADDRESS).
pub fn parse_stun_answer(data: &[u8]) -> Option<([u8; 12], SocketAddr)> {
    if !is_stun(data) || data[0..2] != [0x01, 0x01] {
        return None;
    }
    let txn: [u8; 12] = data[8..20].try_into().ok()?;
    let len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let body = data.get(20..20 + len)?;
    let mut i = 0;
    let mut plain = None;
    while i + 4 <= body.len() {
        let kind = u16::from_be_bytes([body[i], body[i + 1]]);
        let size = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let value = body.get(i + 4..i + 4 + size)?;
        let xored = kind == 0x0020;
        if (xored || kind == 0x0001) && value.len() >= 8 {
            let mut port = u16::from_be_bytes([value[2], value[3]]);
            if xored {
                port ^= 0x2112;
            }
            let addr = match value[1] {
                0x01 => {
                    let mut ip: [u8; 4] = value[4..8].try_into().ok()?;
                    if xored {
                        ip.iter_mut().zip(STUN_COOKIE).for_each(|(b, k)| *b ^= k);
                    }
                    IpAddr::V4(Ipv4Addr::from(ip))
                }
                0x02 if value.len() >= 20 => {
                    let mut ip: [u8; 16] = value[4..20].try_into().ok()?;
                    if xored {
                        let key: Vec<u8> = STUN_COOKIE.iter().chain(txn.iter()).copied().collect();
                        ip.iter_mut().zip(key).for_each(|(b, k)| *b ^= k);
                    }
                    IpAddr::V6(Ipv6Addr::from(ip))
                }
                _ => return None,
            };
            let found = SocketAddr::new(addr, port);
            if xored {
                return Some((txn, found));
            }
            plain = Some(found);
        }
        i += 4 + size.div_ceil(4) * 4;
    }
    plain.map(|a| (txn, a))
}

/// A STUN binding request.
pub fn stun_request(txn: &[u8; 12]) -> Vec<u8> {
    let mut out = vec![0x00, 0x01, 0x00, 0x00];
    out.extend_from_slice(&STUN_COOKIE);
    out.extend_from_slice(txn);
    out
}

/// The address as the socket must be given it: an IPv6 socket speaks to IPv4 through mapped
/// addresses.
fn for_socket(addr: SocketAddr, ipv6: bool) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) if ipv6 => SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0)),
        other => other,
    }
}

/// An address as people read it: `::ffff:1.2.3.4` is `1.2.3.4`.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Tests (this crate's, and those of the crates enabling `loopback`) stay on the loopback: a
/// socket open on the network makes Windows ask the user whether the program may use it, and a
/// test binary is a new program at every build (its name carries a hash) - the question came
/// back after nearly every `cargo test`.
pub const LOOPBACK_ONLY: bool = cfg!(any(test, feature = "loopback"));

impl LinkSocket {
    /// Binds `port` (0: any) on every address, IPv6 and IPv4 together when the system allows.
    pub fn bind(port: u16) -> io::Result<Arc<Self>> {
        let socket = if LOOPBACK_ONLY {
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port))?
        } else {
            match Self::dual(port) {
                Ok(s) => s,
                Err(_) => std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?,
            }
        };
        socket.set_nonblocking(true)?;
        let ipv6 = socket.local_addr()?.is_ipv6();
        let raw = socket.try_clone()?;
        let inner = quinn::default_runtime().ok_or_else(|| io::Error::other("no async runtime"))?.wrap_udp_socket(socket)?;
        Ok(Arc::new(Self { inner, raw, ipv6, stun: Mutex::new(HashMap::new()), punched_by: Mutex::new(Vec::new()) }))
    }

    fn dual(port: u16) -> io::Result<std::net::UdpSocket> {
        let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_only_v6(false)?;
        socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
        Ok(socket.into())
    }

    pub fn port(&self) -> u16 {
        self.inner.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn ipv6(&self) -> bool {
        self.ipv6
    }

    fn send_raw(&self, data: &[u8], to: SocketAddr) -> io::Result<usize> {
        if to.is_ipv6() && !self.ipv6 {
            return Err(io::Error::other("IPv6 unavailable"));
        }
        self.raw.send_to(data, for_socket(to, self.ipv6))
    }

    /// The address a NAT gives this socket, as `server` sees it.
    pub async fn stun(&self, server: SocketAddr, wait: Duration) -> Option<SocketAddr> {
        let txn = random::<12>();
        let (tx, rx) = oneshot::channel();
        self.stun.lock().ok()?.insert(txn, tx);
        let request = stun_request(&txn);
        let mut rx = rx;
        let mut result = None;
        // UDP may lose the first try: three, spaced.
        for _ in 0..3 {
            let _ = self.send_raw(&request, server);
            match tokio::time::timeout(wait / 3, &mut rx).await {
                Ok(Ok(addr)) => {
                    result = Some(canonical(addr));
                    break;
                }
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        if let Ok(mut map) = self.stun.lock() {
            map.remove(&txn);
        }
        result
    }

    /// The public addresses of this socket, as the STUN servers see them.
    pub async fn public_addresses(&self, wait: Duration) -> Vec<SocketAddr> {
        let mut servers = Vec::new();
        for name in STUN_SERVERS {
            if let Ok(Ok(found)) = tokio::time::timeout(wait, tokio::net::lookup_host(*name)).await {
                servers.extend(found.filter(|a| a.is_ipv4() || self.ipv6).take(2));
            }
        }
        let asks = servers.into_iter().map(|s| self.stun(s, wait));
        let mut out: Vec<SocketAddr> = futures_util::future::join_all(asks).await.into_iter().flatten().collect();
        out.sort();
        out.dedup();
        out
    }

    /// Sends a few datagrams to `targets`: our firewall and NAT then let their answers in.
    pub fn punch(&self, targets: &[SocketAddr]) {
        let mut packet = PUNCH_MAGIC.to_vec();
        packet.extend_from_slice(&random::<8>());
        for target in targets {
            let _ = self.send_raw(&packet, *target);
        }
    }

    /// Who punched toward us lately (for the diagnostics and the tests).
    pub fn punched_by(&self) -> Vec<SocketAddr> {
        self.punched_by.lock().map(|v| v.iter().map(|(a, _)| *a).collect()).unwrap_or_default()
    }

    fn take(&self, from: SocketAddr, data: &[u8]) -> bool {
        if is_punch(data) {
            if let Ok(mut seen) = self.punched_by.lock() {
                seen.retain(|(_, at)| at.elapsed() < Duration::from_secs(300));
                seen.push((canonical(from), Instant::now()));
                let excess = seen.len().saturating_sub(64);
                seen.drain(..excess);
            }
            return true;
        }
        if is_stun(data) {
            if let Some((txn, addr)) = parse_stun_answer(data)
                && let Some(tx) = self.stun.lock().ok().and_then(|mut m| m.remove(&txn))
            {
                let _ = tx.send(addr);
            }
            return true;
        }
        false
    }
}

impl AsyncUdpSocket for LinkSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.inner.try_send(transmit)
    }

    fn poll_recv(&self, cx: &mut Context, bufs: &mut [io::IoSliceMut<'_>], meta: &mut [RecvMeta]) -> Poll<io::Result<usize>> {
        let n = match self.inner.poll_recv(cx, bufs, meta) {
            Poll::Ready(Ok(n)) => n,
            other => return other,
        };
        for i in 0..n {
            let len = meta[i].len.min(meta[i].stride.max(1)).min(bufs[i].len());
            if self.take(meta[i].addr, &bufs[i][..len]) {
                // An empty datagram is skipped by quinn.
                meta[i].len = 0;
            }
        }
        Poll::Ready(Ok(n))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// A QUIC endpoint and the socket under it, which a phone replaces when it changes networks.
pub struct Node {
    pub endpoint: quinn::Endpoint,
    socket: Mutex<Arc<LinkSocket>>,
}

fn endpoint_config() -> quinn::EndpointConfig {
    let mut config = quinn::EndpointConfig::default();
    // Every QUIC packet then has its fixed bit, which tells it from a STUN answer or a punch.
    config.grease_quic_bit(false);
    config
}

impl Node {
    /// An endpoint on `port` (0: any), serving if `server` is given.
    pub fn bind(port: u16, server: Option<quinn::ServerConfig>) -> io::Result<Self> {
        let socket = LinkSocket::bind(port)?;
        let runtime = quinn::default_runtime().ok_or_else(|| io::Error::other("no async runtime"))?;
        let endpoint = quinn::Endpoint::new_with_abstract_socket(endpoint_config(), server, socket.clone(), runtime)?;
        Ok(Self { endpoint, socket: Mutex::new(socket) })
    }

    pub fn socket(&self) -> Arc<LinkSocket> {
        self.socket.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn port(&self) -> u16 {
        self.socket().port()
    }

    /// A new socket under the same connections: after a network change, the phone's packets
    /// leave from the new network and QUIC migrates the connections to it.
    pub fn rebind(&self) -> io::Result<()> {
        let socket = LinkSocket::bind(0)?;
        self.endpoint.rebind_abstract(socket.clone())?;
        *self.socket.lock().unwrap_or_else(|e| e.into_inner()) = socket;
        Ok(())
    }
}

/// The addresses a device publishes (encrypted) for the others to reach it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Addresses {
    pub port: u16,
    /// On the local network.
    pub lan: Vec<SocketAddr>,
    /// Global IPv6: reachable as they are, without a NAT.
    pub v6: Vec<SocketAddr>,
    /// Public IPv4, as a NAT gives it (STUN, or a port the router opened).
    pub public: Vec<SocketAddr>,
    /// The router was asked to open the port (UPnP or NAT-PMP) and agreed.
    pub mapped: bool,
}

impl Addresses {
    /// Every address, in the order worth trying: the local network, IPv6, then IPv4.
    pub fn candidates(&self) -> Vec<SocketAddr> {
        let mut out = Vec::new();
        for addr in self.lan.iter().chain(&self.v6).chain(&self.public) {
            if !out.contains(addr) {
                out.push(*addr);
            }
        }
        out
    }
}

/// Whether an IPv6 address can be reached from outside (not loopback, link-local, unique-local
/// or documentation).
pub fn is_global_v6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    // 2000::/3 is the global unicast space; 2001:db8::/32 is for examples only.
    (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

/// The address the system would send from toward the Internet, without sending anything.
pub fn route_address(v6: bool) -> Option<IpAddr> {
    let (bind, probe): (SocketAddr, SocketAddr) = if v6 {
        ((Ipv6Addr::UNSPECIFIED, 0).into(), "[2001:4860:4860::8888]:443".parse().ok()?)
    } else {
        ((Ipv4Addr::UNSPECIFIED, 0).into(), "8.8.8.8:443".parse().ok()?)
    };
    let socket = std::net::UdpSocket::bind(bind).ok()?;
    socket.connect(probe).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// The addresses of this machine for `port`, without asking anyone (STUN and the router come
/// on top).
pub fn local_addresses(port: u16, ipv6: bool) -> Addresses {
    let mut out = Addresses { port, ..Addresses::default() };
    if LOOPBACK_ONLY {
        out.lan.push(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        return out;
    }
    if let Some(ip) = route_address(false) {
        let addr = SocketAddr::new(ip, port);
        // A private address is for the local network; a public one (no NAT) is reachable as is.
        match ip {
            IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]) => out.lan.push(addr),
            IpAddr::V4(_) => out.public.push(addr),
            IpAddr::V6(_) => {}
        }
    }
    if ipv6
        && let Some(IpAddr::V6(ip)) = route_address(true)
        && is_global_v6(&ip)
    {
        out.v6.push(SocketAddr::new(IpAddr::V6(ip), port));
    }
    out
}

/// Why a connection could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaceError {
    /// No address to try.
    NoCandidate,
    /// None answered in time.
    Timeout,
    /// They answered, but refused us or were not the expected PC.
    Refused(String),
}

impl std::fmt::Display for RaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RaceError::NoCandidate => write!(f, "no address to try"),
            RaceError::Timeout => write!(f, "no answer"),
            RaceError::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

/// Delay between the starts of two attempts: the best address (first) gets a head start, the
/// others still go before the first one has had time to time out.
const STAGGER: Duration = Duration::from_millis(60);

/// Tries every candidate in parallel (staggered), keeps the first connection made.
pub async fn race(endpoint: &quinn::Endpoint, config: quinn::ClientConfig, candidates: &[SocketAddr], deadline: Duration) -> Result<quinn::Connection, RaceError> {
    use futures_util::StreamExt;
    if candidates.is_empty() {
        return Err(RaceError::NoCandidate);
    }
    let mut attempts = futures_util::stream::FuturesUnordered::new();
    for (i, addr) in candidates.iter().enumerate() {
        let (endpoint, config, addr) = (endpoint.clone(), config.clone(), *addr);
        attempts.push(async move {
            tokio::time::sleep(STAGGER * i as u32).await;
            match endpoint.connect_with(config, addr, crate::tls::SERVER_NAME) {
                Ok(connecting) => connecting.await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        });
    }
    let mut refused = None;
    let outcome = tokio::time::timeout(deadline, async {
        while let Some(result) = attempts.next().await {
            match result {
                Ok(connection) => return Some(connection),
                Err(why) if !why.contains("timed out") && !why.contains("invalid remote address") => refused = Some(why),
                Err(_) => {}
            }
        }
        None
    })
    .await;
    match outcome {
        Ok(Some(connection)) => Ok(connection),
        _ => Err(refused.map(RaceError::Refused).unwrap_or(RaceError::Timeout)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stun_answer_gives_the_mapped_address() {
        let txn = [7u8; 12];
        // XOR-MAPPED-ADDRESS for 203.0.113.9:40000.
        let port = 40000u16 ^ 0x2112;
        let ip = [203u8 ^ 0x21, 0x12, 113 ^ 0xa4, 9 ^ 0x42];
        let mut answer = vec![0x01, 0x01, 0x00, 12];
        answer.extend_from_slice(&STUN_COOKIE);
        answer.extend_from_slice(&txn);
        answer.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        answer.extend_from_slice(&port.to_be_bytes());
        answer.extend_from_slice(&ip);
        assert_eq!(parse_stun_answer(&answer), Some((txn, "203.0.113.9:40000".parse().unwrap())));
        // A request, a truncated answer, a QUIC packet: nothing.
        assert_eq!(parse_stun_answer(&stun_request(&txn)), None);
        assert_eq!(parse_stun_answer(&answer[..25]), None);
        assert!(!is_stun(&[0xc0; 40]));
        assert!(is_punch(b"\0POLINK\0abcdefgh"));
    }

    #[test]
    fn candidates_come_in_the_order_worth_trying() {
        let a = Addresses {
            port: 1,
            lan: vec!["192.168.1.2:1".parse().unwrap()],
            v6: vec!["[2a01:e0a::1]:1".parse().unwrap()],
            public: vec!["203.0.113.9:1".parse().unwrap(), "192.168.1.2:1".parse().unwrap()],
            mapped: false,
        };
        assert_eq!(a.candidates().len(), 3);
        assert!(a.candidates()[0].ip().is_ipv4());
        assert!(a.candidates()[1].is_ipv6());
    }

    #[test]
    fn only_global_ipv6_is_published() {
        assert!(is_global_v6(&"2a01:e0a:1::5".parse().unwrap()));
        assert!(!is_global_v6(&"fe80::1".parse().unwrap()));
        assert!(!is_global_v6(&"::1".parse().unwrap()));
        assert!(!is_global_v6(&"fd00::1".parse().unwrap()));
        assert!(!is_global_v6(&"2001:db8::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn a_punch_reaches_the_other_socket_and_does_not_disturb_quic() {
        let a = Node::bind(0, None).unwrap();
        let b = Node::bind(0, None).unwrap();
        let target: SocketAddr = (Ipv4Addr::LOCALHOST, b.port()).into();
        // quinn must be reading for the socket to see anything: an endpoint waits for a connection.
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move { endpoint.accept().await });
        for _ in 0..20 {
            a.socket().punch(&[target]);
            tokio::time::sleep(Duration::from_millis(20)).await;
            if !b.socket().punched_by().is_empty() {
                break;
            }
        }
        let seen = b.socket().punched_by();
        assert_eq!(seen.first().map(|s| s.port()), Some(a.port()));
        assert_eq!(seen[0].ip(), IpAddr::V4(Ipv4Addr::LOCALHOST), "the address is shown as IPv4, not mapped");
    }

    #[tokio::test]
    async fn stun_through_the_shared_socket() {
        // A STUN server of our own, on localhost: it answers with the address it saw.
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            while let Ok((n, from)) = server.recv_from(&mut buf).await {
                if n < 20 {
                    continue;
                }
                let txn: [u8; 12] = buf[8..20].try_into().unwrap();
                let SocketAddr::V4(v4) = from else { continue };
                let port = v4.port() ^ 0x2112;
                let mut ip = v4.ip().octets();
                ip.iter_mut().zip(STUN_COOKIE).for_each(|(b, k)| *b ^= k);
                let mut answer = vec![0x01, 0x01, 0x00, 12];
                answer.extend_from_slice(&STUN_COOKIE);
                answer.extend_from_slice(&txn);
                answer.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
                answer.extend_from_slice(&port.to_be_bytes());
                answer.extend_from_slice(&ip);
                let _ = server.send_to(&answer, from).await;
            }
        });
        let node = Node::bind(0, None).unwrap();
        let endpoint = node.endpoint.clone();
        tokio::spawn(async move { endpoint.accept().await });
        let found = node.socket().stun(server_addr, Duration::from_secs(3)).await;
        assert_eq!(found, Some(SocketAddr::from((Ipv4Addr::LOCALHOST, node.port()))));
    }
}
