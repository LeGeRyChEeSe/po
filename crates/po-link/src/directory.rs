//! The directory: one secret gist `po-link` on the user's GitHub account, where each device
//! says who it is and where to reach it.
//!
//! **The account is the trust anchor.** Only someone holding a token of the account can write
//! the gist; a PC accepts the phones listed there, a phone the PCs listed there. Signing in with
//! the same account on both is the pairing.
//!
//! **Nothing useful leaks with the gist's address.** A secret gist is readable by anyone who
//! has its link, so what matters in it is encrypted. Each device writes its own file
//! (`dev-<id>.json`): its public keys, its name, and its addresses encrypted under the
//! *directory key*. That key never appears in clear: it is sealed to each device's X25519 key,
//! one file per device (`key-<id>.json`), by a device that already holds it. A new phone signs
//! in, writes its file; the first PC that sees it seals the key for it; from then on it reads
//! the addresses. A leaked link shows public keys, names and noise.
//!
//! **Revoking rotates the key.** The revoked device's files go, a new key (next epoch) is sealed
//! to the others, and every device re-encrypts its addresses: the revoked one, which still knows
//! the gist's link, reads noise from then on. Each PC also keeps its own list of revoked keys,
//! which a device re-adding itself to the gist does not get past.
//!
//! **Old entries are refused.** Each file is signed by its device and dated; a date older than
//! one already seen is ignored, so an old file put back (with an old address, an old punch
//! request) changes nothing.
//!
//! One file per device is also what lets several devices write at once without losing each
//! other's changes: a gist is updated file by file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::github::{Fetch, GitHub};
use crate::identity::{Identity, PublicKeys, device_id, random, verify};
use crate::net::Addresses;

/// The description the gist is found by.
pub const GIST_DESCRIPTION: &str = "po-link";
const README_FILE: &str = "po-link.md";
const README: &str = "# po-link\n\nPo's remote control directory: each device's public keys and its addresses, encrypted. \
Written by Po; deleting this gist unpairs every device of this account.\n";
const REVOKED_FILE: &str = "revoked.json";
/// A punch request older than this is stale: the phone has given up since.
const PUNCH_FRESH: u64 = 90;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pc,
    Phone,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Pc => "pc",
            Kind::Phone => "phone",
        }
    }
}

/// A phone asks a PC to send packets its way: both firewalls then let the connection through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Punch {
    pub to: String,
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceFile {
    v: u32,
    id: String,
    ed: String,
    x: String,
    name: String,
    kind: Kind,
    updated: u64,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    punch: Option<Punch>,
    sig: String,
}

impl DeviceFile {
    fn payload(&self) -> Vec<u8> {
        let punch = self.punch.as_ref().map(|p| format!("{} {}", p.to, p.at)).unwrap_or_default();
        format!(
            "po-link device v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.id,
            self.ed,
            self.x,
            self.name,
            self.kind.as_str(),
            self.updated,
            self.kid.as_deref().unwrap_or(""),
            self.addr.as_deref().unwrap_or(""),
            punch
        )
        .into_bytes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFile {
    v: u32,
    epoch: u64,
    /// Who made the key.
    by: String,
    to: String,
    k: String,
    /// Who sealed it for `to` (any device holding it) and signs the file.
    signer: String,
    sig: String,
}

impl KeyFile {
    fn kid(&self) -> Kid {
        Kid { epoch: self.epoch, by: self.by.clone() }
    }

    fn payload(&self) -> Vec<u8> {
        format!("po-link key v1\n{}\n{}\n{}\n{}", self.epoch, self.by, self.to, self.k).into_bytes()
    }
}

/// The name of a directory key: its epoch, and the device that made it. A later epoch wins; for
/// the same epoch (two devices made one at once), the smaller maker id wins, so that every device
/// settles on the same key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Kid {
    pub epoch: u64,
    pub by: String,
}

impl Kid {
    fn better_than(&self, other: &Kid) -> bool {
        self.epoch > other.epoch || self.epoch == other.epoch && self.by < other.by
    }

    fn name(&self) -> String {
        format!("{}-{}", self.epoch, self.by)
    }

    fn parse(text: &str) -> Option<Self> {
        let (epoch, by) = text.split_once('-')?;
        Some(Self { epoch: epoch.parse().ok()?, by: by.to_owned() })
    }
}

/// Another device, as the directory shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub keys: PublicKeys,
    /// When it last wrote its entry (Unix seconds).
    pub updated: u64,
    /// Its addresses, once we hold the key they are encrypted with.
    pub addresses: Option<Addresses>,
    pub punch: Option<Punch>,
}

/// What a device keeps of the directory between two launches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    pub login: Option<String>,
    pub gist: Option<String>,
    /// Directory keys known: name → key (base64). The latest few, to read entries not yet
    /// re-encrypted after a rotation.
    pub keys: BTreeMap<String, String>,
    pub current: Option<String>,
    /// Keys never to accept again, whatever the gist says.
    pub revoked: Vec<String>,
    /// The latest date seen of each device's entry: an older one is a replay.
    pub seen: BTreeMap<String, u64>,
    /// The last punch request answered, by device.
    pub punched: BTreeMap<String, u64>,
}

/// The directory as one device sees and writes it.
pub struct Directory {
    gh: GitHub,
    identity: Identity,
    kind: Kind,
    name: String,
    path: Option<PathBuf>,
    pub local: Local,
    login: String,
    gist: String,
    etag: Option<String>,
    files: BTreeMap<String, String>,
    devices: Vec<Device>,
    addresses: Addresses,
    punch: Option<Punch>,
    /// When our own entry was last written, and what it said.
    written: Option<(u64, String)>,
    /// How often our entry is rewritten even unchanged, so that others see we are alive.
    pub heartbeat: u64,
    /// Since when a PC has waited to be given the key, and how long it waits before making the
    /// next one itself.
    waiting_since: Option<u64>,
    pub key_wait: u64,
}

fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    if name.is_empty() { "?".into() } else { name.to_owned() }
}

fn dev_file(id: &str) -> String {
    format!("dev-{id}.json")
}

fn key_file(id: &str) -> String {
    format!("key-{id}.json")
}

fn addr_aad(id: &str, kid: &str) -> Vec<u8> {
    format!("po-link addr {id} {kid}").into_bytes()
}

fn key_aad(to: &str, kid: &Kid) -> Vec<u8> {
    format!("po-link key {to} {}", kid.name()).into_bytes()
}

fn b64_32(text: &str) -> Option<[u8; 32]> {
    B64.decode(text).ok()?.try_into().ok()
}

impl Directory {
    /// Opens the account's directory (making the gist the first time). `path` keeps what this
    /// device learned (keys, dates seen); `None` keeps it in memory only.
    pub async fn open(gh: GitHub, identity: Identity, kind: Kind, name: &str, path: Option<PathBuf>) -> Result<Self, crate::github::Error> {
        let user = gh.user().await?;
        let mut local: Local = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        // Another account: nothing of the previous one is kept, its keys least of all.
        if local.login.as_deref() != Some(user.login.as_str()) {
            local = Local { login: Some(user.login.clone()), ..Local::default() };
        }
        let gist = match &local.gist {
            Some(id) => id.clone(),
            None => match gh.find_gist(GIST_DESCRIPTION).await? {
                Some(id) => id,
                None => gh.create_gist(GIST_DESCRIPTION, &[(README_FILE, README)]).await?,
            },
        };
        local.gist = Some(gist.clone());
        let directory = Self {
            gh,
            identity,
            kind,
            name: clean_name(name),
            path,
            local,
            login: user.login,
            gist,
            etag: None,
            files: BTreeMap::new(),
            devices: Vec::new(),
            addresses: Addresses::default(),
            punch: None,
            written: None,
            heartbeat: 15 * 60,
            waiting_since: None,
            key_wait: 90,
        };
        directory.save();
        Ok(directory)
    }

    pub fn login(&self) -> &str {
        &self.login
    }

    pub fn gist(&self) -> &str {
        &self.gist
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The other devices of the account (revoked ones left out).
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// The directory key is ours: the others' addresses can be read.
    pub fn has_key(&self) -> bool {
        self.local.current.is_some()
    }

    fn save(&self) {
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::write_private(path, serde_json::to_string_pretty(&self.local).unwrap_or_default().as_bytes());
        }
    }

    pub fn set_addresses(&mut self, addresses: Addresses) {
        self.addresses = addresses;
    }

    pub fn addresses(&self) -> &Addresses {
        &self.addresses
    }

    pub fn set_name(&mut self, name: &str) {
        self.name = clean_name(name);
    }

    /// Asks PC `to` to punch toward our addresses (written at the next [`Directory::sync`]).
    pub fn request_punch(&mut self, to: &str) {
        self.punch = Some(Punch { to: to.to_owned(), at: now() });
    }

    fn key(&self, kid: &str) -> Option<[u8; 32]> {
        b64_32(self.local.keys.get(kid)?)
    }

    fn current_kid(&self) -> Option<Kid> {
        self.local.current.as_deref().and_then(Kid::parse)
    }

    fn revoked(&self, id: &str) -> bool {
        self.local.revoked.iter().any(|r| r == id)
    }

    /// Reads the gist (if it changed), learns the keys, seals ours for those who lack it, and
    /// writes our entry if it changed or is getting old. True if something seen changed.
    pub async fn sync(&mut self) -> Result<bool, crate::github::Error> {
        let mut changed = false;
        match self.gh.get_gist(&self.gist, self.etag.as_deref()).await? {
            Fetch::NotModified => {}
            Fetch::Fetched { etag, files } => {
                self.etag = etag;
                self.files = files;
                changed = true;
            }
            Fetch::Gone => {
                // Deleted by the user: every device is unpaired, we start a new one.
                self.gist = self.gh.create_gist(GIST_DESCRIPTION, &[(README_FILE, README)]).await?;
                self.local = Local { login: Some(self.login.clone()), gist: Some(self.gist.clone()), revoked: self.local.revoked.clone(), ..Local::default() };
                self.etag = None;
                self.files.clear();
                self.written = None;
                changed = true;
            }
        }
        if changed {
            self.read_files();
        }
        let writes = self.writes();
        if !writes.is_empty() {
            self.gh.write_files(&self.gist, &writes).await?;
            // What we wrote is now the gist's: no need to read it back, but the ETag changed.
            for (name, content) in &writes {
                match content {
                    Some(c) => self.files.insert(name.clone(), c.clone()),
                    None => self.files.remove(name),
                };
            }
            self.etag = None;
            self.read_files();
        }
        self.save();
        Ok(changed)
    }

    fn read_files(&mut self) {
        // Keys revoked by another device of the account are revoked here too, for good.
        if let Some(list) = self.files.get(REVOKED_FILE).and_then(|t| serde_json::from_str::<Vec<String>>(t).ok()) {
            for id in list {
                if !self.revoked(&id) && id != self.identity.id() {
                    self.local.revoked.push(id);
                }
            }
        }
        let mut devices = Vec::new();
        let me = self.identity.id();
        for (name, text) in &self.files {
            let Some(id) = name.strip_prefix("dev-").and_then(|n| n.strip_suffix(".json")) else { continue };
            let Ok(file) = serde_json::from_str::<DeviceFile>(text) else { continue };
            let (Some(ed), Some(x)) = (b64_32(&file.ed), b64_32(&file.x)) else { continue };
            // The file is its device's: named after its key, signed by it.
            if file.id != id || device_id(&ed) != id || id == me || self.local.revoked.iter().any(|r| r == id) {
                continue;
            }
            let Ok(sig) = B64.decode(&file.sig) else { continue };
            if !verify(&ed, &file.payload(), &sig) {
                continue;
            }
            // An entry older than one already seen is put back from the past: ignored.
            let seen = self.local.seen.get(id).copied().unwrap_or(0);
            if file.updated < seen {
                if let Some(previous) = self.devices.iter().find(|d| d.id == id) {
                    devices.push(previous.clone());
                }
                continue;
            }
            self.local.seen.insert(id.to_owned(), file.updated);
            let addresses = match (&file.kid, &file.addr) {
                (Some(kid), Some(blob)) => self.key(kid).and_then(|k| {
                    let blob = B64.decode(blob).ok()?;
                    let plain = crate::seal::decrypt(&k, &blob, &addr_aad(id, kid))?;
                    serde_json::from_slice::<Addresses>(&plain).ok()
                }),
                _ => None,
            };
            devices.push(Device {
                id: id.to_owned(),
                name: clean_name(&file.name),
                kind: file.kind,
                keys: PublicKeys { ed, x },
                updated: file.updated,
                addresses,
                punch: file.punch,
            });
        }
        // The key sealed for us, if one is better than the one we hold.
        if let Some(file) = self.key_file_for(&me) {
            let kid = file.kid();
            let better = self.current_kid().is_none_or(|c| kid.better_than(&c));
            if better
                && let Ok(sealed) = B64.decode(&file.k)
                && let Some(key) = crate::seal::open(&self.identity, &sealed, &key_aad(&me, &kid))
                && key.len() == 32
            {
                self.local.keys.insert(kid.name(), B64.encode(&key));
                self.local.current = Some(kid.name());
                // A few keys back are enough: entries move to the new one within minutes.
                while self.local.keys.len() > 4 {
                    let oldest = self.local.keys.keys().filter_map(|k| Kid::parse(k)).min_by_key(|k| k.epoch).map(|k| k.name());
                    match oldest {
                        Some(name) if Some(&name) != self.local.current.as_ref() => self.local.keys.remove(&name),
                        _ => break,
                    };
                }
                // The addresses already read with an older key are read again now.
                return self.read_files();
            }
        }
        self.devices = devices;
    }

    /// The key file sealed for `to`, if it is valid: signed by a device of the directory (or us).
    fn key_file_for(&self, to: &str) -> Option<KeyFile> {
        let file: KeyFile = serde_json::from_str(self.files.get(&key_file(to))?).ok()?;
        if file.to != to {
            return None;
        }
        let signer = if file.signer == self.identity.id() { self.identity.public().ed } else { self.dev_key(&file.signer)? };
        verify(&signer, &file.payload(), &B64.decode(&file.sig).ok()?).then_some(file)
    }

    /// A device's signing key, from its file in the gist (even if we do not list it yet).
    fn dev_key(&self, id: &str) -> Option<[u8; 32]> {
        let file: DeviceFile = serde_json::from_str(self.files.get(&dev_file(id))?).ok()?;
        let ed = b64_32(&file.ed)?;
        (device_id(&ed) == id && !self.revoked(id)).then_some(ed)
    }

    /// The best key named in the gist, whoever it is sealed for.
    fn best_kid(&self) -> Option<Kid> {
        self.files
            .iter()
            .filter(|(n, _)| n.starts_with("key-"))
            .filter_map(|(_, t)| serde_json::from_str::<KeyFile>(t).ok())
            .map(|f| f.kid())
            .fold(None, |best: Option<Kid>, k| match best {
                Some(b) if !k.better_than(&b) => Some(b),
                _ => Some(k),
            })
    }

    fn seal_for(&self, to: &str, x: &[u8; 32], kid: &Kid, key: &[u8; 32]) -> String {
        let sealed = crate::seal::seal(x, key, &key_aad(to, kid));
        let mut file = KeyFile { v: 1, epoch: kid.epoch, by: kid.by.clone(), to: to.to_owned(), k: B64.encode(sealed), signer: self.identity.id(), sig: String::new() };
        file.sig = B64.encode(self.identity.sign(&file.payload()));
        serde_json::to_string(&file).unwrap_or_default()
    }

    /// What to write: the key for whoever lacks it, and our own entry.
    fn writes(&mut self) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        let me = self.identity.id();
        // Only a PC makes a key: a phone may be wiped or lost, and a key whose only holder is
        // gone leaves every other device waiting for it forever. The first PC of the account
        // makes one; a PC still not given the existing one after a while (its holders are gone)
        // makes the next one, which everyone then takes.
        if self.local.current.is_none() && self.kind == Kind::Pc {
            let next = match self.best_kid() {
                None => Some(1),
                Some(best) => {
                    let since = *self.waiting_since.get_or_insert_with(now);
                    (now().saturating_sub(since) >= self.key_wait).then_some(best.epoch + 1)
                }
            };
            if let Some(epoch) = next {
                let kid = Kid { epoch, by: me.clone() };
                self.local.keys.insert(kid.name(), B64.encode(random::<32>()));
                self.local.current = Some(kid.name());
            }
        }
        if let (Some(kid), Some(key)) = (self.current_kid(), self.local.current.as_deref().and_then(|k| self.key(k))) {
            // We pass on our key only when it is the best one known; else we wait to be given it.
            let best = self.best_kid();
            if best.as_ref().is_none_or(|b| !b.better_than(&kid)) {
                let mut targets: Vec<(String, [u8; 32])> = self.devices.iter().map(|d| (d.id.clone(), d.keys.x)).collect();
                targets.push((me.clone(), self.identity.public().x));
                for (id, x) in targets {
                    let has = self.key_file_for(&id).is_some_and(|f| f.kid() == kid);
                    if !has {
                        out.push((key_file(&id), Some(self.seal_for(&id, &x, &kid, &key))));
                    }
                }
            }
        }
        if let Some(entry) = self.own_entry() {
            out.push((dev_file(&me), Some(entry)));
        }
        out
    }

    /// Our entry, if it must be written: it changed, it is missing, or it is getting old.
    fn own_entry(&mut self) -> Option<String> {
        let me = self.identity.id();
        let kid = self.local.current.clone();
        let addr = match (&kid, self.local.current.as_deref().and_then(|k| self.key(k))) {
            (Some(kid), Some(key)) => {
                let plain = serde_json::to_vec(&self.addresses).unwrap_or_default();
                Some(B64.encode(crate::seal::encrypt(&key, &plain, &addr_aad(&me, kid))))
            }
            _ => None,
        };
        // What the entry says, apart from the date and the fresh nonces.
        let meaning = format!("{}|{}|{:?}|{:?}|{:?}", self.name, self.kind.as_str(), kid, serde_json::to_string(&self.addresses).ok(), self.punch);
        let in_gist = self.files.contains_key(&dev_file(&me));
        let fresh = self.written.as_ref().is_some_and(|(at, m)| *m == meaning && now().saturating_sub(*at) < self.heartbeat);
        if fresh && in_gist {
            return None;
        }
        let keys = self.identity.public();
        // Strictly later than the last one written, even within the same second.
        let updated = now().max(self.written.as_ref().map(|(at, _)| at + 1).unwrap_or(0));
        let mut file = DeviceFile {
            v: 1,
            id: me,
            ed: B64.encode(keys.ed),
            x: B64.encode(keys.x),
            name: self.name.clone(),
            kind: self.kind,
            updated,
            kid,
            addr,
            punch: self.punch.clone(),
            sig: String::new(),
        };
        file.sig = B64.encode(self.identity.sign(&file.payload()));
        self.written = Some((updated, meaning));
        Some(serde_json::to_string(&file).unwrap_or_default())
    }

    /// Punch requests addressed to us that are fresh and not yet answered: who, and where.
    pub fn take_punches(&mut self) -> Vec<(String, Vec<std::net::SocketAddr>)> {
        let me = self.identity.id();
        let mut out = Vec::new();
        for device in &self.devices {
            let Some(punch) = &device.punch else { continue };
            let answered = self.local.punched.get(&device.id).copied().unwrap_or(0);
            if punch.to == me && punch.at > answered && now().saturating_sub(punch.at) <= PUNCH_FRESH {
                if let Some(addresses) = &device.addresses {
                    out.push((device.id.clone(), addresses.candidates()));
                }
                self.local.punched.insert(device.id.clone(), punch.at);
            }
        }
        out
    }

    /// Revokes a device: its files go, it is refused from now on, and the key changes so that
    /// what is published next is out of its reach.
    pub async fn revoke(&mut self, id: &str) -> Result<(), crate::github::Error> {
        if !self.revoked(id) {
            self.local.revoked.push(id.to_owned());
        }
        self.devices.retain(|d| d.id != id);
        let next = self.current_kid().map(|k| k.epoch).max(self.best_kid().map(|k| k.epoch)).unwrap_or(0) + 1;
        let kid = Kid { epoch: next, by: self.identity.id() };
        self.local.keys.insert(kid.name(), B64.encode(random::<32>()));
        self.local.current = Some(kid.name());
        let mut list: Vec<String> = self.files.get(REVOKED_FILE).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
        if !list.iter().any(|r| r == id) {
            list.push(id.to_owned());
        }
        let removals = vec![
            (dev_file(id), None),
            (key_file(id), None),
            (REVOKED_FILE.to_owned(), Some(serde_json::to_string(&list).unwrap_or_default())),
        ];
        self.gh.write_files(&self.gist, &removals).await?;
        for (name, content) in removals {
            match content {
                Some(c) => self.files.insert(name, c),
                None => self.files.remove(&name),
            };
        }
        self.written = None;
        self.sync().await.map(|_| ())
    }

    /// Takes this device out of the directory (signing out): its files go.
    pub async fn leave(&mut self) -> Result<(), crate::github::Error> {
        let me = self.identity.id();
        self.gh.write_files(&self.gist, &[(dev_file(&me), None), (key_file(&me), None)]).await?;
        self.local = Local::default();
        self.save();
        Ok(())
    }

    /// Our entry is written now, whatever its age (a phone about to connect, a name changed).
    pub fn touch(&mut self) {
        self.written = None;
    }
}

#[cfg(test)]
mod tests;
