//! A fake GitHub on this machine, for trying Po's remote control without a real account:
//! `cargo run -p po-link --features fake --example fake_github -- 18998 alice`
//! (port, and the account every device sign-in is approved as). Point Po at it with
//! `PO_GITHUB_API` and `PO_GITHUB_WEB` set to `http://127.0.0.1:18998`, and the app with its
//! debug setting (from the emulator: `http://10.0.2.2:18998`).

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "18998".into());
    let login = args.next().unwrap_or_else(|| "alice".into());
    let fake = po_link::fake::FakeGitHub::start_on(&format!("0.0.0.0:{port}")).await;
    fake.approve_as(&login);
    println!("fake GitHub on {} — sign-ins approved as {login}", fake.url);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let lines: Vec<String> = std::mem::take(&mut fake.state.lock().unwrap().log);
        for line in lines.iter().filter(|l| !l.ends_with(" 304")) {
            println!("  {line}");
        }
        let (requests, not_modified) = fake.counts();
        let gists = fake.gists_of(&login);
        let files: usize = gists.iter().map(|(_, g)| g.files.len()).sum();
        println!("{requests} requests ({not_modified} not modified), {} gist(s), {files} file(s)", gists.len());
    }
}
