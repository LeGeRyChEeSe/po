//! The phone's side: the account's PCs from the directory, and a connection to one of them by
//! the first of its addresses that answers - punching through the firewalls when none does.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::directory::{Device, Directory, Kind};
use crate::github::GitHub;
use crate::identity::Identity;
use crate::link::{Link, LinkError};
use crate::net::{Node, RaceError};

/// How a connection was made, for the phone to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct(SocketAddr),
    /// After the PC punched toward us.
    Punched(SocketAddr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The PC is not in the directory (or not yet readable: it has not sealed the key for us).
    Unknown,
    /// We do not hold the directory key yet: a PC of the account must come online to give it.
    NoKey,
    Unreachable(String),
    Refused(String),
    Link(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unknown => write!(f, "this PC is not in the account's directory"),
            ConnectError::NoKey => write!(f, "waiting for a PC of the account to accept this phone"),
            ConnectError::Unreachable(e) => write!(f, "PC unreachable: {e}"),
            ConnectError::Refused(e) => write!(f, "connection refused: {e}"),
            ConnectError::Link(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

pub struct Phone {
    pub identity: Identity,
    pub node: Node,
    pub name: String,
    directory: tokio::sync::Mutex<Option<Directory>>,
    /// The address that worked last, by PC: tried first next time.
    last: Mutex<HashMap<String, SocketAddr>>,
    heard: Option<crate::mdns::Heard>,
    _mdns: Option<mdns_sd::ServiceDaemon>,
    /// Addresses tried for every PC on top of the directory's (the emulator's host in a debug
    /// build: `10.0.2.2`, with the PC's port).
    pub extra_hosts: Vec<std::net::IpAddr>,
    /// Ask STUN for our public address before a punch (off in tests).
    pub discover: bool,
}

impl Phone {
    pub fn new(identity: Identity, name: &str, discover: bool) -> std::io::Result<Self> {
        let node = Node::bind(0, None)?;
        let (mdns, heard) = if discover { crate::mdns::listen().map(|(d, h)| (Some(d), Some(h))).unwrap_or((None, None)) } else { (None, None) };
        Ok(Self {
            identity,
            node,
            name: name.to_owned(),
            directory: tokio::sync::Mutex::new(None),
            last: Mutex::new(HashMap::new()),
            heard,
            _mdns: mdns,
            extra_hosts: Vec::new(),
            discover,
        })
    }

    /// Joins the account's directory (after signing in).
    pub async fn attach(&self, gh: GitHub, state: Option<std::sync::Arc<dyn crate::StateStore>>) -> Result<String, crate::github::Error> {
        let mut directory = Directory::open_in(gh, self.identity.clone(), Kind::Phone, &self.name, state).await?;
        // A phone is not online all the time: its entry is written when it has news, not to say
        // it is alive.
        directory.heartbeat = u64::MAX;
        directory.set_addresses(crate::net::local_addresses(self.node.port(), self.node.socket().ipv6()));
        directory.sync().await?;
        let login = directory.login().to_owned();
        *self.directory.lock().await = Some(directory);
        Ok(login)
    }

    pub async fn detach(&self, leave: bool) {
        if let Some(mut directory) = self.directory.lock().await.take()
            && leave
        {
            let _ = directory.leave().await;
        }
    }

    pub async fn attached(&self) -> bool {
        self.directory.lock().await.is_some()
    }

    /// The account's PCs, the directory read again.
    pub async fn pcs(&self) -> Result<(Vec<Device>, bool), crate::github::Error> {
        let mut guard = self.directory.lock().await;
        let Some(directory) = guard.as_mut() else { return Ok((Vec::new(), false)) };
        directory.sync().await?;
        Ok((directory.devices().iter().filter(|d| d.kind == Kind::Pc).cloned().collect(), directory.has_key()))
    }

    /// After a network change: a new socket, onto which QUIC moves the open connections.
    pub fn network_changed(&self) {
        let _ = self.node.rebind();
    }

    fn candidates(&self, pc: &Device) -> Vec<SocketAddr> {
        let mut out = Vec::new();
        let mut add = |a: SocketAddr| {
            if !out.contains(&a) {
                out.push(a);
            }
        };
        if let Some(last) = self.last.lock().ok().and_then(|m| m.get(&pc.id).copied()) {
            add(last);
        }
        if let Some(heard) = &self.heard {
            heard.get(&pc.id).into_iter().for_each(&mut add);
        }
        if let Some(addresses) = &pc.addresses {
            addresses.candidates().into_iter().for_each(&mut add);
            for host in &self.extra_hosts {
                add(SocketAddr::new(*host, addresses.port));
            }
        }
        out
    }

    /// Connects to PC `id`: every address in parallel; if none answers, a punch request through
    /// the directory, and again while the PC punches toward us.
    pub async fn connect(&self, id: &str) -> Result<(Link, Route), ConnectError> {
        let pc = {
            let mut guard = self.directory.lock().await;
            let directory = guard.as_mut().ok_or(ConnectError::Unknown)?;
            if directory.device(id).is_none_or(|d| d.addresses.is_none()) {
                let _ = directory.sync().await;
            }
            if !directory.has_key() {
                return Err(ConnectError::NoKey);
            }
            directory.device(id).filter(|d| d.kind == Kind::Pc).cloned().ok_or(ConnectError::Unknown)?
        };
        let config = crate::tls::client_config(&self.identity, pc.keys.ed);
        let candidates = self.candidates(&pc);
        let first = crate::net::race(&self.node.endpoint, config.clone(), &candidates, Duration::from_millis(2500)).await;
        let (connection, route) = match first {
            Ok(c) => {
                let at = crate::net::canonical(c.remote_address());
                (c, Route::Direct(at))
            }
            Err(RaceError::Refused(why)) => return Err(ConnectError::Refused(why)),
            Err(_) => {
                // Our addresses as of now (the network may have changed), and the request.
                let mut addresses = crate::net::local_addresses(self.node.port(), self.node.socket().ipv6());
                if self.discover {
                    addresses.public = self.node.socket().public_addresses(Duration::from_secs(2)).await;
                }
                {
                    let mut guard = self.directory.lock().await;
                    let directory = guard.as_mut().ok_or(ConnectError::Unknown)?;
                    directory.set_addresses(addresses);
                    directory.request_punch(id);
                    directory.sync().await.map_err(|e| ConnectError::Unreachable(e.to_string()))?;
                }
                // The PC reads the directory every ten seconds: we keep trying a little longer.
                let deadline = std::time::Instant::now() + Duration::from_secs(25);
                let mut found = None;
                while std::time::Instant::now() < deadline {
                    // Our own packets toward the PC open our side meanwhile.
                    self.node.socket().punch(&candidates);
                    match crate::net::race(&self.node.endpoint, config.clone(), &candidates, Duration::from_millis(1500)).await {
                        Ok(c) => {
                            found = Some(c);
                            break;
                        }
                        Err(RaceError::Refused(why)) => return Err(ConnectError::Refused(why)),
                        Err(_) => {}
                    }
                }
                let connection = found.ok_or_else(|| ConnectError::Unreachable(RaceError::Timeout.to_string()))?;
                let at = crate::net::canonical(connection.remote_address());
                (connection, Route::Punched(at))
            }
        };
        let at = match &route {
            Route::Direct(a) | Route::Punched(a) => *a,
        };
        if let Ok(mut last) = self.last.lock() {
            last.insert(id.to_owned(), at);
        }
        let link = Link::open(connection, &self.name, concat!("po-mobile/", env!("CARGO_PKG_VERSION"))).await.map_err(|e: LinkError| ConnectError::Link(e.to_string()))?;
        Ok((link, route))
    }
}

/// A phone shared between tasks.
pub type SharedPhone = Arc<Phone>;
