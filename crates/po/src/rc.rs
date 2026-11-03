//! `/rc`: the phone's remote control, from inside Po - the GitHub account it pairs through, the
//! PC's name, the devices, and the background service that keeps the link.
//!
//! Pairing a phone is signing in with the same GitHub account in Po and in the app: there is no
//! code to copy. The connection is remembered for every session (the one Po Labs uses to share
//! contributions too), and can be switched or dropped here at any time.
//!
//! In the full-screen interface every screen of `/rc` is a menu, built again every second while
//! it is open: the connection validated on GitHub, a phone that shows up, the service that starts
//! are seen as they happen. Esc goes back to the screen above. Without the full screen (and from
//! the phone), the same words are commands that answer in text.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use po_i18n::t;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::service;

/// The word of the interface's language: every subcommand exists in both.
fn w(fr: &'static str, en: &'static str) -> &'static str {
    if po_i18n::lang() == po_i18n::Lang::Fr { fr } else { en }
}

/// Where the phone app is downloaded from.
pub fn app_link() -> String {
    format!("https://github.com/{}/releases/latest", crate::github::Config::from_env().upstream)
}

// ---------------------------------------------------------------------------------------------
// The GitHub connection under way.

/// A connection started from `/rc`: the page, the code, and how it ended once it has.
struct Login {
    /// Which one: a newer one replaces it, the older one's outcome is then ignored.
    id: u64,
    url: String,
    code: String,
    deadline: Instant,
    outcome: Option<Result<String, String>>,
    /// When the screen saying it is done was last built: closed for a while, `/rc` gives the
    /// menu again.
    welcome_seen: Option<Instant>,
    dismissed: bool,
}

static LOGIN: Mutex<Option<Login>> = Mutex::new(None);

fn login_state<R>(f: impl FnOnce(&mut Option<Login>) -> R) -> R {
    f(&mut LOGIN.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Starts a connection: GitHub gives a page and a code; Po waits in the background.
async fn start_login() -> Result<(), String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (url, code, expires) = crate::github::login_in_background(&crate::github::Config::from_env(), move |outcome| {
        login_state(|login| {
            if let Some(login) = login.as_mut().filter(|l| l.id == id) {
                login.outcome = Some(outcome);
            }
        });
        forget_login_cache();
    })
    .await?;
    login_state(|login| {
        *login = Some(Login { id, url, code, deadline: Instant::now() + Duration::from_secs(expires), outcome: None, welcome_seen: None, dismissed: false });
    });
    // The service waits for the connection and starts the link with it.
    if service::running().is_none() {
        service::start()?;
    }
    Ok(())
}

/// A connection waiting for its code, or one that failed: what the login screen shows.
fn pending_login() -> Option<(String, String, Duration, Option<String>)> {
    login_state(|login| {
        let l = login.as_ref()?;
        match &l.outcome {
            None if Instant::now() < l.deadline => Some((l.url.clone(), l.code.clone(), l.deadline - Instant::now(), None)),
            None => Some((l.url.clone(), l.code.clone(), Duration::ZERO, Some(t!("labs.github_expired").to_owned()))),
            Some(Err(e)) => Some((l.url.clone(), l.code.clone(), Duration::ZERO, Some(e.clone()))),
            Some(Ok(_)) => None,
        }
    })
}

/// The account just connected, while its welcome screen is open (or was a moment ago).
fn just_connected() -> Option<String> {
    login_state(|login| {
        let l = login.as_mut()?;
        let Some(Ok(name)) = &l.outcome else { return None };
        if l.dismissed || l.welcome_seen.is_some_and(|at| at.elapsed() > Duration::from_secs(4)) {
            l.dismissed = true;
            return None;
        }
        l.welcome_seen = Some(Instant::now());
        Some(name.clone())
    })
}

fn dismiss_welcome() {
    login_state(|login| {
        if let Some(l) = login.as_mut() {
            l.dismissed = true;
        }
    });
}

// ---------------------------------------------------------------------------------------------
// What the menus read every second, kept cheap.

/// The account's name, asked of GitHub once per connection (a menu is built every second).
static ACCOUNT: Mutex<Option<(String, Option<String>)>> = Mutex::new(None);

fn forget_login_cache() {
    *ACCOUNT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The login of the remembered connection: the service's word, else GitHub's (asked in the
/// background, `None` until it answers).
fn login_name(token: &str) -> Option<String> {
    if let Some(state) = service::running().filter(|s| !s.login.is_empty()) {
        return Some(state.login);
    }
    let mut cache = ACCOUNT.lock().unwrap_or_else(|e| e.into_inner());
    match cache.as_ref() {
        Some((t, name)) if t == token => name.clone(),
        _ => {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else { return None };
            *cache = Some((token.to_owned(), None));
            let token = token.to_owned();
            runtime.spawn(async move {
                let gh = po_link::github::GitHub::new(&service::github_config(), token.clone());
                if let Ok(Ok(user)) = tokio::time::timeout(Duration::from_secs(8), gh.user()).await {
                    let mut cache = ACCOUNT.lock().unwrap_or_else(|e| e.into_inner());
                    if cache.as_ref().is_some_and(|(t, _)| *t == token) {
                        *cache = Some((token, Some(user.login)));
                    }
                }
            });
            None
        }
    }
}

static INSTALLED: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// Is the service registered at logon? Asking Windows takes a moment: asked again every 5 s.
fn installed() -> bool {
    let mut cache = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    match *cache {
        Some((at, value)) if at.elapsed() < Duration::from_secs(5) => value,
        _ => {
            let value = service::installed();
            *cache = Some((Instant::now(), value));
            value
        }
    }
}

/// Just changed: the next read asks Windows again.
fn forget_installed() {
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn kind_label(kind: &str) -> String {
    match kind {
        "phone" => t!("rc.kind.phone").to_owned(),
        "pc" => t!("rc.kind.pc").to_owned(),
        other => other.to_owned(),
    }
}

/// "mm:ss" left.
fn left(d: Duration) -> String {
    format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60)
}

/// Opens a page in the browser of this PC.
fn open_page(url: &str) -> Result<(), String> {
    // Only addresses Po made (GitHub's page, the app's), never anything a shell would read.
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || "\"'&|<>^%`$;".contains(c)) {
        return Err(t!("rc.open_failed").to_owned());
    }
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().map(|_| ()).map_err(|_| t!("rc.open_failed").to_owned())
}

// ---------------------------------------------------------------------------------------------
// Commands: the line mode, the phone, and what the menus' items run.

/// `/rc …`.
pub async fn command(argument: Option<&str>, rest: &[&str]) -> Reply {
    let mut reply = Reply::default();
    match argument {
        None | Some("etat" | "état" | "status") => {
            // Nobody connected: `/rc` is the connection itself.
            if argument.is_none() && service::token().is_none() {
                login(&mut reply).await;
            } else {
                status(&mut reply).await;
            }
        }
        Some("menu") => status(&mut reply).await,
        Some("connecter" | "connexion" | "login" | "connect") => match account().await {
            Some(Ok(user)) if user.can_gist() => {
                reply.plain(t!("rc.already", login = user.login));
                reply.dim(t!("rc.switch_hint"));
            }
            _ => login(&mut reply).await,
        },
        Some("nouveau-code" | "new-code") => login(&mut reply).await,
        Some("changer" | "switch") => {
            crate::github::forget_token();
            forget_login_cache();
            reply.dim(t!("rc.switch.other_account"));
            login(&mut reply).await;
        }
        Some("deconnecter" | "déconnecter" | "logout" | "disconnect") => {
            crate::github::forget_token();
            forget_login_cache();
            login_state(|l| *l = None);
            reply.plain(t!("rc.logged_out"));
            reply.dim(t!("rc.logged_out_hint"));
        }
        Some("ouvrir" | "open") => {
            let url = pending_login().map(|(url, ..)| url).unwrap_or_else(|| "https://github.com/login/device".to_owned());
            match open_page(&url) {
                Ok(()) => reply.dim(t!("rc.opened", url = url)),
                Err(e) => reply.warn(e),
            }
        }
        Some("telecharger" | "télécharger" | "download") => match open_page(&app_link()) {
            Ok(()) => reply.dim(t!("rc.opened", url = app_link())),
            Err(e) => reply.warn(e),
        },
        Some("mobile" | "app") => mobile_steps(&mut reply),
        Some("nom" | "name") => {
            let name = rest.join(" ");
            let mut settings = service::Settings::load();
            if name.trim().is_empty() {
                reply.field(t!("rc.name"), settings.name());
                reply.dim(t!("rc.name_hint"));
            } else {
                settings.name = Some(name.trim().chars().take(64).collect());
                match settings.save() {
                    Ok(()) => {
                        let _ = service::ask("rename", "");
                        reply.plain(t!("rc.renamed", name = settings.name()));
                    }
                    Err(e) => reply.warn(e.to_string()),
                }
            }
        }
        Some("appareils" | "devices") => devices(&mut reply),
        Some("revoquer" | "révoquer" | "revoke") => match rest.first() {
            None => {
                reply.dim(t!("rc.revoke_hint"));
                devices(&mut reply);
            }
            Some(which) => revoke(which, &mut reply).await,
        },
        Some("demarrer" | "démarrer" | "start") => match service::running() {
            Some(_) => reply.plain(t!("service.already")),
            None => match service::start() {
                Ok(()) => reply.plain(t!("service.started")),
                Err(e) => reply.warn(e),
            },
        },
        Some("arreter" | "arrêter" | "stop") => match service::stop().await {
            Ok(()) => reply.plain(t!("service.stopped")),
            Err(e) => reply.warn(e),
        },
        Some("installer" | "install") => {
            match service::install() {
                Ok(how) => {
                    reply.plain(t!("service.installed", how = how));
                    if service::running().is_none() {
                        let _ = service::start();
                    }
                }
                Err(e) => reply.warn(e),
            }
            forget_installed();
        }
        Some("retirer" | "uninstall") => {
            service::uninstall();
            forget_installed();
            reply.plain(t!("rc.at_logon_removed"));
        }
        Some(other) => reply.warn(t!("rc.unknown", arg = other)),
    }
    reply
}

/// The account behind the remembered connection, if it still works.
async fn account() -> Option<Result<po_link::github::User, String>> {
    let token = service::token()?;
    let gh = po_link::github::GitHub::new(&service::github_config(), token);
    Some(match tokio::time::timeout(Duration::from_secs(8), gh.user()).await {
        Ok(Ok(user)) => Ok(user),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(t!("rc.github_slow").to_owned()),
    })
}

/// Starts a connection; says where to go and what to type (the line mode's login screen).
async fn login(reply: &mut Reply) {
    match start_login().await {
        Ok(()) => {
            let (url, code, ..) = pending_login().unwrap_or_default();
            reply.section(t!("rc.login.title"));
            reply.field(t!("rc.login.page"), url);
            reply.field(t!("rc.login.code"), code);
            reply.dim(t!("rc.login.then"));
        }
        Err(e) => reply.warn(e),
    }
}

fn mobile_steps(reply: &mut Reply) {
    reply.section(t!("rc.mobile.title"));
    reply.field(t!("rc.mobile.download"), app_link());
    for line in steps(None) {
        reply.plain(line);
    }
}

/// What remains to do on the phone.
fn steps(login: Option<&str>) -> Vec<String> {
    let login = login.map(str::to_owned).unwrap_or_else(|| t!("rc.mobile.same_account").to_owned());
    vec![
        t!("rc.mobile.step1", link = app_link()),
        t!("rc.mobile.step2", login = login),
        t!("rc.mobile.step3", name = service::Settings::load().name()),
    ]
}

async fn status(reply: &mut Reply) {
    reply.section(t!("rc.title"));
    match account().await {
        None => {
            reply.field(t!("rc.account"), t!("rc.not_connected"));
            reply.dim(t!("rc.connect_hint"));
        }
        Some(Ok(user)) if !user.can_gist() => {
            reply.field(t!("rc.account"), &user.login);
            reply.warn(t!("rc.needs_gist"));
        }
        Some(Ok(user)) => reply.field(t!("rc.account"), &user.login),
        Some(Err(e)) => reply.field(t!("rc.account"), t!("rc.account_error", error = e)),
    }
    reply.field(t!("rc.name"), service::Settings::load().name());
    for line in service::status_lines() {
        reply.dim(line);
    }
}

fn devices(reply: &mut Reply) {
    match service::running() {
        Some(state) if state.devices.is_empty() => reply.dim(t!("rc.no_devices")),
        Some(state) => {
            reply.section(t!("rc.devices"));
            for (id, name, kind, seen) in &state.devices {
                reply.item(format!("{name} ({})", kind_label(kind)), t!("rc.device_detail", id = id, ago = service::ago(*seen)));
            }
        }
        None => reply.dim(t!("rc.devices_need_service")),
    }
}

async fn revoke(which: &str, reply: &mut Reply) {
    let Some(state) = service::running() else {
        reply.warn(t!("rc.devices_need_service"));
        return;
    };
    // By id, or by name when it is unambiguous.
    let matches: Vec<_> = state.devices.iter().filter(|(id, name, ..)| id == which || name.eq_ignore_ascii_case(which)).collect();
    match matches.as_slice() {
        [(id, name, ..)] => match service::ask("revoke", id) {
            Ok(()) => reply.plain(t!("rc.revoked", name = name)),
            Err(e) => reply.warn(e.to_string()),
        },
        [] => reply.warn(t!("rc.no_such_device", which = which)),
        _ => reply.warn(t!("rc.ambiguous", which = which)),
    }
}

// ---------------------------------------------------------------------------------------------
// The screens of the full-screen interface.

/// The menus of `/rc`.
pub async fn menu(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    let connected = service::token();
    match arg {
        None => match connected {
            None => Some(login_screen(cmd).await),
            Some(token) => Some(match just_connected() {
                Some(login) => welcome_screen(cmd, &login),
                None => main_screen(cmd, &token),
            }),
        },
        Some("menu") => {
            dismiss_welcome();
            Some(match connected {
                Some(token) => main_screen(cmd, &token),
                None => login_screen(cmd).await,
            })
        }
        Some("etat" | "état" | "status") => Some(status_screen(cmd, connected.as_deref())),
        Some("compte" | "account") => Some(account_screen(cmd, connected.as_deref())),
        Some("appareils" | "devices") => Some(devices_screen(cmd)),
        Some("appareil" | "device") => rest.first().and_then(|id| device_screen(cmd, id)),
        Some("revoquer" | "révoquer" | "revoke") if rest.is_empty() => Some(devices_screen(cmd)),
        Some("service") => Some(service_screen(cmd)),
        Some("mobile" | "app") => Some(mobile_screen(cmd)),
        _ => None,
    }
}

fn back_item(cmd: &str, to: &str) -> Item {
    Item::run(t!("rc.back"), format!("{cmd} {to}"))
}

/// Nobody connected: the connection starts at once, its code shown, and the screen says the
/// moment GitHub validates it.
async fn login_screen(cmd: &str) -> Menu {
    if pending_login().is_none() && let Err(e) = start_login().await {
        return Menu::new(t!("rc.login.title"), vec![Item::run(t!("rc.login.retry"), cmd.to_owned()), Item::close(t!("rc.close"))])
            .note(t!("rc.login.failed", error = e));
    }
    let Some((url, code, remaining, failure)) = pending_login() else {
        // Validated between the two reads: the next second shows the welcome.
        return Menu::new(t!("rc.login.title"), vec![Item::close(t!("rc.close"))]).note(t!("rc.login.validated")).live();
    };
    let mut note = vec![t!("rc.login.why").to_owned(), String::new(), t!("rc.login.step_page", url = url), t!("rc.login.step_code", code = code), String::new()];
    let items = match &failure {
        None => {
            note.push(format!("~ {}", t!("rc.login.waiting", left = left(remaining))));
            vec![
                Item::run(t!("rc.login.open"), format!("{cmd} {}", w("ouvrir", "open"))).then(cmd.to_owned()),
                Item::run(t!("rc.login.new_code"), format!("{cmd} {}", w("nouveau-code", "new-code"))).then(cmd.to_owned()),
                Item::close(t!("rc.login.later")),
            ]
        }
        Some(error) => {
            note.push(t!("rc.login.failed", error = error));
            vec![Item::run(t!("rc.login.new_code"), format!("{cmd} {}", w("nouveau-code", "new-code"))).then(cmd.to_owned()), Item::close(t!("rc.close"))]
        }
    };
    Menu::new(t!("rc.login.title"), items).note(note.join("\n")).live().busy(failure.is_none())
}

/// Just connected: what remains to do on the phone, the phone seen when it comes, and the
/// service offered at logon - a letter for yes or no, Enter to leave.
fn welcome_screen(cmd: &str, login: &str) -> Menu {
    welcome_with(cmd, login, installed())
}

fn welcome_with(cmd: &str, login: &str, at_logon: bool) -> Menu {
    let running = service::running();
    let mut note = vec![t!("rc.welcome.connected", login = login), String::new(), t!("rc.welcome.phone").to_owned()];
    note.extend(steps(Some(login)).into_iter().map(|s| format!("  {s}")));
    note.push(String::new());
    let phones: Vec<_> = running.as_ref().map(|s| s.devices.iter().filter(|d| d.2 == "phone").collect()).unwrap_or_default();
    let mut busy = true;
    match (&running, phones.first()) {
        (None, _) => note.push(format!("~ {}", t!("rc.welcome.service_starting"))),
        (Some(s), _) if s.state != "running" => note.push(format!("~ {}", t!("rc.welcome.service_connecting"))),
        (Some(_), None) => note.push(format!("~ {}", t!("rc.welcome.waiting_phone"))),
        (Some(_), Some((_, name, _, seen))) => {
            busy = false;
            note.push(t!("rc.welcome.phone_seen", name = name, ago = service::ago(*seen)));
        }
    }
    let mut items = vec![Item::close(t!("rc.welcome.quit"))];
    if at_logon {
        note.push(t!("rc.welcome.at_logon").to_owned());
        items.push(Item::run(t!("rc.welcome.menu"), format!("{cmd} menu")).key('m'));
    } else {
        note.push(String::new());
        note.push(t!("rc.welcome.offer_logon").to_owned());
        let yes = if po_i18n::lang() == po_i18n::Lang::Fr { 'o' } else { 'y' };
        items.push(Item::run(t!("rc.welcome.logon_yes"), format!("{cmd} {}", w("installer", "install"))).key(yes).then(cmd.to_owned()));
        items.push(Item::run(t!("rc.welcome.logon_no"), format!("{cmd} menu")).key('n'));
    }
    Menu::new(t!("rc.welcome.title"), items).note(note.join("\n")).live().busy(busy)
}

/// One line of the service's state, for the main screen.
fn service_summary() -> String {
    match service::running() {
        Some(s) if s.state == "running" => t!("rc.summary.running"),
        Some(s) if s.state == "signed_out" => t!("rc.summary.signed_out"),
        Some(_) => t!("rc.summary.connecting"),
        None => t!("rc.summary.stopped"),
    }
    .to_owned()
}

fn main_screen(cmd: &str, token: &str) -> Menu {
    let running = service::running();
    let login = login_name(token);
    let devices = running.as_ref().map_or(0, |s| s.devices.len());
    let at_logon = installed();
    let mut note = vec![t!("rc.main.connected", login = login.clone().unwrap_or_else(|| "…".into()))];
    match &running {
        Some(s) if s.state == "running" => note.push(t!("rc.main.service_on", n = devices)),
        Some(_) => note.push(format!("~ {}", t!("rc.welcome.service_connecting"))),
        None => note.push(t!("menu.rc.note_stopped").to_owned()),
    }
    if let Some(error) = running.as_ref().and_then(|s| s.error.clone()) {
        note.push(t!("service.status.error", error = error));
    }
    let busy = running.as_ref().is_some_and(|s| s.state != "running");
    let service_detail = format!("{}{}", service_summary(), if at_logon { t!("rc.summary.at_logon") } else { "" });
    let items = vec![
        Item::run(t!("rc.main.status"), format!("{cmd} {}", w("etat", "status"))),
        Item::run(t!("rc.main.devices"), format!("{cmd} {}", w("appareils", "devices"))).detail(t!("menu.count", n = devices)),
        Item::run(t!("rc.main.service"), format!("{cmd} service")).detail(service_detail),
        Item::run(t!("rc.main.account"), format!("{cmd} {}", w("compte", "account"))).detail(login.unwrap_or_default()),
        Item::fill(t!("menu.rc.name"), format!("{cmd} {} ", w("nom", "name"))).detail(service::Settings::load().name()),
        Item::run(t!("rc.main.mobile"), format!("{cmd} mobile")),
        Item::close(t!("rc.close")),
    ];
    Menu::new(t!("menu.rc"), items).note(note.join("\n")).live().busy(busy)
}

fn status_screen(cmd: &str, token: Option<&str>) -> Menu {
    let mut note = vec![t!("rc.status.account", login = token.and_then(login_name).unwrap_or_else(|| t!("rc.not_connected").to_owned()))];
    note.push(t!("rc.status.name", name = service::Settings::load().name()));
    note.extend(service::status_lines());
    Menu::new(t!("rc.main.status"), vec![back_item(cmd, "menu")]).note(note.join("\n")).live().back(format!("{cmd} menu"))
}

fn account_screen(cmd: &str, token: Option<&str>) -> Menu {
    let login = token.and_then(login_name);
    let note = match &login {
        Some(login) => t!("rc.account.as", login = login),
        None => t!("rc.account.checking").to_owned(),
    };
    let items = vec![
        Item::run(t!("menu.rc.switch"), format!("{cmd} {}", w("changer", "switch"))).then(cmd.to_owned()).detail(t!("rc.account.switch_hint")),
        Item::run(t!("menu.rc.logout"), format!("{cmd} {}", w("deconnecter", "logout"))).detail(t!("rc.account.logout_hint")),
        back_item(cmd, "menu"),
    ];
    Menu::new(t!("rc.main.account"), items).note(note).live().back(format!("{cmd} menu")).at(2)
}

fn devices_screen(cmd: &str) -> Menu {
    let back = format!("{cmd} menu");
    let Some(state) = service::running() else {
        let items = vec![
            Item::run(t!("menu.rc.start"), format!("{cmd} {}", w("demarrer", "start"))).then(format!("{cmd} {}", w("appareils", "devices"))),
            back_item(cmd, "menu"),
        ];
        return Menu::new(t!("rc.main.devices"), items).note(t!("rc.devices.service_off")).live().back(back);
    };
    let mut items: Vec<Item> = state
        .devices
        .iter()
        .map(|(id, name, kind, seen)| Item::run(format!("{name} ({})", kind_label(kind)), format!("{cmd} {} {id}", w("appareil", "device"))).detail(service::ago(*seen)))
        .collect();
    let note = if items.is_empty() { t!("rc.devices.none").to_owned() } else { t!("rc.devices.note").to_owned() };
    let empty = items.is_empty();
    items.push(Item::run(t!("rc.main.mobile"), format!("{cmd} mobile")));
    items.push(back_item(cmd, "menu"));
    let at = if empty { items.len() - 2 } else { 0 };
    Menu::new(t!("rc.main.devices"), items).note(note).live().back(back).busy(empty && state.state == "running").at(at)
}

fn device_screen(cmd: &str, id: &str) -> Option<Menu> {
    let devices = format!("{cmd} {}", w("appareils", "devices"));
    let state = service::running()?;
    let Some((id, name, kind, seen)) = state.devices.iter().find(|d| d.0 == id) else {
        // Revoked meanwhile: the list says so.
        return Some(devices_screen(cmd));
    };
    let note = [t!("rc.device.kind", kind = kind_label(kind)), t!("rc.device.seen", ago = service::ago(*seen)), t!("rc.device.id", id = id)].join("\n");
    let items = vec![
        Item::run(t!("rc.device.revoke"), format!("{cmd} {} {id}", w("revoquer", "revoke"))).then(devices.clone()).detail(t!("menu.rc.revoke_note")),
        Item::run(t!("rc.back"), devices.clone()),
    ];
    Some(Menu::new(name.clone(), items).note(note).live().back(devices).at(1))
}

fn service_screen(cmd: &str) -> Menu {
    let here = format!("{cmd} service");
    let running = service::running();
    let mut note = service::status_lines();
    note.retain(|l| !l.starts_with("  "));
    let busy = running.as_ref().is_some_and(|s| s.state != "running");
    if busy {
        note.insert(1, format!("~ {}", t!("rc.welcome.service_connecting")));
    }
    let mut items = vec![match &running {
        Some(_) => Item::run(t!("menu.rc.stop"), format!("{cmd} {}", w("arreter", "stop"))).then(here.clone()).detail(t!("rc.service.stop_hint")),
        None => Item::run(t!("menu.rc.start"), format!("{cmd} {}", w("demarrer", "start"))).then(here.clone()),
    }];
    items.push(if installed() {
        Item::run(t!("menu.rc.no_logon"), format!("{cmd} {}", w("retirer", "uninstall"))).then(here.clone())
    } else {
        Item::run(t!("menu.rc.logon"), format!("{cmd} {}", w("installer", "install"))).then(here.clone()).detail(t!("menu.rc.logon_hint"))
    });
    items.push(back_item(cmd, "menu"));
    let at = items.len() - 1;
    Menu::new(t!("rc.main.service"), items).note(note.join("\n")).live().back(format!("{cmd} menu")).busy(busy).at(at)
}

fn mobile_screen(cmd: &str) -> Menu {
    let mut note = vec![t!("rc.mobile.intro").to_owned(), String::new()];
    note.extend(steps(None));
    let items = vec![
        Item::run(t!("rc.mobile.open"), format!("{cmd} {}", w("telecharger", "download"))).then(format!("{cmd} mobile")).detail(app_link()),
        back_item(cmd, "menu"),
    ];
    Menu::new(t!("rc.mobile.title"), items).note(note.join("\n")).back(format!("{cmd} menu")).at(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .map(|i| match &i.action {
                po_tui::menu::Action::Run(c) | po_tui::menu::Action::Show(c) | po_tui::menu::Action::Fill(c) => c.clone(),
            })
            .collect()
    }

    #[test]
    fn the_rc_screens_go_back_where_they_came_from() {
        // Other tests change the language meanwhile: either word will do.
        let main = main_screen("/rc", "token");
        let all = commands(&main);
        for (fr, en) in [("etat", "status"), ("appareils", "devices"), ("service", "service"), ("compte", "account"), ("nom ", "name "), ("mobile", "mobile")] {
            assert!(all.iter().any(|c| *c == format!("/rc {fr}") || *c == format!("/rc {en}")), "{en} in {all:?}");
        }
        assert!(main.live && main.back.is_none(), "the top screen is live and Esc closes it");
        assert!(main.items.last().unwrap().closes());
        for screen in [status_screen("/rc", None), account_screen("/rc", None), devices_screen("/rc"), service_screen("/rc"), mobile_screen("/rc")] {
            assert_eq!(screen.back.as_deref(), Some("/rc menu"), "{}", screen.title);
            assert!(commands(&screen).iter().any(|c| c == "/rc menu"), "a Back item in {}", screen.title);
        }
        // An action comes back to its screen, to show what changed.
        let service = service_screen("/rc");
        for (item, command) in service.items.iter().zip(commands(&service)) {
            if command != "/rc menu" {
                // The words of either language.
                assert_eq!(item.then.as_deref(), Some("/rc service"), "{command}");
            }
        }
    }

    #[test]
    fn the_welcome_offers_logon_with_letters_and_enter_leaves() {
        let welcome = welcome_with("/rc", "octocat", false);
        let note = welcome.note.clone().unwrap();
        assert!(note.contains("octocat") && note.contains(&app_link()), "{note}");
        assert!(welcome.current().unwrap().closes(), "Enter leaves");
        assert!(!welcome.current().unwrap().current, "nothing marked as in force");
        let yes = welcome.hotkey('o').or(welcome.hotkey('y')).expect("a letter for yes");
        let yes = &welcome.items[yes];
        assert_eq!(yes.then.as_deref(), Some("/rc"), "the welcome shows the service at logon after");
        let no = &welcome.items[welcome.hotkey('n').expect("a letter for no")];
        assert!(matches!(&no.action, po_tui::menu::Action::Run(c) if c == "/rc menu"));
        // Already at logon: nothing to offer, the menu a letter away.
        let done = welcome_with("/rc", "octocat", true);
        assert!(done.hotkey('n').is_none() && done.hotkey('m').is_some());
    }

    #[test]
    fn the_welcome_gives_way_to_the_menu_once_closed() {
        login_state(|l| {
            *l = Some(Login {
                id: 0,
                url: String::new(),
                code: String::new(),
                deadline: Instant::now(),
                outcome: Some(Ok("octocat".into())),
                welcome_seen: Some(Instant::now() - Duration::from_secs(10)),
                dismissed: false,
            })
        });
        assert!(just_connected().is_none(), "closed for a while: the menu again");
        login_state(|l| *l = None);
        assert!(pending_login().is_none() && just_connected().is_none());
    }

    #[test]
    fn only_pages_po_made_are_opened() {
        assert!(open_page("calc.exe").is_err());
        assert!(open_page("https://x\" & calc").is_err());
        assert!(open_page("https://github.com/a b").is_err());
    }
}
