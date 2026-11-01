//! `po-mobile` — the phone's core, shared by the Android app (Kotlin) and, later, the iOS one
//! (Swift), through UniFFI.
//!
//! The interface only draws: signing in, the account's PCs, the link to each (kept up, made
//! again after a cut), the sessions followed (their conversation rebuilt from the events, with
//! the events missed during a cut replayed), and what the user does — all of it is here, so that
//! both apps behave the same and the tests reach it from Rust.
//!
//! Calls block (the app makes them off its main thread); what happens on its own (a session
//! changed, the link went, a turn ended) comes back through [`Listener`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use po_link::directory::Kind;
use po_link::github::{self, GitHub};
use po_link::phone::{Phone, Route};
use po_link::{Identity, Link};
use po_wire::{Conversation, Event, Feed, Reply, Request, Subscribe};

uniffi::setup_scaffolding!();

pub mod records;
pub use records::*;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{reason}")]
    Network { reason: String },
    #[error("not signed in")]
    SignedOut,
    #[error("sign-in declined")]
    Denied,
    #[error("sign-in code expired")]
    Expired,
    #[error("{reason}")]
    Unreachable { reason: String },
    #[error("{reason}")]
    Refused { reason: String },
    #[error("{reason}")]
    Pc { reason: String },
}

fn network(e: impl std::fmt::Display) -> CoreError {
    CoreError::Network { reason: e.to_string() }
}

/// Where the phone keeps its secrets: its private key, its GitHub token, what it learned of the
/// directory (whose keys open the PCs' addresses). The app implements it with the platform's
/// protection — on Android, AES-GCM under a non-exportable key of the Android Keystore; on iOS,
/// the Keychain — so that nothing secret lies in a plain file. The core stays platform-neutral.
#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    /// The secret `name`, if kept.
    fn load(&self, name: String) -> Option<Vec<u8>>;
    /// Keeps `value` as `name`; false if it could not be kept.
    fn save(&self, name: String, value: Vec<u8>) -> bool;
    fn remove(&self, name: String);
}

/// The names of the secrets.
const IDENTITY: &str = "identity";
const GITHUB: &str = "github";
const DIRECTORY: &str = "directory";

/// The directory's state, kept through the app's secret storage.
struct DirectoryState(Arc<dyn SecretStore>);

impl po_link::StateStore for DirectoryState {
    fn load(&self) -> Option<Vec<u8>> {
        self.0.load(DIRECTORY.into())
    }

    fn save(&self, bytes: &[u8]) {
        self.0.save(DIRECTORY.into(), bytes.to_vec());
    }
}

/// Moves the secrets an earlier version kept in plain files into the secret storage, then
/// deletes the files (only once kept: a failure leaves them for the next launch).
fn migrate(dir: &std::path::Path, secrets: &dyn SecretStore) {
    for (file, name) in [("identity.json", IDENTITY), ("github.json", GITHUB), ("directory.json", DIRECTORY)] {
        let path = dir.join(file);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let kept = secrets.load(name.into()).is_some() || secrets.save(name.into(), bytes);
        if kept {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// What the app hears without asking.
#[uniffi::export(with_foreign)]
pub trait Listener: Send + Sync {
    /// A followed session changed: the rows from `from` on, and the rest of its state.
    fn on_session(&self, pc: String, key: String, update: SessionUpdate);
    /// The link to a PC came up, went, or is being made again.
    fn on_link(&self, pc: String, state: LinkState);
    /// Worth a notification when the app is in the background: an approval or a question
    /// waits, a turn ended.
    fn on_alert(&self, pc: String, key: String, alert: Alert);
}

/// A session followed: its conversation, and the place in its events.
struct Followed {
    stop: tokio::sync::watch::Sender<bool>,
}

struct Inner {
    secrets: Arc<dyn SecretStore>,
    config: github::Config,
    phone: Phone,
    listener: Mutex<Option<Arc<dyn Listener>>>,
    login: Mutex<Option<String>>,
    pending: Mutex<Option<github::DeviceCode>>,
    links: Mutex<HashMap<String, Arc<Link>>>,
    /// One connection attempt at a time per PC.
    connecting: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    followed: Mutex<HashMap<(String, String), Followed>>,
    next_input: AtomicU64,
}

#[derive(uniffi::Object)]
pub struct PoCore {
    runtime: tokio::runtime::Runtime,
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Inner {
    fn listener(&self) -> Option<Arc<dyn Listener>> {
        lock(&self.listener).clone()
    }

    fn stored_token(&self) -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(&self.secrets.load(GITHUB.into())?).ok()?;
        (v["api"].as_str() == Some(self.config.api.as_str())).then(|| v["token"].as_str().map(str::to_owned)).flatten()
    }

    async fn attach(&self, token: String) -> Result<String, CoreError> {
        let login = self.phone.attach(GitHub::new(&self.config, token), Some(Arc::new(DirectoryState(self.secrets.clone())))).await.map_err(|e| match e {
            github::Error::Status(401, _) => CoreError::SignedOut,
            other => network(other),
        })?;
        *lock(&self.login) = Some(login.clone());
        Ok(login)
    }

    /// The link to `pc`, made if needed.
    async fn link(self: &Arc<Self>, pc: &str) -> Result<Arc<Link>, CoreError> {
        if let Some(link) = lock(&self.links).get(pc).filter(|l| !l.is_closed()).cloned() {
            return Ok(link);
        }
        let gate = lock(&self.connecting).entry(pc.to_owned()).or_default().clone();
        let _held = gate.lock().await;
        if let Some(link) = lock(&self.links).get(pc).filter(|l| !l.is_closed()).cloned() {
            return Ok(link);
        }
        if let Some(l) = self.listener() {
            l.on_link(pc.to_owned(), LinkState::Connecting);
        }
        let result = self.phone.connect(pc).await;
        let (link, route) = match result {
            Ok(found) => found,
            Err(e) => {
                if let Some(l) = self.listener() {
                    l.on_link(pc.to_owned(), LinkState::Down { reason: e.to_string() });
                }
                return Err(match e {
                    po_link::phone::ConnectError::Refused(m) => CoreError::Refused { reason: m },
                    other => CoreError::Unreachable { reason: other.to_string() },
                });
            }
        };
        let link = Arc::new(link);
        lock(&self.links).insert(pc.to_owned(), link.clone());
        let (address, punched) = match route {
            Route::Direct(a) => (a.to_string(), false),
            Route::Punched(a) => (a.to_string(), true),
        };
        if let Some(l) = self.listener() {
            l.on_link(pc.to_owned(), LinkState::Up { address, punched, name: link.name.clone() });
        }
        // Said when it goes, so that the app shows it and the sessions reconnect.
        let (watch, me, id) = (link.clone(), self.clone(), pc.to_owned());
        tokio::spawn(async move {
            watch.closed().await;
            let current = lock(&me.links).get(&id).is_some_and(|l| Arc::ptr_eq(l, &watch));
            if current {
                lock(&me.links).remove(&id);
                if let Some(l) = me.listener() {
                    l.on_link(id, LinkState::Down { reason: "connection lost".into() });
                }
            }
        });
        Ok(link)
    }

    async fn request(self: &Arc<Self>, pc: &str, request: Request, wait: Duration) -> Result<Reply, CoreError> {
        // One retry on a fresh link: the first may have died in a tunnel unnoticed.
        for attempt in 0..2 {
            let link = self.link(pc).await?;
            match link.request(request.clone(), wait).await {
                Ok(Reply::Error { message }) => return Err(CoreError::Pc { reason: message }),
                Ok(reply) => return Ok(reply),
                Err(po_link::LinkError::Closed) if attempt == 0 => {
                    lock(&self.links).remove(pc);
                }
                Err(e) => return Err(CoreError::Unreachable { reason: e.to_string() }),
            }
        }
        Err(CoreError::Unreachable { reason: "connection lost".into() })
    }

    /// Follows a session until told to stop: the snapshot or the missed events, then the
    /// events; after a cut, from where it stopped.
    async fn follow(self: Arc<Self>, pc: String, key: String, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut conversation = Conversation::default();
        let mut epoch: Option<String> = None;
        let mut seq = 0u64;
        let mut backoff = Duration::from_millis(500);
        while !*stop.borrow() {
            let link = match self.link(&pc).await {
                Ok(link) => link,
                Err(_) => {
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = stop.changed() => return,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                    continue;
                }
            };
            let Ok(mut feed) = link.subscribe(&Subscribe { session: key.clone(), epoch: epoch.clone(), since: seq }).await else {
                tokio::time::sleep(backoff).await;
                continue;
            };
            backoff = Duration::from_millis(500);
            let mut last_sent = std::time::Instant::now() - Duration::from_secs(1);
            let mut dirty = false;
            loop {
                // Changes are sent at most every 50 ms: a fast model sends a hundred fragments a
                // second, the screen needs twenty.
                let tick = tokio::time::sleep(Duration::from_millis(50));
                tokio::select! {
                    item = feed.recv() => match item {
                        Some(Feed::Snapshot { epoch: e, seq: s, conversation: c }) => {
                            epoch = Some(e);
                            seq = s;
                            // Applied as a reset: every row is new to the screen.
                            conversation.apply(&Event::Reset { conversation: c });
                            dirty = true;
                        }
                        Some(Feed::Event { seq: s, event }) => {
                            if s <= seq {
                                continue;
                            }
                            seq = s;
                            self.alert(&pc, &key, &event, &conversation);
                            conversation.apply(&event);
                            dirty = true;
                        }
                        Some(Feed::Gone { reason }) => {
                            if reason == "lagged" {
                                break;
                            }
                            conversation.apply(&Event::Closed);
                            self.send_update(&pc, &key, &mut conversation);
                            return;
                        }
                        None => break,
                    },
                    _ = tick, if dirty => {}
                    _ = stop.changed() => return,
                }
                if dirty && last_sent.elapsed() >= Duration::from_millis(50) {
                    self.send_update(&pc, &key, &mut conversation);
                    dirty = false;
                    last_sent = std::time::Instant::now();
                }
            }
            if dirty {
                self.send_update(&pc, &key, &mut conversation);
            }
            // A saved session is shown once: nothing follows.
            if conversation.closed {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = stop.changed() => return,
            }
        }
    }

    fn send_update(&self, pc: &str, key: &str, conversation: &mut Conversation) {
        let from = conversation.take_changes().unwrap_or(conversation.items.len());
        let subagents = conversation.take_subagents_changed();
        if let Some(l) = self.listener() {
            l.on_session(pc.to_owned(), key.to_owned(), SessionUpdate::from_conversation(conversation, from, subagents));
        }
    }

    fn alert(&self, pc: &str, key: &str, event: &Event, before: &Conversation) {
        let alert = match event {
            Event::Approval { tool, summary, .. } => Alert::Approval { tool: tool.clone(), summary: summary.clone() },
            Event::Question { question, .. } => Alert::Question { question: question.clone() },
            Event::TurnEnded { error, .. } => Alert::TurnEnded {
                title: before.status.title.clone().unwrap_or_default(),
                error: error.clone(),
                last: before
                    .items
                    .iter()
                    .rev()
                    .find_map(|i| match i {
                        po_wire::Item::Assistant { text, .. } => Some(po_wire::clip(text, 200)),
                        _ => None,
                    })
                    .unwrap_or_default(),
            },
            _ => return,
        };
        if let Some(l) = self.listener() {
            l.on_alert(pc.to_owned(), key.to_owned(), alert);
        }
    }
}

#[uniffi::export]
impl PoCore {
    /// `data_dir`: the app's private folder (older versions kept their secrets there in plain
    /// files: they move to `secrets`). `secrets`: where keys, token and directory state are kept.
    /// `github_api`/`github_web`: GitHub, or a test server in a debug build. `extra_hosts`:
    /// addresses tried for every PC on top of the directory's (`10.0.2.2` in the emulator).
    #[uniffi::constructor]
    pub fn new(
        data_dir: String,
        device_name: String,
        github_api: String,
        github_web: String,
        extra_hosts: Vec<String>,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Arc<Self>, CoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("po-core").build().map_err(network)?;
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir).map_err(network)?;
        migrate(&dir, secrets.as_ref());
        let identity = match secrets.load(IDENTITY.into()).and_then(|b| Identity::from_stored(&b)) {
            Some(identity) => identity,
            None => {
                let identity = Identity::generate();
                if !secrets.save(IDENTITY.into(), identity.to_stored()) {
                    return Err(CoreError::Network { reason: "the secret storage refused the key".into() });
                }
                identity
            }
        };
        let mut config = github::Config::default();
        if !github_api.trim().is_empty() {
            config.api = github_api.trim().trim_end_matches('/').to_owned();
        }
        if !github_web.trim().is_empty() {
            config.web = github_web.trim().trim_end_matches('/').to_owned();
        }
        let phone = {
            let _guard = runtime.enter();
            let mut phone = Phone::new(identity, &device_name, true).map_err(network)?;
            phone.extra_hosts = extra_hosts.iter().filter_map(|h| h.trim().parse().ok()).collect();
            phone
        };
        // Input numbers keep growing across launches: the PC refuses a number already seen.
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let inner = Arc::new(Inner {
            secrets,
            config,
            phone,
            listener: Mutex::new(None),
            login: Mutex::new(None),
            pending: Mutex::new(None),
            links: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashMap::new()),
            followed: Mutex::new(HashMap::new()),
            next_input: AtomicU64::new(millis * 1000),
        });
        Ok(Arc::new(Self { runtime, inner }))
    }

    pub fn set_listener(&self, listener: Arc<dyn Listener>) {
        *lock(&self.inner.listener) = Some(listener);
    }

    /// This phone's id in the directory.
    pub fn device_id(&self) -> String {
        self.inner.phone.identity.id()
    }

    /// The account signed in, if any (`resume` first, at launch).
    pub fn account(&self) -> Option<String> {
        lock(&self.inner.login).clone()
    }

    /// At launch: the account remembered, joined again.
    pub fn resume(&self) -> Result<Option<String>, CoreError> {
        let Some(token) = self.inner.stored_token() else { return Ok(None) };
        let inner = self.inner.clone();
        match self.runtime.block_on(async move { inner.attach(token).await }) {
            Ok(login) => Ok(Some(login)),
            Err(CoreError::SignedOut) => {
                self.inner.secrets.remove(GITHUB.into());
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Starts signing in: the page to open and the code to type there.
    pub fn sign_in_start(&self) -> Result<SignIn, CoreError> {
        let config = self.inner.config.clone();
        let code = self.runtime.block_on(async move { github::device_start(&github::http(), &config, github::SCOPE).await }).map_err(network)?;
        let out = SignIn { url: code.url.clone(), code: code.user_code.clone() };
        *lock(&self.inner.pending) = Some(code);
        Ok(out)
    }

    /// Waits until the code is entered: the account, joined.
    pub fn sign_in_wait(&self) -> Result<String, CoreError> {
        let code = lock(&self.inner.pending).take().ok_or(CoreError::SignedOut)?;
        let inner = self.inner.clone();
        self.runtime.block_on(async move {
            let token = github::device_wait(&github::http(), &inner.config, &code).await.map_err(|e| match e {
                github::Error::Denied => CoreError::Denied,
                github::Error::Expired => CoreError::Expired,
                other => network(other),
            })?;
            let body = serde_json::json!({"api": inner.config.api, "token": token}).to_string();
            if !inner.secrets.save(GITHUB.into(), body.into_bytes()) {
                return Err(CoreError::Network { reason: "the secret storage refused the token".into() });
            }
            inner.attach(token).await
        })
    }

    /// Signs out: this phone leaves the account's directory, its token is forgotten.
    pub fn sign_out(&self) {
        for (_, f) in lock(&self.inner.followed).drain() {
            let _ = f.stop.send(true);
        }
        for (_, link) in lock(&self.inner.links).drain() {
            link.close();
        }
        let inner = self.inner.clone();
        self.runtime.block_on(async move { inner.phone.detach(true).await });
        self.inner.secrets.remove(GITHUB.into());
        self.inner.secrets.remove(DIRECTORY.into());
        *lock(&self.inner.login) = None;
    }

    /// The account's PCs, the directory read again. `ready` is false while no PC has accepted
    /// this phone yet (the first PC to come online does it).
    pub fn pcs(&self) -> Result<PcList, CoreError> {
        let inner = self.inner.clone();
        let (devices, ready) = self.runtime.block_on(async move { inner.phone.pcs().await }).map_err(network)?;
        let links = lock(&self.inner.links);
        Ok(PcList {
            ready,
            pcs: devices
                .into_iter()
                .filter(|d| d.kind == Kind::Pc)
                .map(|d| PcInfo {
                    connected: links.get(&d.id).is_some_and(|l| !l.is_closed()),
                    reachable: d.addresses.is_some(),
                    id: d.id,
                    name: d.name,
                    seen: d.updated,
                })
                .collect(),
        })
    }

    /// Connects to `pc` (if not already); the address that answered.
    pub fn connect(&self, pc: String) -> Result<String, CoreError> {
        let inner = self.inner.clone();
        self.runtime.block_on(async move { inner.link(&pc).await.map(|l| l.remote().to_string()) })
    }

    pub fn disconnect(&self, pc: String) {
        if let Some(link) = lock(&self.inner.links).remove(&pc) {
            link.close();
        }
    }

    pub fn projects(&self, pc: String) -> Result<Vec<ProjectRec>, CoreError> {
        match self.call(&pc, Request::Projects, 20)? {
            Reply::Projects { projects } => Ok(projects.into_iter().map(ProjectRec::from).collect()),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    pub fn sessions(&self, pc: String, project: Option<String>) -> Result<Vec<SessionRec>, CoreError> {
        match self.call(&pc, Request::Sessions { project }, 20)? {
            Reply::Sessions { sessions } => Ok(sessions.into_iter().map(SessionRec::from).collect()),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Follows a session: its updates come through [`Listener::on_session`].
    pub fn open(&self, pc: String, key: String) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        if let Some(old) = lock(&self.inner.followed).insert((pc.clone(), key.clone()), Followed { stop: tx }) {
            let _ = old.stop.send(true);
        }
        self.runtime.spawn(self.inner.clone().follow(pc, key, rx));
    }

    /// Stops following a session (the session goes on on the PC).
    pub fn close_view(&self, pc: String, key: String) {
        if let Some(f) = lock(&self.inner.followed).remove(&(pc, key)) {
            let _ = f.stop.send(true);
        }
    }

    /// Acts on a session: what the PC answered.
    pub fn send(&self, pc: String, key: String, input: InputRec) -> Result<String, CoreError> {
        let id = self.inner.next_input.fetch_add(1, Ordering::Relaxed);
        match self.call(&pc, Request::Input { session: key, id, input: input.into() }, 140)? {
            Reply::Done { text } => Ok(text),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    pub fn menu(&self, pc: String, key: String, line: String) -> Result<Option<MenuRec>, CoreError> {
        match self.call(&pc, Request::Menu { session: key, line }, 60)? {
            Reply::Menu { menu } => Ok(menu.map(MenuRec::from)),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    pub fn commands(&self, pc: String, key: String) -> Result<Vec<CommandRec>, CoreError> {
        match self.call(&pc, Request::Commands { session: key }, 20)? {
            Reply::Commands { commands } => Ok(commands.into_iter().map(|c| CommandRec { name: c.name, description: c.description }).collect()),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Opens a session in `project` on `pc`, without a terminal; its key.
    pub fn new_session(&self, pc: String, project: String, prompt: Option<String>) -> Result<String, CoreError> {
        match self.call(&pc, Request::NewSession { project, prompt }, 90)? {
            Reply::Session { key } => Ok(key),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Opens a saved session again (or finds it open); its key.
    pub fn resume_session(&self, pc: String, saved: String) -> Result<String, CoreError> {
        match self.call(&pc, Request::Resume { saved }, 90)? {
            Reply::Session { key } => Ok(key),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Ends a session opened from the phone.
    pub fn close_session(&self, pc: String, key: String) -> Result<String, CoreError> {
        match self.call(&pc, Request::Close { session: key }, 20)? {
            Reply::Done { text } => Ok(text),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// The phone changed networks (Wi-Fi to mobile data, or the network went): a new socket, and
    /// every link made again at once — the address that answered may not be reachable from the
    /// new network (a PC's local address, seen from mobile data), and waiting for the old link to
    /// time out would leave the screen frozen half a minute. The sessions followed resume from
    /// the last event they had.
    pub fn network_changed(&self) {
        let _guard = self.runtime.enter();
        self.inner.phone.network_changed();
        let links: Vec<(String, Arc<Link>)> = lock(&self.inner.links).drain().collect();
        for (pc, link) in links {
            link.close();
            if let Some(l) = self.inner.listener() {
                l.on_link(pc, LinkState::Connecting);
            }
        }
    }

    /// Sends a file to the session's folder on the PC (a photo, a document); the `@path` that
    /// attaches it to a message.
    pub fn upload(&self, pc: String, key: String, name: String, data: Vec<u8>) -> Result<String, CoreError> {
        use base64::Engine;
        if data.len() > po_wire::MAX_UPLOAD {
            return Err(CoreError::Pc { reason: format!("> {} MB", po_wire::MAX_UPLOAD / (1024 * 1024)) });
        }
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        match self.call(&pc, Request::Upload { session: key, name, data }, 120)? {
            Reply::Done { text } => Ok(text),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Speech to text by the PC's own whisper: nothing goes through a third party. `wav`: 16 kHz
    /// mono 16-bit.
    pub fn transcribe(&self, pc: String, wav: Vec<u8>) -> Result<String, CoreError> {
        use base64::Engine;
        let audio = base64::engine::general_purpose::STANDARD.encode(wav);
        match self.call(&pc, Request::Transcribe { audio }, 120)? {
            Reply::Done { text } => Ok(text),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// What the PC is using now: processor, memory, cards, disks.
    pub fn usage(&self, pc: String) -> Result<UsageRec, CoreError> {
        match self.call(&pc, Request::Usage, 15)? {
            Reply::Usage { usage } => Ok(usage.into()),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }

    /// Removes a saved session from the lists (it goes to the PC's bin of sessions).
    pub fn forget(&self, pc: String, saved: String) -> Result<String, CoreError> {
        match self.call(&pc, Request::Forget { saved }, 20)? {
            Reply::Done { text } => Ok(text),
            other => Err(CoreError::Pc { reason: format!("{other:?}") }),
        }
    }
}

impl PoCore {
    fn call(&self, pc: &str, request: Request, wait: u64) -> Result<Reply, CoreError> {
        let inner = self.inner.clone();
        let pc = pc.to_owned();
        self.runtime.block_on(async move { inner.request(&pc, request, Duration::from_secs(wait)).await })
    }
}

#[cfg(test)]
mod tests;
