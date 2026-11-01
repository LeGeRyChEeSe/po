//! The phone's core against a PC on localhost and the fake GitHub, as the app uses it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use po_link::fake::FakeGitHub;
use po_link::tls::Peer;
use po_wire::{Catchup, Event, Feed, Journal, Reply, Request, Subscribe};
use tokio::sync::{broadcast, mpsc};

use super::*;

struct Session {
    journal: Mutex<Journal>,
    live: broadcast::Sender<(u64, Event)>,
    inputs: Mutex<Vec<po_wire::Input>>,
}

impl Session {
    fn push(&self, event: Event) {
        let mut j = self.journal.lock().unwrap();
        let seq = j.push(event.clone());
        let _ = self.live.send((seq, event));
    }
}

#[async_trait::async_trait]
impl po_link::Backend for Session {
    fn name(&self) -> String {
        "Desk".into()
    }

    async fn request(&self, _: &Peer, request: Request) -> Reply {
        match request {
            Request::Projects => Reply::Projects { projects: vec![po_wire::ProjectInfo { path: "C:/p".into(), name: "p".into(), saved: 2, live: 1, last: String::new() }] },
            Request::Input { input, .. } => {
                self.inputs.lock().unwrap().push(input);
                Reply::Done { text: "ok".into() }
            }
            Request::NewSession { .. } => Reply::Session { key: "live:1".into() },
            _ => Reply::Error { message: "no".into() },
        }
    }

    async fn subscribe(&self, _: &Peer, sub: Subscribe) -> mpsc::Receiver<Feed> {
        let (tx, rx) = mpsc::channel(256);
        let (catchup, epoch, mut live) = {
            let j = self.journal.lock().unwrap();
            (j.since(sub.epoch.as_deref(), sub.since), j.epoch.clone(), self.live.subscribe())
        };
        tokio::spawn(async move {
            let mut last = sub.since;
            match catchup {
                Catchup::Snapshot { seq, conversation } => {
                    last = seq;
                    let _ = tx.send(Feed::Snapshot { epoch, seq, conversation }).await;
                }
                Catchup::Events(events) => {
                    for (seq, event) in events {
                        last = seq;
                        let _ = tx.send(Feed::Event { seq, event }).await;
                    }
                }
            }
            while let Ok((seq, event)) = live.recv().await {
                if seq > last {
                    last = seq;
                    if tx.send(Feed::Event { seq, event }).await.is_err() {
                        break;
                    }
                }
            }
        });
        rx
    }
}

/// The app's secret storage, in memory: what the Keystore or the Keychain do on a phone.
#[derive(Default)]
struct Memory(Mutex<std::collections::HashMap<String, Vec<u8>>>);

impl SecretStore for Memory {
    fn load(&self, name: String) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(&name).cloned()
    }
    fn save(&self, name: String, value: Vec<u8>) -> bool {
        self.0.lock().unwrap().insert(name, value);
        true
    }
    fn remove(&self, name: String) {
        self.0.lock().unwrap().remove(&name);
    }
}

/// Every file under `dir`: none may hold a secret in clear.
fn plain_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir).map(|e| e.flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

#[derive(Default)]
struct Heard {
    updates: Mutex<Vec<SessionUpdate>>,
    links: Mutex<Vec<LinkState>>,
    alerts: Mutex<Vec<Alert>>,
}

impl Listener for Heard {
    fn on_session(&self, _: String, _: String, update: SessionUpdate) {
        self.updates.lock().unwrap().push(update);
    }
    fn on_link(&self, _: String, state: LinkState) {
        self.links.lock().unwrap().push(state);
    }
    fn on_alert(&self, _: String, _: String, alert: Alert) {
        self.alerts.lock().unwrap().push(alert);
    }
}

/// The rows the app would show, rebuilt from the updates as it does.
fn rows(updates: &[SessionUpdate]) -> Vec<ItemRec> {
    let mut rows: Vec<ItemRec> = Vec::new();
    for u in updates {
        rows.truncate(u.from as usize);
        rows.extend(u.items.iter().cloned());
    }
    rows
}

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never: {what}");
}

#[test]
fn the_app_signs_in_finds_the_pc_follows_a_session_and_acts() {
    let pc_runtime = tokio::runtime::Runtime::new().unwrap();
    let session = Arc::new(Session { journal: Mutex::new(Journal::new("run", 1000)), live: broadcast::channel(1000).0, inputs: Mutex::new(Vec::new()) });
    session.push(Event::User { text: "hello".into() });
    let (fake, pc) = pc_runtime.block_on(async {
        let fake = FakeGitHub::start().await;
        let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let options = po_link::pc::Options {
            port,
            name: "Desk".into(),
            poll: Duration::from_millis(200),
            discover: false,
            extra: vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))],
            ..Default::default()
        };
        let gh = po_link::github::GitHub::new(&fake.config(), fake.token_for("alice"));
        let pc = po_link::pc::Pc::start(Identity::generate(), gh, options, session.clone()).await.unwrap();
        (fake, pc)
    });
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(Memory::default());
    let core = PoCore::new(dir.path().display().to_string(), "Pixel".into(), fake.url.clone(), fake.url.clone(), vec![], secrets.clone()).unwrap();
    let heard = Arc::new(Heard::default());
    core.set_listener(heard.clone());
    assert_eq!(core.resume().unwrap(), None, "nothing remembered yet");
    let code = core.sign_in_start().unwrap();
    assert!(!code.code.is_empty());
    fake.approve_as("alice");
    assert_eq!(core.sign_in_wait().unwrap(), "alice");
    // The token went to the secret storage, not to a file.
    assert!(secrets.load("github".into()).is_some_and(|t| String::from_utf8_lossy(&t).contains("gho_fake")));
    // The PC sees the phone, seals it the key: the phone then reads the PC's addresses.
    let mut found = None;
    wait_for("the PC accepts the phone", || {
        let list = core.pcs().unwrap();
        found = list.pcs.iter().find(|p| p.reachable).map(|p| p.id.clone());
        list.ready && found.is_some()
    });
    let pc_id = found.unwrap();
    wait_for("the PC trusts the phone", || pc.trust.get(&core.inner.phone.identity.public().ed).is_some());
    core.connect(pc_id.clone()).unwrap();
    assert!(heard.links.lock().unwrap().iter().any(|l| matches!(l, LinkState::Up { name, .. } if name == "Desk")));
    assert_eq!(core.projects(pc_id.clone()).unwrap()[0].name, "p");
    core.open(pc_id.clone(), "live:1".into());
    wait_for("the snapshot", || rows(&heard.updates.lock().unwrap()).first().is_some_and(|r| r.text == "hello"));
    session.push(Event::TurnStarted);
    session.push(Event::TextDelta { text: "Wor".into() });
    session.push(Event::TextDelta { text: "king".into() });
    session.push(Event::Approval { id: 9, tool: "bash".into(), summary: "cargo test".into() });
    wait_for("the approval", || heard.updates.lock().unwrap().last().is_some_and(|u| u.approval.as_ref().is_some_and(|a| a.id == 9)));
    let shown = rows(&heard.updates.lock().unwrap());
    assert_eq!(shown[1].kind, ItemKind::Assistant);
    assert_eq!(shown[1].text, "Working");
    assert!(heard.alerts.lock().unwrap().iter().any(|a| matches!(a, Alert::Approval { tool, .. } if tool == "bash")));
    core.send(pc_id.clone(), "live:1".into(), InputRec::Approve { id: 9, granted: true }).unwrap();
    assert_eq!(session.inputs.lock().unwrap()[0], po_wire::Input::Approve { id: 9, granted: true });
    session.push(Event::ApprovalDone { id: 9, granted: true });
    session.push(Event::TurnEnded { interrupted: false, error: None, summary: Some("1 s".into()) });
    wait_for("the end of the turn", || heard.alerts.lock().unwrap().iter().any(|a| matches!(a, Alert::TurnEnded { last, .. } if last == "Working")));
    // The link goes (a tunnel): the session follows again from where it was, nothing twice.
    core.disconnect(pc_id.clone());
    session.push(Event::Notice { text: "while away".into() });
    wait_for("the event missed", || rows(&heard.updates.lock().unwrap()).iter().any(|r| r.text == "while away"));
    let shown = rows(&heard.updates.lock().unwrap());
    assert_eq!(shown.iter().filter(|r| r.text == "hello").count(), 1);
    core.close_view(pc_id.clone(), "live:1".into());
    assert_eq!(core.new_session(pc_id, "C:/p".into(), None).unwrap(), "live:1");
    // Launched again: the account is remembered.
    // Keys, token and the directory's state (its keys) are all in the secret storage.
    assert!(plain_files(dir.path()).is_empty(), "plain files left: {:?}", plain_files(dir.path()));
    assert!(secrets.load("directory".into()).is_some() && secrets.load("identity".into()).is_some());
    let id = core.device_id();
    drop(core);
    let again = PoCore::new(dir.path().display().to_string(), "Pixel".into(), fake.url.clone(), fake.url.clone(), vec![], secrets.clone()).unwrap();
    assert_eq!(again.device_id(), id, "the same key after a restart");
    assert_eq!(again.resume().unwrap().as_deref(), Some("alice"));
    again.sign_out();
    assert_eq!(again.account(), None);
    assert!(secrets.load("github".into()).is_none() && secrets.load("directory".into()).is_none());
    drop(pc);
}

#[test]
fn a_conversation_becomes_flat_rows() {
    let mut c = Conversation::default();
    c.apply(&Event::ToolStarted { name: "edit".into(), summary: "a.rs".into() });
    c.apply(&Event::ToolFinished { name: "edit".into(), summary: "a.rs (+2)".into(), error: false });
    c.apply(&Event::ToolDiff { diff: "+x\n+y".into() });
    c.apply(&Event::CommandOutput { command: "/cost".into(), lines: vec![po_wire::OutLine::Field { key: "total".into(), value: "$0".into() }] });
    let u = SessionUpdate::from_conversation(&c, 0);
    assert_eq!(u.items[0].kind, ItemKind::Tool);
    assert_eq!(u.items[0].status, ToolStatus::Done);
    assert_eq!(u.items[0].detail, "+x\n+y");
    assert_eq!(u.items[1].lines[0].kind, "field");
    assert_eq!(SessionUpdate::from_conversation(&c, 99).items.len(), 0);
}

#[test]
fn secrets_kept_in_plain_files_move_to_the_secret_storage() {
    // What an earlier version left in the app's folder.
    let dir = tempfile::tempdir().unwrap();
    let old = Identity::generate();
    std::fs::write(dir.path().join("identity.json"), old.to_stored()).unwrap();
    std::fs::write(dir.path().join("github.json"), br#"{"api":"https://api.github.com","token":"gho_old"}"#).unwrap();
    std::fs::write(dir.path().join("directory.json"), b"{}").unwrap();
    let secrets = Arc::new(Memory::default());
    let core = PoCore::new(dir.path().display().to_string(), "Pixel".into(), String::new(), String::new(), vec![], secrets.clone()).unwrap();
    assert_eq!(core.device_id(), old.id(), "the phone keeps its identity");
    assert!(plain_files(dir.path()).is_empty(), "{:?}", plain_files(dir.path()));
    assert_eq!(core.inner.stored_token().as_deref(), Some("gho_old"));
    assert_eq!(secrets.load("directory".into()).as_deref(), Some(&b"{}"[..]));
}

/// A storage that refuses everything (a locked keystore): nothing is lost, nothing is written
/// in clear.
struct Refusing;

impl SecretStore for Refusing {
    fn load(&self, _: String) -> Option<Vec<u8>> {
        None
    }
    fn save(&self, _: String, _: Vec<u8>) -> bool {
        false
    }
    fn remove(&self, _: String) {}
}

#[test]
fn a_storage_that_refuses_keeps_the_old_files_and_writes_nothing_in_clear() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("github.json"), b"{}").unwrap();
    assert!(PoCore::new(dir.path().display().to_string(), "Pixel".into(), String::new(), String::new(), vec![], Arc::new(Refusing)).is_err());
    assert_eq!(plain_files(dir.path()), vec!["github.json".to_owned()], "left for the next launch, nothing added");
}
