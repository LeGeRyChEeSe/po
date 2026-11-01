//! `/rc`: the phone's remote control, from inside Po — the GitHub account it pairs through, the
//! PC's name, the devices, and the background service that keeps the link.
//!
//! Pairing a phone is signing in with the same GitHub account in Po and in the app: there is no
//! code to copy. The connection is remembered for every session (the one Po Labs uses to share
//! contributions too), and can be switched or dropped here at any time.

use po_i18n::t;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::service;

/// The word of the interface's language: every subcommand exists in both.
fn w(fr: &'static str, en: &'static str) -> &'static str {
    if po_i18n::lang() == po_i18n::Lang::Fr { fr } else { en }
}

/// The account behind the remembered connection, if it still works.
async fn account() -> Option<Result<po_link::github::User, String>> {
    let token = service::token()?;
    let gh = po_link::github::GitHub::new(&service::github_config(), token);
    Some(match tokio::time::timeout(std::time::Duration::from_secs(8), gh.user()).await {
        Ok(Ok(user)) => Ok(user),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(t!("rc.github_slow").to_owned()),
    })
}

/// Starts a sign-in in the background; says where to go and what to type.
async fn login(reply: &mut Reply) {
    match crate::github::login_in_background(&crate::github::Config::from_env()).await {
        Ok((url, code)) => {
            reply.section(t!("rc.login.title"));
            reply.field(t!("rc.login.page"), url);
            reply.field(t!("rc.login.code"), code);
            reply.dim(t!("rc.login.then"));
            // The service waits for the connection and starts the link with it.
            if service::running().is_none()
                && let Err(e) = service::start()
            {
                reply.warn(e);
            }
        }
        Err(e) => reply.warn(e),
    }
}

/// `/rc …`.
pub async fn command(argument: Option<&str>, rest: &[&str]) -> Reply {
    let mut reply = Reply::default();
    match argument {
        None | Some("etat" | "état" | "status") => status(&mut reply).await,
        Some("connecter" | "connexion" | "login" | "connect") => match account().await {
            Some(Ok(user)) if user.can_gist() => {
                reply.plain(t!("rc.already", login = user.login));
                reply.dim(t!("rc.switch_hint"));
            }
            _ => login(&mut reply).await,
        },
        Some("changer" | "switch") => {
            crate::github::forget_token();
            reply.dim(t!("rc.switch.other_account"));
            login(&mut reply).await;
        }
        Some("deconnecter" | "déconnecter" | "logout" | "disconnect") => {
            crate::github::forget_token();
            reply.plain(t!("rc.logged_out"));
            reply.dim(t!("rc.logged_out_hint"));
        }
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
        Some("installer" | "install") => match service::install() {
            Ok(how) => {
                reply.plain(t!("service.installed", how = how));
                if service::running().is_none() {
                    let _ = service::start();
                }
            }
            Err(e) => reply.warn(e),
        },
        Some("retirer" | "uninstall") => {
            service::uninstall();
            reply.plain(t!("rc.at_logon_removed"));
        }
        Some(other) => reply.warn(t!("rc.unknown", arg = other)),
    }
    reply
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
                reply.item(format!("{name} ({kind})"), t!("rc.device_detail", id = id, ago = service::ago(*seen)));
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

/// The menus of `/rc`.
pub fn menu(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    match arg {
        None => {
            let connected = service::token().is_some();
            let running = service::running();
            let mut items = vec![Item::show(t!("menu.rc.status"), format!("{cmd} {}", w("etat", "status")))];
            if connected {
                items.push(Item::run(t!("menu.rc.switch"), format!("{cmd} {}", w("changer", "switch"))).detail(running.as_ref().map(|s| s.login.clone()).unwrap_or_default()));
                items.push(Item::run(t!("menu.rc.logout"), format!("{cmd} {}", w("deconnecter", "logout"))));
            } else {
                items.push(Item::run(t!("menu.rc.login"), format!("{cmd} {}", w("connecter", "login"))).detail(t!("menu.rc.login_hint")));
            }
            items.push(Item::fill(t!("menu.rc.name"), format!("{cmd} {} ", w("nom", "name"))).detail(service::Settings::load().name()));
            items.push(Item::show(t!("menu.rc.devices"), format!("{cmd} {}", w("appareils", "devices"))));
            if running.as_ref().is_some_and(|s| !s.devices.is_empty()) {
                items.push(Item::run(t!("menu.rc.revoke"), format!("{cmd} {}", w("revoquer", "revoke"))));
            }
            items.push(match &running {
                Some(_) => Item::run(t!("menu.rc.stop"), format!("{cmd} {}", w("arreter", "stop"))),
                None => Item::run(t!("menu.rc.start"), format!("{cmd} {}", w("demarrer", "start"))),
            });
            items.push(if service::installed() {
                Item::run(t!("menu.rc.no_logon"), format!("{cmd} {}", w("retirer", "uninstall")))
            } else {
                Item::run(t!("menu.rc.logon"), format!("{cmd} {}", w("installer", "install"))).detail(t!("menu.rc.logon_hint"))
            });
            let note = match &running {
                Some(s) if s.state == "running" => t!("menu.rc.note_running", login = s.login),
                Some(_) => t!("menu.rc.note_waiting").to_owned(),
                None => t!("menu.rc.note_stopped").to_owned(),
            };
            Some(Menu::new(t!("menu.rc"), items).note(note))
        }
        Some(sub @ ("revoquer" | "révoquer" | "revoke")) if rest.is_empty() => {
            let state = service::running()?;
            let items = state
                .devices
                .iter()
                .map(|(id, name, kind, seen)| Item::run(format!("{name} ({kind})"), format!("{cmd} {sub} {id}")).detail(service::ago(*seen)))
                .collect::<Vec<_>>();
            (!items.is_empty()).then(|| Menu::new(t!("menu.rc.revoke"), items).note(t!("menu.rc.revoke_note")))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rc_menu_offers_the_account_the_name_and_the_service() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let top = menu("/rc", None, &[]).unwrap();
        let commands: Vec<String> = top
            .items
            .iter()
            .map(|i| match &i.action {
                po_tui::menu::Action::Run(c) | po_tui::menu::Action::Show(c) | po_tui::menu::Action::Fill(c) => c.clone(),
            })
            .collect();
        assert!(commands.iter().any(|c| c == "/rc status"));
        assert!(commands.iter().any(|c| c == "/rc name "));
        assert!(commands.iter().any(|c| c == "/rc login" || c == "/rc switch"));
        assert!(commands.iter().any(|c| c == "/rc start" || c == "/rc stop"));
        assert!(menu("/rc", Some("name"), &["x"]).is_none(), "a full command opens no menu");
    }
}
