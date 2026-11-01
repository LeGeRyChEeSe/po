//! Finding a PC on the same network without asking anyone: the PC announces itself by mDNS
//! (`_po-link._udp.local.`, its device id in the TXT record), the phone listens.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const SERVICE: &str = "_po-link._udp.local.";

/// An announcement in progress; it stops when dropped.
pub struct Advertiser {
    daemon: mdns_sd::ServiceDaemon,
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Announces device `id` on `port` at `ips`.
pub fn advertise(id: &str, port: u16, ips: &[IpAddr]) -> Option<Advertiser> {
    if ips.is_empty() {
        return None;
    }
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let host = format!("po-{id}.local.");
    let ips: Vec<IpAddr> = ips.to_vec();
    let info = mdns_sd::ServiceInfo::new(SERVICE, &format!("po-{id}"), &host, &ips[..], port, &[("id", id)][..]).ok()?;
    daemon.register(info).ok()?;
    Some(Advertiser { daemon })
}

/// What the phone has heard: device id → addresses.
#[derive(Clone, Default)]
pub struct Heard(Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>);

impl Heard {
    pub fn get(&self, id: &str) -> Vec<SocketAddr> {
        self.0.lock().map(|m| m.get(id).cloned().unwrap_or_default()).unwrap_or_default()
    }
}

/// Listens for PCs in the background, as long as the returned daemon lives.
pub fn listen() -> Option<(mdns_sd::ServiceDaemon, Heard)> {
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let events = daemon.browse(SERVICE).ok()?;
    let heard = Heard::default();
    let shared = heard.clone();
    std::thread::Builder::new()
        .name("po-link-mdns".into())
        .spawn(move || {
            while let Ok(event) = events.recv() {
                if let mdns_sd::ServiceEvent::ServiceResolved(info) = event
                    && let Some(id) = info.get_property_val_str("id")
                {
                    let addrs: Vec<SocketAddr> = info.get_addresses().iter().map(|ip| SocketAddr::new(*ip, info.get_port())).collect();
                    if let Ok(mut map) = shared.0.lock() {
                        map.insert(id.to_owned(), addrs);
                    }
                }
            }
        })
        .ok()?;
    Some((daemon, heard))
}

/// Listens for a while and returns what was heard (for a one-off search).
pub fn search(wait: Duration) -> HashMap<String, Vec<SocketAddr>> {
    let Some((daemon, heard)) = listen() else { return HashMap::new() };
    std::thread::sleep(wait);
    let _ = daemon.shutdown();
    heard.0.lock().map(|m| m.clone()).unwrap_or_default()
}
