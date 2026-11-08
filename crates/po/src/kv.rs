//! The disk parking of the session server's cache (see `po_local::kvstore`), plugged into the
//! agent, and the session switch of `/resume`.
//!
//! Finding where the server saves its files means reading its command line, which takes about a
//! second on Windows: it runs in the background at startup, and the first park or unpark waits
//! for it at most a few seconds.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use po_local::kvstore::{self, KvStore};

/// The parking of the session's server, once detected (`None`: the server cannot save).
static STORE: OnceLock<Option<KvStore>> = OnceLock::new();
/// Session to switch to when Po restarts (`/resume`).
static RESUME_TARGET: Mutex<Option<String>> = Mutex::new(None);

/// Po's registers of parked conversations.
pub fn register_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("kv"))
}

fn pref_number(key: &str) -> u64 {
    po_i18n::prefs::get_str(key).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Starts looking for where the server at `base_url` (a single-slot local server) saves its
/// slots, and plugs the parking into the agent.
pub fn setup(agent: &mut po_agent::Agent, endpoint: &str, base_url: &str) {
    let local = base_url.contains("127.0.0.1") || base_url.contains("localhost");
    let (Some(port), Some(registers)) = (port_of(base_url), register_dir()) else { return };
    if !local {
        return;
    }
    let base = base_url.to_owned();
    std::thread::spawn(move || {
        let store = kvstore::detect_save_dir(port).map(|dir| {
            KvStore::new(&base, dir, registers.join(format!("register-{port}.json")))
                .with_limits(pref_number("kv_quota_gb"), pref_number("kv_max_days"))
        });
        if let Some(store) = &store {
            store.sweep();
        }
        let _ = STORE.set(store);
    });
    agent.set_parking(endpoint, Arc::new(DiskParking));
}

/// Plugs the parking already set up into another agent (the Po Labs session).
pub fn attach(agent: &mut po_agent::Agent, endpoint: &str) {
    agent.set_parking(endpoint, Arc::new(DiskParking));
}

fn port_of(base_url: &str) -> Option<u16> {
    let rest = base_url.split("://").nth(1)?;
    let host = rest.split('/').next()?;
    host.rsplit(':').next()?.parse().ok()
}

/// The parking once detected, waiting for the detection a few seconds at most.
async fn store() -> Option<&'static KvStore> {
    for _ in 0..60 {
        if let Some(found) = STORE.get() {
            return found.as_ref();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// The parking, if already known (for display).
pub fn current() -> Option<&'static KvStore> {
    STORE.get().and_then(Option::as_ref)
}

struct DiskParking;

#[async_trait]
impl po_agent::Parking for DiskParking {
    async fn park(&self, key: &str) -> bool {
        match store().await {
            Some(store) => store.park(key).await.is_ok(),
            None => false,
        }
    }

    async fn unpark(&self, key: &str) -> bool {
        match store().await {
            Some(store) => store.unpark(key).await.is_some(),
            None => false,
        }
    }

    fn forget(&self, key: &str) {
        match current() {
            Some(store) => store.forget(key),
            None => {
                if let Some(dir) = register_dir() {
                    kvstore::forget_everywhere(&dir, key);
                }
            }
        }
    }
}

/// Parks the conversation as Po leaves it, so that resuming it later reads nothing again.
pub async fn park_on_exit(agent: &po_agent::Agent) {
    if agent.session.estimated_prompt_tokens() < kvstore::MIN_TOKENS {
        return;
    }
    if let (Some(parking), Some(key)) = (agent.parking(), &agent.session_key) {
        parking.park(key).await;
    }
}

/// Loads a resumed conversation back into the server, if it was parked.
pub async fn unpark_on_resume(agent: &po_agent::Agent) -> bool {
    match (agent.parking(), &agent.session_key) {
        (Some(parking), Some(key)) => parking.unpark(key).await,
        _ => false,
    }
}

pub fn take_resume_target() -> Option<String> {
    RESUME_TARGET.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_port_is_read_from_the_base_url() {
        assert_eq!(super::port_of("http://127.0.0.1:8080/v1"), Some(8080));
        assert_eq!(super::port_of("http://localhost:18080"), Some(18080));
        assert_eq!(super::port_of("https://api.example.com/v1"), None);
    }
}
