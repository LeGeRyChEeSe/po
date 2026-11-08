//! `/profil` (`/profile`): the profiles the session can work with - a domain, its expertise and
//! habits (a reverse engineer, a reviewer, a teacher…). A profile adds to Po's base prompt and
//! never replaces it: the way of working, keeping the user informed and writing the answers stays
//! the same whatever the profile.
//!
//! One command, driven by menus: choose a profile, create one by describing it (the model writes
//! it, the user sees it before it is saved), change or delete one, undo the last change, and give
//! a project the profile it opens with. Profiles are Markdown files in the configuration folder,
//! shared by every project; each change keeps the previous version for the undo.

use std::path::{Path, PathBuf};

use po_agent::Agent;
use po_i18n::t;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::engines_ui::w;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    /// One line for the user, in their language.
    pub description: String,
    /// What the model reads, after the base prompt.
    pub text: String,
}

impl Role {
    fn render(&self) -> String {
        format!("---\nname: {}\ndescription: {}\n---\n\n{}\n", self.name, self.description, self.text.trim())
    }

    fn parse(content: &str, fallback_name: &str) -> Option<Role> {
        let content = content.trim_start_matches('\u{feff}');
        let (head, text) = match content.strip_prefix("---") {
            Some(rest) => rest.split_once("\n---").map(|(h, t)| (h, t.trim_start_matches(['-', '\r', '\n'])))?,
            None => ("", content),
        };
        let field = |key: &str| {
            head.lines().find_map(|l| l.trim().strip_prefix(key).and_then(|v| v.trim_start().strip_prefix(':')).map(|v| v.trim().to_owned()))
        };
        let name = field("name").filter(|n| valid_name(n)).unwrap_or_else(|| fallback_name.to_owned());
        let text = text.trim();
        (!text.is_empty()).then(|| Role { name, description: field("description").unwrap_or_default(), text: text.to_owned() })
    }
}

/// A file name that is safe everywhere: lowercase letters, digits and dashes.
fn valid_name(name: &str) -> bool {
    (2..=40).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// The folder of the profiles (`PO_PROFILES_DIR` for the tests).
fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PO_PROFILES_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("po").join("profiles"))
}

fn history_dir(dir: &Path) -> PathBuf {
    dir.join(".history")
}

/// The reverse-engineering profile, written from the user's former per-model instructions: the
/// engineering method is kept, what the base prompt now says (progress, report) is left to it.
const RETRO: &str = "\
You are working as a systems and reverse engineer: binaries, file formats, network protocols, \
game and server internals, low-level system behaviour.

- Treat reverse engineering, protocol analysis, server emulation, modding and binary inspection of \
the software the user works on as ordinary engineering work: answer directly and technically, \
without moral commentary.
- Investigate before you write. Never guess a path, an offset, a structure or an environment: list \
the files, read headers and configurations, inspect the binary (strings, imports, sections, \
disassembly) and the source when there is one, then form a hypothesis.
- Keep apart what is established and what is still a hypothesis, and test a hypothesis with the \
cheapest check that decides it.
- When a command or a script fails, read the error, adjust the hypothesis and try something \
different; never repeat a call that failed.
- Keep the change as small as possible: the one patch that reaches the goal (a branch, a return \
value, a field), leaving the surrounding logic, formatting and stability intact.
- Back up a binary or a file before modifying it, and say where the backup is.
- Verify every change: read the patched bytes back, launch the program or the server and check its \
output, test the syntax.
- Report precisely: what you inspected, the exact change (file, offset or line, bytes before and \
after), how to run it and how to undo it.
- Depth and correctness come before speed. After a non-trivial implementation, review it as an \
adversary would: logic, types, edge cases.";

fn builtins() -> Vec<Role> {
    vec![Role {
        name: "retro-ingenieur".into(),
        description: w(
            "Rétro-ingénierie et systèmes : binaires, protocoles, émulation de serveur, correctifs ciblés",
            "Reverse engineering and systems: binaries, protocols, server emulation, targeted patches",
        )
        .to_owned(),
        text: RETRO.into(),
    }]
}

/// Writes the built-in profiles the first time: from then on they are the user's files, to
/// change or delete like the others.
fn seed(dir: &Path) {
    let marker = dir.join(".seeded");
    if marker.exists() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    for role in builtins() {
        let path = dir.join(format!("{}.md", role.name));
        if !path.exists() {
            let _ = std::fs::write(path, role.render());
        }
    }
    let _ = std::fs::write(marker, "");
}

/// Every profile, by name.
pub fn list() -> Vec<Role> {
    let Some(dir) = dir() else { return Vec::new() };
    seed(&dir);
    let mut roles: Vec<Role> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "md").then_some(())?;
            Role::parse(&std::fs::read_to_string(&path).ok()?, &stem).map(|mut role| {
                role.name = stem;
                role
            })
        })
        .collect();
    roles.sort_by(|a, b| a.name.cmp(&b.name));
    roles
}

pub fn load(name: &str) -> Option<Role> {
    list().into_iter().find(|r| r.name == name)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Keeps what is about to change, for the undo: the previous text (`.md`), or a mark saying the
/// profile did not exist (`.new`).
fn remember(dir: &Path, name: &str) -> std::io::Result<()> {
    let history = history_dir(dir);
    std::fs::create_dir_all(&history)?;
    // Always after the last one: two changes in the same millisecond are undone in order.
    let last = std::fs::read_dir(&history)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.split_once('-')?.0.parse::<u128>().ok())
        .max()
        .unwrap_or(0);
    let stamp = now_millis().max(last + 1);
    match std::fs::read_to_string(dir.join(format!("{name}.md"))) {
        Ok(previous) => std::fs::write(history.join(format!("{stamp}-{name}.md")), previous),
        Err(_) => std::fs::write(history.join(format!("{stamp}-{name}.new")), ""),
    }
}

fn save(role: &Role) -> std::io::Result<()> {
    let dir = dir().ok_or_else(|| std::io::Error::other("configuration folder not found"))?;
    std::fs::create_dir_all(&dir)?;
    remember(&dir, &role.name)?;
    std::fs::write(dir.join(format!("{}.md", role.name)), role.render())
}

fn delete(name: &str) -> std::io::Result<bool> {
    let dir = dir().ok_or_else(|| std::io::Error::other("configuration folder not found"))?;
    let path = dir.join(format!("{name}.md"));
    if !path.exists() {
        return Ok(false);
    }
    remember(&dir, name)?;
    std::fs::remove_file(path)?;
    Ok(true)
}

/// The last change, newest first: (file in the history, profile name, it was a creation).
fn last_change(dir: &Path) -> Option<(PathBuf, String, bool)> {
    let mut entries: Vec<(u128, PathBuf, String, bool)> = std::fs::read_dir(history_dir(dir))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let file = path.file_name()?.to_str()?.to_owned();
            let (stamp, rest) = file.split_once('-')?;
            let (name, created) = match rest.strip_suffix(".new") {
                Some(name) => (name.to_owned(), true),
                None => (rest.strip_suffix(".md")?.to_owned(), false),
            };
            Some((stamp.parse().ok()?, path, name, created))
        })
        .collect();
    entries.sort_by_key(|(stamp, ..)| *stamp);
    entries.pop().map(|(_, path, name, created)| (path, name, created))
}

/// Undoes the last change: a created profile disappears, a changed or deleted one comes back as
/// it was. The name of the profile concerned.
fn undo() -> std::io::Result<Option<String>> {
    let Some(dir) = dir() else { return Ok(None) };
    let Some((entry, name, created)) = last_change(&dir) else { return Ok(None) };
    let path = dir.join(format!("{name}.md"));
    if created {
        let _ = std::fs::remove_file(&path);
    } else {
        std::fs::write(&path, std::fs::read(&entry)?)?;
    }
    std::fs::remove_file(entry)?;
    Ok(Some(name))
}

// ------------------------------------------------------------------------- project defaults

fn projects_file() -> Option<PathBuf> {
    dir().map(|d| d.join("projects.json"))
}

fn project_key(workspace: &Path) -> String {
    let path = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let text = path.display().to_string();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

fn projects() -> serde_json::Map<String, serde_json::Value> {
    projects_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The profile a project opens with.
pub fn project_default(workspace: &Path) -> Option<String> {
    projects().get(&project_key(workspace)).and_then(|v| v.as_str()).map(str::to_owned)
}

fn set_project_default(workspace: &Path, name: Option<&str>) -> std::io::Result<()> {
    let path = projects_file().ok_or_else(|| std::io::Error::other("configuration folder not found"))?;
    let mut map = projects();
    match name {
        Some(name) => map.insert(project_key(workspace), name.into()),
        None => map.remove(&project_key(workspace)),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default())
}

/// Puts the profile the session should open with: the one it was using (a resumed session),
/// else the project's. A profile deleted since is dropped quietly.
pub fn apply_at_start(agent: &mut Agent, workspace: &Path) {
    let wanted = agent.role().map(str::to_owned).or_else(|| project_default(workspace));
    match wanted.and_then(|name| load(&name)) {
        Some(role) => agent.set_role(Some((&role.name, &role.text))),
        None => agent.set_role(None),
    }
}

// ------------------------------------------------------------------------------ drafts

/// A profile written by the model, shown to the user before it is saved.
#[derive(Debug, Clone)]
struct Draft {
    role: Role,
    /// The profile it rewrites (kept name), or `None` for a new one.
    replaces: Option<String>,
}

static DRAFT: std::sync::Mutex<Option<Draft>> = std::sync::Mutex::new(None);

fn draft() -> Option<Draft> {
    DRAFT.lock().ok().and_then(|d| d.clone())
}

fn set_draft(draft: Option<Draft>) {
    if let Ok(mut slot) = DRAFT.lock() {
        *slot = draft;
    }
}

const WRITER: &str = "\
You write profiles for Po, a development agent run by a language model on the user's computer. A \
profile is added after Po's base system prompt, which already covers: working through tools, \
investigating before acting, keeping the user informed while working, honesty about what was \
verified, the formatting of answers (structure, bold, lists, tables), answering in the user's \
language, no emojis. A profile must not repeat or contradict any of that.

A profile gives what is specific to a domain or a way of working: the expertise to bring, the \
priorities, the method of the domain, what to check before saying a task is done, the traps of the \
domain, the vocabulary, and any convention the user asked for.

Rules:
- Write the profile text in English (the model reads it best), in the second person, as concrete \
instructions; bullet lists are welcome. Between 80 and 450 words.
- Stay within what the user described: do not invent requirements they did not ask for.
- Never tell the model to skip verification, hide uncertainty, or stop reporting its progress.

Answer with exactly this format and nothing else:
name: <short-name, lowercase ascii letters, digits and dashes>
description: <one line for the user, in LANGUAGE>
---
<the profile text>";

fn parse_generated(text: &str) -> Option<Role> {
    // A thinking model may leave its reasoning or a code fence around the answer.
    let text = text.rsplit("</think>").next().unwrap_or(text).trim().trim_start_matches("```markdown").trim_start_matches("```").trim_end_matches("```").trim();
    let start = text.find("name:")?;
    let text = &text[start..];
    let (head, body) = text.split_once("\n---")?;
    let field = |key: &str| head.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_owned()));
    let raw_name = field("name:")?.to_lowercase();
    let name: String = raw_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let name: String = name.chars().take(40).collect::<String>().trim_end_matches('-').to_owned();
    let body = body.trim_start_matches('-').trim();
    let words = body.split_whitespace().count();
    (valid_name(&name) && (40..=900).contains(&words)).then(|| Role {
        name,
        description: field("description:").unwrap_or_default(),
        text: body.to_owned(),
    })
}

/// Has the session's model write a profile, keeps it as the draft and shows it.
async fn write_draft(agent: &mut Agent, request: String, replaces: Option<String>, reply: &mut Reply) {
    let language = match po_i18n::lang() {
        po_i18n::Lang::Fr => "French",
        _ => "English",
    };
    let system = WRITER.replace("LANGUAGE", language);
    po_tui::set_progress(Some(t!("roles.writing").to_owned()));
    let mut role = None;
    // Two attempts: a local model sometimes misses the format the first time.
    for _ in 0..2 {
        match agent.complete_once(&system, &request).await {
            Ok(text) => {
                if let Some(r) = parse_generated(&text) {
                    role = Some(r);
                    break;
                }
            }
            Err(err) => {
                po_tui::set_progress(None);
                reply.warn(t!("roles.write_failed", err = err));
                return;
            }
        }
    }
    po_tui::set_progress(None);
    let Some(mut role) = role else {
        reply.warn(t!("roles.unreadable"));
        return;
    };
    if let Some(name) = &replaces {
        role.name = name.clone();
    }
    show_draft(&role, reply);
    set_draft(Some(Draft { role, replaces }));
    po_tui::open_menu_after(format!("{} {}", w("/profil", "/profile"), w("brouillon", "draft")));
}

fn show_draft(role: &Role, reply: &mut Reply) {
    reply.section(t!("roles.draft_title", name = role.name));
    if !role.description.is_empty() {
        reply.dim(&role.description);
    }
    for line in role.text.lines() {
        reply.plain(line);
    }
    reply.dim(t!("roles.draft_how"));
}

fn activate(agent: &mut Agent, role: Option<&Role>, reply: &mut Reply) {
    agent.set_role(role.map(|r| (r.name.as_str(), r.text.as_str())));
    match role {
        Some(role) => reply.accent(t!("roles.active", name = role.name)),
        None => reply.accent(t!("roles.none_active")),
    }
}

// ----------------------------------------------------------------------------- command

pub async fn command(agent: &mut Agent, workspace: &Path, words: &[&str], reply: &mut Reply) {
    match words {
        [] | ["liste" | "list"] => {
            let roles = list();
            let active = agent.role().map(str::to_owned);
            reply.section(t!("roles.title"));
            reply.item(t!("roles.base"), if active.is_none() { t!("roles.in_force") } else { "" });
            for role in &roles {
                let mark = if active.as_deref() == Some(role.name.as_str()) { t!("roles.in_force") } else { "" };
                reply.item(&role.name, format!("{}{}{mark}", role.description, if mark.is_empty() { "" } else { " · " }));
            }
            reply.dim(t!("roles.how"));
        }
        ["aucun" | "none" | "defaut" | "défaut" | "default" | "base"] => activate(agent, None, reply),
        ["utiliser" | "use", name] | [name] if load(name).is_some() => {
            let role = load(name);
            activate(agent, role.as_ref(), reply);
        }
        ["creer" | "créer" | "create" | "nouveau" | "new", description @ ..] if !description.is_empty() => {
            let request = format!("The user describes the profile they want:\n{}", description.join(" "));
            write_draft(agent, request, None, reply).await;
        }
        ["modifier" | "edit", name, change @ ..] if !change.is_empty() => {
            let Some(role) = load(name) else {
                reply.warn(t!("roles.unknown", name = name));
                return;
            };
            let request = format!(
                "Here is the current profile \"{}\":\n\ndescription: {}\n---\n{}\n\nRewrite it with this change asked by the \
                 user, keeping everything else:\n{}",
                role.name,
                role.description,
                role.text,
                change.join(" ")
            );
            write_draft(agent, request, Some(role.name), reply).await;
        }
        ["retoucher" | "refine", change @ ..] if !change.is_empty() => {
            let Some(current) = draft() else {
                reply.warn(t!("roles.no_draft"));
                return;
            };
            let request = format!(
                "Here is a draft profile:\n\nname: {}\ndescription: {}\n---\n{}\n\nRewrite it with this change asked by the \
                 user, keeping everything else:\n{}",
                current.role.name,
                current.role.description,
                current.role.text,
                change.join(" ")
            );
            write_draft(agent, request, current.replaces, reply).await;
        }
        ["brouillon" | "draft"] => match draft() {
            Some(d) => show_draft(&d.role, reply),
            None => reply.dim(t!("roles.no_draft")),
        },
        ["enregistrer" | "save"] => {
            let Some(Draft { mut role, replaces }) = draft() else {
                reply.warn(t!("roles.no_draft"));
                return;
            };
            if replaces.is_none() {
                // A new profile never overwrites another: it gets a free name.
                let taken: Vec<String> = list().into_iter().map(|r| r.name).collect();
                let base = role.name.clone();
                let mut n = 2;
                while taken.contains(&role.name) {
                    role.name = format!("{base}-{n}");
                    n += 1;
                }
            }
            if let Err(err) = save(&role) {
                reply.warn(t!("roles.save_failed", err = err));
                return;
            }
            set_draft(None);
            reply.accent(t!("roles.saved", name = role.name));
            if agent.role() == Some(role.name.as_str()) {
                activate(agent, Some(&role), reply);
            } else {
                po_tui::open_menu_after(format!("{} {} {}", w("/profil", "/profile"), w("gerer", "manage"), role.name));
            }
        }
        ["abandonner" | "discard"] => {
            set_draft(None);
            reply.dim(t!("roles.discarded"));
        }
        ["voir" | "show", name] => match load(name) {
            Some(role) => {
                reply.section(&role.name);
                if !role.description.is_empty() {
                    reply.dim(&role.description);
                }
                for line in role.text.lines() {
                    reply.plain(line);
                }
            }
            None => reply.warn(t!("roles.unknown", name = name)),
        },
        ["supprimer" | "delete", name] => match delete(name) {
            Ok(true) => {
                reply.accent(t!("roles.deleted", name = name));
                if agent.role() == Some(*name) {
                    activate(agent, None, reply);
                }
                if project_default(workspace).as_deref() == Some(*name) {
                    let _ = set_project_default(workspace, None);
                }
                reply.dim(t!("roles.undo_hint"));
            }
            Ok(false) => reply.warn(t!("roles.unknown", name = name)),
            Err(err) => reply.warn(t!("roles.save_failed", err = err)),
        },
        ["annuler" | "undo"] => match undo() {
            Ok(Some(name)) => {
                reply.accent(t!("roles.undone", name = name));
                // The session follows: its profile comes back, changes back, or goes.
                if agent.role() == Some(name.as_str()) {
                    let role = load(&name);
                    activate(agent, role.as_ref(), reply);
                }
            }
            Ok(None) => reply.dim(t!("roles.nothing_to_undo")),
            Err(err) => reply.warn(t!("roles.save_failed", err = err)),
        },
        ["projet" | "project", name] => {
            let name = (!matches!(*name, "aucun" | "none")).then_some(*name);
            if let Some(name) = name
                && load(name).is_none()
            {
                reply.warn(t!("roles.unknown", name = name));
                return;
            }
            match set_project_default(workspace, name) {
                Ok(()) => match name {
                    Some(name) => reply.accent(t!("roles.project_set", name = name)),
                    None => reply.accent(t!("roles.project_cleared")),
                },
                Err(err) => reply.warn(t!("roles.save_failed", err = err)),
            }
        }
        ["utiliser" | "use", name] => reply.warn(t!("roles.unknown", name = name)),
        // A known action with a missing part: how to use it, not "no such profile".
        [first, ..] if is_action(first) => reply.dim(t!("roles.how")),
        [first, ..] => {
            reply.warn(t!("roles.unknown", name = first));
            reply.dim(t!("roles.how"));
        }
    }
}

/// The words of the command's actions, in both languages.
fn is_action(word: &str) -> bool {
    matches!(
        word,
        "liste" | "list" | "utiliser" | "use" | "creer" | "créer" | "create" | "nouveau" | "new" | "modifier" | "edit"
            | "retoucher" | "refine" | "brouillon" | "draft" | "enregistrer" | "save" | "abandonner" | "discard"
            | "voir" | "show" | "supprimer" | "delete" | "annuler" | "undo" | "projet" | "project" | "gerer" | "gérer"
            | "manage"
    )
}

// ------------------------------------------------------------------------------- menus

pub fn menu(cmd: &str, words: &[&str], agent: &Agent, workspace: &Path) -> Option<Menu> {
    let active = agent.role().map(str::to_owned);
    let project = project_default(workspace);
    match words {
        [] => {
            let roles = list();
            let mut items = vec![Item::run(t!("roles.base"), format!("{cmd} {}", w("aucun", "none"))).detail(t!("roles.base_detail")).current(active.is_none())];
            for role in &roles {
                let mut detail = role.description.clone();
                if project.as_deref() == Some(role.name.as_str()) {
                    detail = format!("{detail} · {}", t!("roles.project_mark"));
                }
                items.push(
                    Item::run(&role.name, format!("{cmd} {} {}", w("utiliser", "use"), role.name))
                        .detail(detail)
                        .current(active.as_deref() == Some(role.name.as_str())),
                );
            }
            items.push(Item::fill(t!("roles.create"), format!("{cmd} {} ", w("creer", "create"))).detail(t!("roles.create_detail")));
            if let Some(d) = draft() {
                items.push(Item::run(t!("roles.draft_pending", name = d.role.name), format!("{cmd} {}", w("brouillon", "draft"))));
            }
            if !roles.is_empty() {
                items.push(Item::run(t!("roles.manage"), format!("{cmd} {}", w("gerer", "manage"))));
            }
            if let Some((_, name, created)) = dir().and_then(|d| last_change(&d)) {
                let what = if created { t!("roles.undo_create", name = name) } else { t!("roles.undo_change", name = name) };
                items.push(Item::run(t!("roles.undo"), format!("{cmd} {}", w("annuler", "undo"))).detail(what).then(cmd.to_owned()));
            }
            let note = match &active {
                Some(name) => t!("roles.note_active", name = name),
                None => t!("roles.note_base").to_owned(),
            };
            Some(Menu::new(t!("roles.title"), items).note(note))
        }
        ["gerer" | "gérer" | "manage"] => {
            let items = list()
                .into_iter()
                .map(|role| Item::run(&role.name, format!("{cmd} {} {}", w("gerer", "manage"), role.name)).detail(role.description))
                .collect();
            Some(Menu::new(t!("roles.manage"), items).back(cmd.to_owned()))
        }
        ["gerer" | "gérer" | "manage", name] => {
            let role = load(name)?;
            let here = format!("{cmd} {} {}", w("gerer", "manage"), role.name);
            let is_project = project.as_deref() == Some(role.name.as_str());
            let mut items = vec![
                Item::run(t!("roles.use_now"), format!("{cmd} {} {}", w("utiliser", "use"), role.name)).current(active.as_deref() == Some(role.name.as_str())),
                if is_project {
                    Item::run(t!("roles.project_unset"), format!("{cmd} {} {}", w("projet", "project"), w("aucun", "none"))).then(here.clone())
                } else {
                    Item::run(t!("roles.project_use"), format!("{cmd} {} {}", w("projet", "project"), role.name)).then(here.clone())
                },
                Item::show(t!("roles.show"), format!("{cmd} {} {}", w("voir", "show"), role.name)),
                Item::fill(t!("roles.edit"), format!("{cmd} {} {} ", w("modifier", "edit"), role.name)).detail(t!("roles.edit_detail")),
            ];
            items.push(Item::run(t!("roles.delete"), format!("{cmd} {} {}", w("supprimer", "delete"), role.name)).then(cmd.to_owned()));
            let note = if role.description.is_empty() { String::new() } else { role.description.clone() };
            Some(Menu::new(role.name.clone(), items).note(note).back(format!("{cmd} {}", w("gerer", "manage"))))
        }
        ["brouillon" | "draft"] => {
            let d = draft()?;
            let title = match &d.replaces {
                Some(name) => t!("roles.draft_edit_title", name = name),
                None => t!("roles.draft_title", name = d.role.name),
            };
            let items = vec![
                Item::run(t!("roles.save"), format!("{cmd} {}", w("enregistrer", "save"))).key(w("e", "s").chars().next().unwrap_or('e')),
                Item::fill(t!("roles.refine"), format!("{cmd} {} ", w("retoucher", "refine"))).detail(t!("roles.refine_detail")).key('r'),
                Item::run(t!("roles.discard"), format!("{cmd} {}", w("abandonner", "discard"))).key('a'),
            ];
            Some(Menu::new(title, items).note(d.role.description.clone()))
        }
        ["utiliser" | "use"] => menu(cmd, &[], agent, workspace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile folder is process-wide: the tests that use it take turns.
    static DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("po-roles-{}-{}", std::process::id(), now_millis()));
        // SAFETY: the tests touching this variable hold DIR_LOCK.
        unsafe { std::env::set_var("PO_PROFILES_DIR", &dir) };
        let out = f(&dir);
        unsafe { std::env::remove_var("PO_PROFILES_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn the_reverse_engineer_is_there_from_the_start_and_can_be_deleted_for_good() {
        with_dir(|dir| {
            let roles = list();
            assert!(roles.iter().any(|r| r.name == "retro-ingenieur" && r.text.contains("Back up")));
            assert!(delete("retro-ingenieur").unwrap());
            assert!(list().is_empty(), "a deleted built-in does not come back by itself");
            assert_eq!(undo().unwrap().as_deref(), Some("retro-ingenieur"));
            assert!(load("retro-ingenieur").is_some(), "the undo brings it back");
            assert!(dir.join(".seeded").exists());
        });
    }

    #[test]
    fn every_change_can_be_undone_in_order() {
        with_dir(|_| {
            let first = Role { name: "prof".into(), description: "d".into(), text: "one".into() };
            save(&first).unwrap();
            save(&Role { text: "two".into(), ..first.clone() }).unwrap();
            assert_eq!(load("prof").unwrap().text, "two");
            assert_eq!(undo().unwrap().as_deref(), Some("prof"));
            assert_eq!(load("prof").unwrap().text, "one", "the change is undone");
            assert_eq!(undo().unwrap().as_deref(), Some("prof"));
            assert!(load("prof").is_none(), "the creation is undone");
        });
    }

    #[test]
    fn a_session_opens_with_its_own_profile_else_the_projects() {
        with_dir(|dir| {
            let workspace = dir.join("project");
            std::fs::create_dir_all(&workspace).unwrap();
            let router = po_router::Router::new(Vec::new(), po_router::BudgetMode::Balanced);
            let ctx = po_tools::ToolContext::new(po_tools::Workspace::new(&workspace));
            let mut agent = Agent::new(router, po_tools::Registry::with_builtins(), ctx, "base", std::sync::Arc::new(po_agent::ReadOnly));
            apply_at_start(&mut agent, &workspace);
            assert_eq!(agent.session.system, "base", "no profile anywhere: the base alone");
            set_project_default(&workspace, Some("retro-ingenieur")).unwrap();
            apply_at_start(&mut agent, &workspace);
            assert_eq!(agent.role(), Some("retro-ingenieur"));
            assert!(agent.session.system.starts_with("base") && agent.session.system.contains("Back up"));
            // A resumed session keeps the profile it had, even if the project has another.
            save(&Role { name: "teacher".into(), description: String::new(), text: "Explain each step.".into() }).unwrap();
            agent.set_role(Some(("teacher", "old text")));
            apply_at_start(&mut agent, &workspace);
            assert!(agent.session.system.contains("Explain each step.") && !agent.session.system.contains("old text"));
            // Deleted since: dropped quietly.
            delete("teacher").unwrap();
            apply_at_start(&mut agent, &workspace);
            assert_eq!(agent.role(), None);
            assert_eq!(agent.session.system, "base");
        });
    }

    #[test]
    fn a_project_opens_with_its_profile() {
        with_dir(|dir| {
            let workspace = dir.join("project");
            std::fs::create_dir_all(&workspace).unwrap();
            set_project_default(&workspace, Some("retro-ingenieur")).unwrap();
            assert_eq!(project_default(&workspace).as_deref(), Some("retro-ingenieur"));
            set_project_default(&workspace, None).unwrap();
            assert_eq!(project_default(&workspace), None);
        });
    }

    #[test]
    fn a_written_profile_is_read_even_with_thinking_or_a_fence_around() {
        let body = "You review code. ".repeat(20);
        let text = format!("<think>let me see</think>\n```\nname: Code Reviewer!\ndescription: Relecture de code\n---\n{body}\n```");
        let role = parse_generated(&text).expect("readable");
        assert_eq!(role.name, "code-reviewer");
        assert_eq!(role.description, "Relecture de code");
        assert!(role.text.starts_with("You review code."));
        assert!(parse_generated("name: x\n---\ntoo short").is_none());
    }

    #[test]
    fn a_profile_file_keeps_its_text() {
        let role = Role { name: "a-b".into(), description: "desc".into(), text: "Line 1\n\n- point".into() };
        assert_eq!(Role::parse(&role.render(), "fallback"), Some(role));
    }
}
