//! Two ends on localhost: a PC (as the service runs it) and phones, through the fake GitHub.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use po_wire::{Catchup, Event, Feed, Input, Journal, Reply, Request, Subscribe};
use tokio::sync::{broadcast, mpsc};

use crate::fake::FakeGitHub;
use crate::github::GitHub;
use crate::identity::Identity;
use crate::link::Backend;
use crate::pc::{Options, Pc};
use crate::phone::Phone;
use crate::tls::Peer;

/// A PC with one session, whose events the test pushes.
struct Session {
    journal: Mutex<Journal>,
    live: broadcast::Sender<(u64, Event)>,
    inputs: Mutex<Vec<Input>>,
}

impl Session {
    fn new() -> Arc<Self> {
        Arc::new(Self { journal: Mutex::new(Journal::new(format!("{:x}", rand_u64()), 50_000)), live: broadcast::channel(100_000).0, inputs: Mutex::new(Vec::new()) })
    }

    fn push(&self, event: Event) {
        let mut journal = self.journal.lock().unwrap();
        let seq = journal.push(event.clone());
        let _ = self.live.send((seq, event));
    }
}

fn rand_u64() -> u64 {
    u64::from_le_bytes(crate::identity::random::<8>())
}

#[async_trait]
impl Backend for Session {
    fn name(&self) -> String {
        "Test PC".into()
    }

    async fn request(&self, _peer: &Peer, request: Request) -> Reply {
        match request {
            Request::Projects => Reply::Projects { projects: vec![po_wire::ProjectInfo { path: "D:/p".into(), name: "p".into(), ..Default::default() }] },
            Request::Input { input, .. } => {
                self.inputs.lock().unwrap().push(input);
                Reply::Done { text: "ok".into() }
            }
            Request::NewSession { .. } => {
                // Opening a session takes a while: other requests must not wait for it.
                tokio::time::sleep(Duration::from_millis(800)).await;
                Reply::Session { key: "new".into() }
            }
            _ => Reply::Error { message: "unsupported".into() },
        }
    }

    async fn subscribe(&self, _peer: &Peer, subscribe: Subscribe) -> mpsc::Receiver<Feed> {
        let (tx, rx) = mpsc::channel(1024);
        let (catchup, epoch, mut live) = {
            let journal = self.journal.lock().unwrap();
            (journal.since(subscribe.epoch.as_deref(), subscribe.since), journal.epoch.clone(), self.live.subscribe())
        };
        tokio::spawn(async move {
            let mut last = match catchup {
                Catchup::Snapshot { seq, conversation } => {
                    let _ = tx.send(Feed::Snapshot { epoch, seq, conversation }).await;
                    seq
                }
                Catchup::Events(events) => {
                    let mut last = subscribe.since;
                    for (seq, event) in events {
                        last = seq;
                        let _ = tx.send(Feed::Event { seq, event }).await;
                    }
                    last
                }
            };
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

fn localhost(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// A PC listening on localhost for the account `login`.
async fn pc(fake: &FakeGitHub, login: &str, session: Arc<Session>) -> Pc {
    let identity = Identity::generate();
    // The port is chosen first, so that localhost can be published as its address.
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let options = Options { port, name: "Test PC".into(), poll: Duration::from_millis(200), discover: false, extra: vec![localhost(port)], ..Options::default() };
    Pc::start(identity, GitHub::new(&fake.config(), fake.token_for(login)), options, session).await.unwrap()
}

async fn phone(fake: &FakeGitHub, login: &str) -> Phone {
    let phone = Phone::new(Identity::generate(), "Test phone", false).unwrap();
    phone.attach(GitHub::new(&fake.config(), fake.token_for(login)), None).await.unwrap();
    phone
}

/// Waits until the PC has sealed the key for the phone and lists it.
async fn paired(pc: &Pc, phone: &Phone) -> String {
    let pc_id = pc.directory.lock().await.identity().id();
    for _ in 0..100 {
        pc.sync_now().await.unwrap();
        if let Ok((pcs, true)) = phone.pcs().await
            && pcs.iter().any(|d| d.id == pc_id && d.addresses.is_some())
            && pc.trust.get(&phone.identity.public().ed).is_some()
        {
            return pc_id;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never paired");
}

async fn next_event(feed: &mut mpsc::Receiver<Feed>) -> (u64, Event) {
    match tokio::time::timeout(Duration::from_secs(10), feed.recv()).await {
        Ok(Some(Feed::Event { seq, event })) => (seq, event),
        other => panic!("expected an event, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_of_the_account_connects_follows_and_acts() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    session.push(Event::User { text: "before".into() });
    let pc = pc(&fake, "alice", session.clone()).await;
    let phone = phone(&fake, "alice").await;
    let pc_id = paired(&pc, &phone).await;
    let (link, _route) = phone.connect(&pc_id).await.unwrap();
    assert_eq!(link.name, "Test PC");
    assert_eq!(link.pc, pc_id);
    let Reply::Projects { projects } = link.request(Request::Projects, Duration::from_secs(5)).await.unwrap() else { panic!() };
    assert_eq!(projects[0].name, "p");
    let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: None, since: 0 }).await.unwrap();
    let Some(Feed::Snapshot { conversation, seq, .. }) = feed.recv().await else { panic!() };
    assert_eq!(seq, 1);
    assert_eq!(conversation.items.len(), 1);
    session.push(Event::TextDelta { text: "live".into() });
    assert_eq!(next_event(&mut feed).await, (2, Event::TextDelta { text: "live".into() }));
    // An input sent twice (the answer was lost) is carried out once.
    let input = Request::Input { session: "s".into(), id: 1, input: Input::Prompt { text: "go".into() } };
    link.request(input.clone(), Duration::from_secs(5)).await.unwrap();
    assert_eq!(link.request(input, Duration::from_secs(5)).await.unwrap(), Reply::Done { text: "duplicate".into() });
    assert_eq!(session.inputs.lock().unwrap().len(), 1);
    // A slow request does not hold up a quick one.
    let started = std::time::Instant::now();
    let (slow, quick) = tokio::join!(
        link.request(Request::NewSession { project: "p".into(), prompt: None }, Duration::from_secs(5)),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let r = link.request(Request::Ping { t: 9 }, Duration::from_secs(5)).await;
            (r, started.elapsed())
        }
    );
    assert_eq!(slow.unwrap(), Reply::Session { key: "new".into() });
    assert_eq!(quick.0.unwrap(), Reply::Pong { t: 9 });
    assert!(quick.1 < Duration::from_millis(600), "the ping waited for the slow request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_the_directory_does_not_list_is_refused_before_any_message() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    let pc = pc(&fake, "alice", session.clone()).await;
    let port = pc.status().port;
    let pc_key = pc.directory.lock().await.identity().public().ed;
    // A phone of another account, which somehow knows the PC's address and key.
    let stranger = phone(&fake, "mallory").await;
    let config = crate::tls::client_config(&stranger.identity, pc_key);
    let result = crate::net::race(&stranger.node.endpoint, config, &[localhost(port)], Duration::from_secs(3)).await;
    match result {
        Err(crate::net::RaceError::Refused(_)) => {}
        Ok(connection) => {
            // Some stacks report the refusal only on the first stream.
            assert!(crate::link::Link::open(connection, "x", "x").await.is_err());
        }
        Err(other) => panic!("{other:?}"),
    }
    assert!(session.inputs.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_does_not_talk_to_a_pc_with_another_key() {
    let fake = FakeGitHub::start().await;
    let real = pc(&fake, "alice", Session::new()).await;
    let phone = phone(&fake, "alice").await;
    paired(&real, &phone).await;
    // Another machine answering at an address: the phone expects the real PC's key.
    let impostor = pc(&fake, "mallory", Session::new()).await;
    let config = crate::tls::client_config(&phone.identity, real.directory.lock().await.identity().public().ed);
    let result = crate::net::race(&phone.node.endpoint, config, &[localhost(impostor.status().port)], Duration::from_secs(3)).await;
    assert!(result.is_err(), "connected to an impostor");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_revoked_while_connected_is_cut_off() {
    let fake = FakeGitHub::start().await;
    let pc = pc(&fake, "alice", Session::new()).await;
    let phone = phone(&fake, "alice").await;
    let pc_id = paired(&pc, &phone).await;
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    pc.revoke(&phone.identity.id()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(6), link.closed()).await.expect("still connected after revocation");
    // And it cannot come back.
    assert!(phone.connect(&pc_id).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_cut_the_phone_gets_what_it_missed_and_nothing_twice() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    let pc = pc(&fake, "alice", session.clone()).await;
    let phone = phone(&fake, "alice").await;
    let pc_id = paired(&pc, &phone).await;
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: None, since: 0 }).await.unwrap();
    let Some(Feed::Snapshot { epoch, .. }) = feed.recv().await else { panic!() };
    for i in 0..10 {
        session.push(Event::Notice { text: i.to_string() });
    }
    let mut last = 0;
    for _ in 0..10 {
        last = next_event(&mut feed).await.0;
    }
    // The tunnel: the connection goes, events go on.
    link.close();
    drop(feed);
    for i in 10..25 {
        session.push(Event::Notice { text: i.to_string() });
    }
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: Some(epoch.clone()), since: last }).await.unwrap();
    for i in 10..25 {
        let (seq, event) = next_event(&mut feed).await;
        assert_eq!(seq, last + 1 + (i - 10) as u64);
        assert_eq!(event, Event::Notice { text: i.to_string() });
    }
    // Another run of the session (a restart): the phone gets a snapshot, not wrong numbers.
    let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: Some("other run".into()), since: last }).await.unwrap();
    assert!(matches!(feed.recv().await, Some(Feed::Snapshot { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stress_many_events_many_followers_and_a_reconnection_storm() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    let pc = pc(&fake, "alice", session.clone()).await;
    let phone = Arc::new(phone(&fake, "alice").await);
    let pc_id = paired(&pc, &phone).await;
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    let link = Arc::new(link);
    // Eight followers of the same session.
    let mut followers = Vec::new();
    for _ in 0..8 {
        let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: None, since: 0 }).await.unwrap();
        assert!(matches!(feed.recv().await, Some(Feed::Snapshot { .. })));
        followers.push(feed);
    }
    const N: u64 = 20_000;
    let pusher = {
        let session = session.clone();
        tokio::spawn(async move {
            for i in 0..N {
                session.push(Event::TextDelta { text: format!("{i} ") });
                if i % 500 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        })
    };
    let started = std::time::Instant::now();
    let mut checks = Vec::new();
    for mut feed in followers {
        checks.push(tokio::spawn(async move {
            let mut expected = 1;
            while expected <= N {
                let (seq, _) = next_event(&mut feed).await;
                assert_eq!(seq, expected, "out of order or lost");
                expected += 1;
            }
        }));
    }
    pusher.await.unwrap();
    for c in checks {
        c.await.unwrap();
    }
    let elapsed = started.elapsed();
    eprintln!("stress: {} events to 8 followers in {elapsed:?}", N);
    assert!(elapsed < Duration::from_secs(60));
    // A reconnection storm: thirty connections opened and dropped in a row, then one that works.
    for _ in 0..30 {
        let (l, _) = phone.connect(&pc_id).await.unwrap();
        l.close();
    }
    let (l, _) = phone.connect(&pc_id).await.unwrap();
    assert_eq!(l.request(Request::Ping { t: 1 }, Duration::from_secs(5)).await.unwrap(), Reply::Pong { t: 1 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_traffic_does_not_bring_the_pc_down() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    let pc = pc(&fake, "alice", session.clone()).await;
    let port = pc.status().port;
    // Garbage at the UDP port: random datagrams, fake QUIC headers, oversized ones.
    let junk = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    for i in 0..2000u32 {
        let len = (i as usize * 37) % 1400 + 1;
        let mut data: Vec<u8> = (0..len).map(|j| (i as usize * 131 + j * 7) as u8).collect();
        if i % 3 == 0 {
            data[0] = 0xc0; // a long-header lookalike
        }
        let _ = junk.send_to(&data, localhost(port));
    }
    let phone = phone(&fake, "alice").await;
    let pc_id = paired(&pc, &phone).await;
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    // An authenticated phone sending nonsense on a stream: that stream fails, not the PC.
    let connection = {
        let config = crate::tls::client_config(&phone.identity, pc.directory.lock().await.identity().public().ed);
        crate::net::race(&phone.node.endpoint, config, &[localhost(port)], Duration::from_secs(3)).await.unwrap()
    };
    for tag in *b"CFZ" {
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(&[tag]).await.unwrap();
        // A frame claiming 4 GB, then bytes that are not JSON.
        send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        send.write_all(&[0xff; 64]).await.unwrap();
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(2), recv.read_to_end(1024)).await;
    }
    // A frame that is JSON but not a request.
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    send.write_all(b"C").await.unwrap();
    send.write_all(&po_wire::encode(&serde_json::json!({"rid": 1, "op": "rm -rf"}))).await.unwrap();
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), recv.read_to_end(1024)).await;
    // The PC still answers, on the other connection.
    assert_eq!(link.request(Request::Ping { t: 2 }, Duration::from_secs(5)).await.unwrap(), Reply::Pong { t: 2 });
    assert!(session.inputs.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_changing_networks_keeps_its_connection() {
    let fake = FakeGitHub::start().await;
    let session = Session::new();
    let pc = pc(&fake, "alice", session.clone()).await;
    let phone = phone(&fake, "alice").await;
    let pc_id = paired(&pc, &phone).await;
    let (link, _) = phone.connect(&pc_id).await.unwrap();
    let mut feed = link.subscribe(&Subscribe { session: "s".into(), epoch: None, since: 0 }).await.unwrap();
    assert!(matches!(feed.recv().await, Some(Feed::Snapshot { .. })));
    let before = phone.node.port();
    // Wi-Fi to mobile: a new socket, a new port; QUIC moves the connection over.
    phone.network_changed();
    assert_ne!(phone.node.port(), before);
    for round in 0..3 {
        session.push(Event::Notice { text: format!("after {round}") });
        let (_, event) = next_event(&mut feed).await;
        assert_eq!(event, Event::Notice { text: format!("after {round}") });
        assert_eq!(link.request(Request::Ping { t: round }, Duration::from_secs(5)).await.unwrap(), Reply::Pong { t: round });
        phone.network_changed();
    }
    assert!(!link.is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_signed_in_before_any_pc_waits_then_connects() {
    let fake = FakeGitHub::start().await;
    let phone = phone(&fake, "alice").await;
    // The first device of the account makes the key, phone or not: it hands it to the PC later.
    let (pcs, has_key) = phone.pcs().await.unwrap();
    assert!(pcs.is_empty() && has_key);
    let pc = pc(&fake, "alice", Session::new()).await;
    let pc_id = paired(&pc, &phone).await;
    assert!(phone.connect(&pc_id).await.is_ok());
}
