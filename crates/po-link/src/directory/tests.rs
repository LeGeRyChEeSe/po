use super::*;
use crate::fake::FakeGitHub;

fn addresses(last: u8) -> Addresses {
    Addresses { port: 41000 + last as u16, lan: vec![format!("192.168.1.{last}:{}", 41000 + last as u16).parse().unwrap()], ..Addresses::default() }
}

async fn device(fake: &FakeGitHub, login: &str, kind: Kind, name: &str, last: u8) -> Directory {
    let gh = GitHub::new(&fake.config(), fake.token_for(login));
    let mut d = Directory::open(gh, Identity::generate(), kind, name, None).await.unwrap();
    d.set_addresses(addresses(last));
    d
}

/// Every device syncs, a few rounds: what the service and the app do every few seconds.
async fn settle(devices: &mut [&mut Directory]) {
    for _ in 0..3 {
        for d in devices.iter_mut() {
            d.sync().await.unwrap();
        }
    }
}

fn set_file(fake: &FakeGitHub, gist: &str, name: &str, content: Option<String>) {
    let mut state = fake.state.lock().unwrap();
    let g = state.gists.get_mut(gist).unwrap();
    match content {
        Some(c) => g.files.insert(name.to_owned(), c),
        None => g.files.remove(name),
    };
    g.version += 1;
}

fn file(fake: &FakeGitHub, gist: &str, name: &str) -> Option<String> {
    fake.state.lock().unwrap().gists.get(gist)?.files.get(name).cloned()
}

#[tokio::test]
async fn signing_in_with_the_same_account_pairs_the_phone_and_the_pc() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    pc.sync().await.unwrap();
    assert!(pc.has_key(), "the first device of the account makes the key");
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    assert_eq!(phone.gist(), pc.gist(), "both find the same gist");
    phone.sync().await.unwrap();
    assert!(!phone.has_key(), "the phone waits for a PC to hand it the key");
    assert_eq!(phone.devices()[0].name, "Desktop");
    assert!(phone.devices()[0].addresses.is_none());
    settle(&mut [&mut pc, &mut phone]).await;
    assert!(phone.has_key());
    let seen = phone.device(&pc.identity().id()).unwrap();
    assert_eq!(seen.kind, Kind::Pc);
    assert_eq!(seen.addresses, Some(addresses(20)));
    assert_eq!(pc.device(&phone.identity().id()).unwrap().addresses, Some(addresses(50)));
    assert_eq!(fake.gists_of("alice").len(), 1);
}

#[tokio::test]
async fn a_leaked_gist_address_shows_no_address_and_cannot_be_written() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    settle(&mut [&mut pc, &mut phone]).await;
    // Someone with the link (and any GitHub account) reads the whole gist…
    let outsider = GitHub::new(&fake.config(), fake.token_for("mallory"));
    let Fetch::Fetched { files, .. } = outsider.get_gist(pc.gist(), None).await.unwrap() else { panic!() };
    let everything: String = files.values().cloned().collect();
    for secret in ["192.168.1.20", "192.168.1.50", "41020", "41050"] {
        assert!(!everything.contains(secret), "{secret} readable in the gist");
    }
    // …but writes nothing to it.
    assert!(outsider.write_files(pc.gist(), &[("dev-x.json".into(), Some("{}".into()))]).await.is_err());
    // And a directory of another account is another gist, where none of these devices are.
    let mut other = device(&fake, "mallory", Kind::Phone, "Evil", 66).await;
    other.sync().await.unwrap();
    assert_ne!(other.gist(), pc.gist());
    assert!(other.devices().is_empty());
}

#[tokio::test]
async fn an_entry_changed_without_its_key_is_ignored() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    settle(&mut [&mut pc, &mut phone]).await;
    let name = format!("dev-{}.json", phone.identity().id());
    let original = file(&fake, pc.gist(), &name).unwrap();
    // Renamed by hand, the signature no longer matches: the entry is left out.
    set_file(&fake, pc.gist(), &name, Some(original.replace("Pixel", "Pwned")));
    pc.sync().await.unwrap();
    assert!(pc.device(&phone.identity().id()).is_none());
    // Another key's entry, filed under the phone's name: left out too.
    let intruder = Identity::generate();
    let forged = original.replace(&phone.identity().id(), &intruder.id());
    set_file(&fake, pc.gist(), &format!("dev-{}.json", intruder.id()), Some(forged));
    set_file(&fake, pc.gist(), &name, Some(original));
    pc.sync().await.unwrap();
    assert_eq!(pc.devices().len(), 1);
    assert_eq!(pc.devices()[0].id, phone.identity().id());
    // Garbage does not stop the reading of the rest.
    set_file(&fake, pc.gist(), "dev-0000000000000000.json", Some("{not json".into()));
    set_file(&fake, pc.gist(), "key-0000000000000000.json", Some("[]".into()));
    pc.sync().await.unwrap();
    assert_eq!(pc.devices().len(), 1);
}

#[tokio::test]
async fn an_old_entry_put_back_is_refused() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    settle(&mut [&mut pc, &mut phone]).await;
    let name = format!("dev-{}.json", phone.identity().id());
    let old = file(&fake, pc.gist(), &name).unwrap();
    phone.set_addresses(addresses(51));
    phone.request_punch(&pc.identity().id());
    phone.sync().await.unwrap();
    pc.sync().await.unwrap();
    assert_eq!(pc.device(&phone.identity().id()).unwrap().addresses, Some(addresses(51)));
    assert_eq!(pc.take_punches().len(), 1);
    // Someone puts the old, signed entry back: the newer one stays.
    set_file(&fake, pc.gist(), &name, Some(old));
    pc.sync().await.unwrap();
    assert_eq!(pc.device(&phone.identity().id()).unwrap().addresses, Some(addresses(51)));
    // The punch request answered is not answered again.
    assert!(pc.take_punches().is_empty());
}

#[tokio::test]
async fn a_revoked_phone_reads_nothing_new_and_cannot_come_back() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    let mut lost = device(&fake, "alice", Kind::Phone, "Lost phone", 50).await;
    let mut kept = device(&fake, "alice", Kind::Phone, "Tablet", 60).await;
    settle(&mut [&mut pc, &mut lost, &mut kept]).await;
    assert!(lost.device(&pc.identity().id()).unwrap().addresses.is_some());
    pc.revoke(&lost.identity().id()).await.unwrap();
    pc.set_addresses(addresses(21));
    settle(&mut [&mut pc, &mut kept]).await;
    assert!(file(&fake, pc.gist(), &format!("dev-{}.json", lost.identity().id())).is_none());
    assert_eq!(kept.device(&pc.identity().id()).unwrap().addresses, Some(addresses(21)), "the others get the new key");
    // The lost phone still has a token and the gist's link: it reads, and even writes itself back.
    lost.touch();
    lost.sync().await.unwrap();
    assert_eq!(lost.device(&pc.identity().id()).unwrap().addresses, None, "the new addresses are out of its reach");
    pc.sync().await.unwrap();
    assert!(pc.device(&lost.identity().id()).is_none(), "refused, whatever the gist says");
    kept.sync().await.unwrap();
    assert!(kept.device(&lost.identity().id()).is_none(), "the revocation reached the other devices");
}

#[tokio::test]
async fn two_pcs_making_a_key_at_once_settle_on_one() {
    let fake = FakeGitHub::start().await;
    let mut a = device(&fake, "alice", Kind::Pc, "A", 1).await;
    let mut b = device(&fake, "alice", Kind::Pc, "B", 2).await;
    // Neither saw a key before making its own.
    a.sync().await.unwrap();
    b.local.current = None;
    b.local.keys.clear();
    b.files.retain(|n, _| !n.starts_with("key-"));
    let _ = b.writes();
    for _ in 0..4 {
        a.sync().await.unwrap();
        b.sync().await.unwrap();
    }
    assert_eq!(a.local.current, b.local.current);
    assert_eq!(a.device(&b.identity().id()).unwrap().addresses, Some(addresses(2)));
    assert_eq!(b.device(&a.identity().id()).unwrap().addresses, Some(addresses(1)));
}

#[tokio::test]
async fn polling_an_unchanged_gist_costs_nothing() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    pc.sync().await.unwrap();
    pc.sync().await.unwrap();
    let (_, before) = fake.counts();
    for _ in 0..5 {
        assert!(!pc.sync().await.unwrap());
    }
    let (_, after) = fake.counts();
    assert_eq!(after - before, 5, "five conditional requests, five 304");
}

#[tokio::test]
async fn a_deleted_gist_is_made_again() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    pc.sync().await.unwrap();
    let old = pc.gist().to_owned();
    fake.state.lock().unwrap().gists.remove(&old);
    pc.sync().await.unwrap();
    assert_ne!(pc.gist(), old);
    assert!(file(&fake, pc.gist(), &format!("dev-{}.json", pc.identity().id())).is_some());
}

#[tokio::test]
async fn github_down_is_an_error_not_a_loss() {
    let fake = FakeGitHub::start().await;
    let mut pc = device(&fake, "alice", Kind::Pc, "Desktop", 20).await;
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    settle(&mut [&mut pc, &mut phone]).await;
    fake.fail_next(3);
    assert!(pc.sync().await.is_err());
    assert_eq!(pc.devices().len(), 1, "what was known stays known");
    fake.fail_next(0);
    pc.sync().await.unwrap();
    assert!(pc.device(&phone.identity().id()).is_some());
}

#[tokio::test]
async fn the_state_survives_a_restart_and_another_account_starts_afresh() {
    let fake = FakeGitHub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("directory.json");
    let identity = Identity::generate();
    let token = fake.token_for("alice");
    let mut first = Directory::open(GitHub::new(&fake.config(), &token), identity.clone(), Kind::Pc, "PC", Some(path.clone())).await.unwrap();
    first.sync().await.unwrap();
    let kid = first.local.current.clone();
    let again = Directory::open(GitHub::new(&fake.config(), &token), identity.clone(), Kind::Pc, "PC", Some(path.clone())).await.unwrap();
    assert_eq!(again.local.current, kid);
    let bob = Directory::open(GitHub::new(&fake.config(), fake.token_for("bob")), identity, Kind::Pc, "PC", Some(path)).await.unwrap();
    assert!(bob.local.current.is_none(), "alice's key is not kept for bob");
    assert_ne!(bob.gist(), again.gist());
}

#[tokio::test]
async fn a_key_whose_holders_are_gone_is_replaced_by_the_next_pc() {
    let fake = FakeGitHub::start().await;
    // A phone alone: it makes no key, it waits.
    let mut phone = device(&fake, "alice", Kind::Phone, "Pixel", 50).await;
    phone.sync().await.unwrap();
    assert!(!phone.has_key());
    // A first PC makes one, then is gone for good (reinstalled: a new identity).
    let mut old = device(&fake, "alice", Kind::Pc, "Old", 1).await;
    old.sync().await.unwrap();
    assert!(old.has_key());
    drop(old);
    let mut pc = device(&fake, "alice", Kind::Pc, "New", 2).await;
    pc.key_wait = 0;
    settle(&mut [&mut pc, &mut phone]).await;
    assert!(pc.has_key(), "the new PC made the next key");
    assert_eq!(phone.device(&pc.identity().id()).unwrap().addresses, Some(addresses(2)));
    assert_eq!(pc.local.current.as_deref().map(|k| k.starts_with("2-")), Some(true));
}
