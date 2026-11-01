//! The PC's side, as Po's service runs it: listen, keep the directory, open the router, answer
//! the phones' punch requests.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::directory::{Directory, Kind};
use crate::github::GitHub;
use crate::identity::Identity;
use crate::link::Backend;
use crate::net::{Addresses, Node};
use crate::tls::{Peer, Trust};

#[derive(Debug, Clone)]
pub struct Options {
    /// UDP port (0: any).
    pub port: u16,
    pub name: String,
    /// Where the directory's state is kept.
    pub state: Option<PathBuf>,
    /// How often the directory is read (a conditional request: free when nothing changed).
    pub poll: Duration,
    /// Ask STUN, the router and the local network (off in tests, which stay on localhost).
    pub discover: bool,
    /// Addresses published on top of those found (a test's localhost, an emulator's host).
    pub extra: Vec<SocketAddr>,
}

impl Default for Options {
    fn default() -> Self {
        Self { port: 0, name: "PC".into(), state: None, poll: Duration::from_secs(10), discover: true, extra: Vec::new() }
    }
}

/// What the service says about the link.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub login: String,
    pub port: u16,
    pub addresses: Addresses,
    pub phones: Vec<Peer>,
    /// Every device of the account but this one: (id, name, kind, last written).
    pub devices: Vec<(String, String, Kind, u64)>,
    pub mapping: Option<String>,
    pub last_error: Option<String>,
    pub syncs: u64,
}

pub struct Pc {
    pub node: Arc<Node>,
    pub trust: Arc<Trust>,
    pub directory: Arc<tokio::sync::Mutex<Directory>>,
    pub status: Arc<Mutex<Status>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _mdns: Option<crate::mdns::Advertiser>,
}

impl Drop for Pc {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.node.endpoint.close(0u32.into(), b"service stopped");
    }
}

/// Our addresses: local ones, plus the router's and STUN's when asked.
async fn find_addresses(node: &Node, options: &Options) -> (Addresses, Option<String>) {
    let socket = node.socket();
    let mut addresses = crate::net::local_addresses(socket.port(), socket.ipv6());
    let mut mapping = None;
    if options.discover {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        if let Some(IpAddr::V4(lan)) = addresses.lan.first().map(|a| a.ip())
            && let Some(found) = crate::portmap::open(lan, socket.port()).await
        {
            addresses.public.push(found.public);
            addresses.mapped = true;
            mapping = Some(format!("{} {}", found.via, found.public));
        }
        for public in socket.public_addresses(Duration::from_secs(2)).await {
            if public.is_ipv4() && !addresses.public.contains(&public) {
                addresses.public.push(public);
            }
        }
    }
    for extra in &options.extra {
        if !addresses.lan.contains(extra) {
            addresses.lan.push(*extra);
        }
    }
    (addresses, mapping)
}

fn phones(directory: &Directory) -> Vec<Peer> {
    directory
        .devices()
        .iter()
        .filter(|d| d.kind == Kind::Phone)
        .map(|d| Peer { id: d.id.clone(), name: d.name.clone(), ed: d.keys.ed })
        .collect()
}

fn note(status: &Mutex<Status>, directory: &Directory, error: Option<String>) {
    if let Ok(mut s) = status.lock() {
        s.login = directory.login().to_owned();
        s.addresses = directory.addresses().clone();
        s.phones = phones(directory);
        s.devices = directory.devices().iter().map(|d| (d.id.clone(), d.name.clone(), d.kind, d.updated)).collect();
        s.last_error = error;
        s.syncs += 1;
    }
}

impl Pc {
    pub async fn start(identity: Identity, gh: GitHub, options: Options, backend: Arc<dyn Backend>) -> Result<Self, String> {
        let trust = Trust::new();
        let node = Arc::new(Node::bind(options.port, Some(crate::tls::server_config(&identity, trust.clone()))).map_err(|e| e.to_string())?);
        let mut directory = Directory::open(gh, identity.clone(), Kind::Pc, &options.name, options.state.clone()).await.map_err(|e| e.to_string())?;
        let (addresses, mapping) = find_addresses(&node, &options).await;
        directory.set_addresses(addresses.clone());
        let first = directory.sync().await;
        trust.set(phones(&directory));
        let status = Arc::new(Mutex::new(Status { port: node.port(), mapping, ..Status::default() }));
        note(&status, &directory, first.err().map(|e| e.to_string()));
        let mdns = if options.discover {
            let ips: Vec<IpAddr> = addresses.lan.iter().chain(&addresses.v6).map(|a| a.ip()).collect();
            crate::mdns::advertise(&identity.id(), node.port(), &ips)
        } else {
            None
        };
        let directory = Arc::new(tokio::sync::Mutex::new(directory));
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(crate::link::serve(node.endpoint.clone(), identity.id(), trust.clone(), backend)));
        // The directory: read every few seconds, punches answered, addresses kept current.
        {
            let (node, trust, directory, status, options) = (node.clone(), trust.clone(), directory.clone(), status.clone(), options.clone());
            tasks.push(tokio::spawn(async move {
                let mut rounds: u64 = 0;
                let mut failures = 0u32;
                loop {
                    // GitHub down or the network gone: wait longer each time, up to five minutes.
                    let pause = options.poll * 2u32.saturating_pow(failures.min(5));
                    tokio::time::sleep(pause.min(Duration::from_secs(300))).await;
                    rounds += 1;
                    // Local addresses each minute; the router and STUN each half hour.
                    let every = (60 / options.poll.as_secs().max(1)).max(1);
                    if rounds.is_multiple_of(every) {
                        let quick = Options { discover: options.discover && rounds.is_multiple_of(every * 30), ..options.clone() };
                        let (mut fresh, mapping) = find_addresses(&node, &quick).await;
                        let mut dir = directory.lock().await;
                        if !quick.discover {
                            // Keep what only the slow search finds.
                            let old = dir.addresses().clone();
                            for p in old.public {
                                if !fresh.public.contains(&p) {
                                    fresh.public.push(p);
                                }
                            }
                            fresh.mapped |= old.mapped;
                        } else if let Ok(mut s) = status.lock() {
                            s.mapping = mapping;
                        }
                        dir.set_addresses(fresh);
                    }
                    let mut dir = directory.lock().await;
                    let result = dir.sync().await;
                    failures = if result.is_ok() { 0 } else { failures + 1 };
                    trust.set(phones(&dir));
                    let punches = dir.take_punches();
                    note(&status, &dir, result.err().map(|e| e.to_string()));
                    drop(dir);
                    // Packets toward the phone for a few seconds, while it tries toward us.
                    if !punches.is_empty() {
                        let socket = node.socket();
                        tokio::spawn(async move {
                            for _ in 0..25 {
                                for (_, targets) in &punches {
                                    socket.punch(targets);
                                }
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        });
                    }
                }
            }));
        }
        Ok(Self { node, trust, directory, status, tasks, _mdns: mdns })
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub async fn revoke(&self, id: &str) -> Result<(), String> {
        let mut dir = self.directory.lock().await;
        dir.revoke(id).await.map_err(|e| e.to_string())?;
        self.trust.set(phones(&dir));
        note(&self.status, &dir, None);
        Ok(())
    }

    pub async fn rename(&self, name: &str) -> Result<(), String> {
        let mut dir = self.directory.lock().await;
        dir.set_name(name);
        dir.sync().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Reads the directory now (the user just signed a phone in and waits).
    pub async fn sync_now(&self) -> Result<(), String> {
        let mut dir = self.directory.lock().await;
        let result = dir.sync().await;
        self.trust.set(phones(&dir));
        note(&self.status, &dir, result.as_ref().err().map(|e| e.to_string()));
        result.map(|_| ()).map_err(|e| e.to_string())
    }
}
