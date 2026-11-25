//! The journal of frictions: what keeps snagging, for Po to notice and propose a repair.
//!
//! The agent hands each snag it meets (a tool failing, arguments that do not read, a reply cut by
//! the output limit...) to `record`, which appends one line to `%APPDATA%\po\friction.jsonl`. The
//! file never leaves the PC and holds no file contents: a kind, a key that makes two occurrences
//! the same snag, and a few words. From it Po finds what recurs, and does one of two things:
//!
//! - a closed catalog of deterministic adjustments ("speaking louder"): each is announced, written
//!   in `adjustments.json`, and undone with `/frictions undo <id>`;
//! - for the rest, proposals the user picks from (a note in the project, a Po Labs request,
//!   ignore). Nothing is applied without their choice, and Po never changes its own code outside
//!   Po Labs.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use po_agent::awareness::{Friction, FrictionKind};
use po_local::prefs::LocalPrefs;
use serde::{Deserialize, Serialize};

/// Lines kept in the journal: beyond, the oldest go.
pub const CAP: usize = 5000;
/// Characters kept of an entry's detail.
const DETAIL_MAX: usize = 200;
/// The size of the check on the journal's length, in appends: counting its lines at each one
/// would reread the file for nothing.
const CHECK_EVERY: usize = 100;
/// A friction recurs from this many occurrences...
const RECURRING_COUNT: usize = 3;
/// ...over at least this many sessions...
const RECURRING_SESSIONS: usize = 2;
/// ...within this window.
const WINDOW_SECS: u64 = 14 * 24 * 3600;
/// Ceiling of an automatic raise of a model's output: past it the snag is only signaled.
const MAX_OUTPUT_CEILING: u32 = 32_768;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `%APPDATA%\po` (`~/.config/po` elsewhere), read as the other preference files are.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .map(|d| d.join("po"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: u64,
    pub session: Option<String>,
    pub model: String,
    pub kind: String,
    pub key: String,
    pub detail: String,
}

impl Entry {
    pub fn of(friction: &Friction, ts: u64) -> Self {
        Self {
            ts,
            session: friction.session.clone(),
            model: friction.model.clone(),
            kind: friction.kind.as_str().to_owned(),
            key: truncate(&friction.key, 120),
            detail: truncate(&friction.detail, DETAIL_MAX),
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ------------------------------------------------------------------------------- the journal

/// The append-only file.
pub struct Journal {
    path: PathBuf,
    since_check: Mutex<usize>,
}

impl Journal {
    pub fn at(dir: &Path) -> Self {
        Self { path: dir.join("friction.jsonl"), since_check: Mutex::new(0) }
    }

    pub fn append(&self, entry: &Entry) {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(mut line) = serde_json::to_string(entry) else { return };
        line.push('\n');
        let written = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).and_then(|mut f| f.write_all(line.as_bytes()));
        if written.is_err() {
            return;
        }
        let check = {
            let mut since = self.since_check.lock().unwrap_or_else(|e| e.into_inner());
            *since += 1;
            let due = *since >= CHECK_EVERY;
            if due {
                *since = 0;
            }
            due
        };
        if check {
            self.trim();
        }
    }

    /// Keeps the newest [`CAP`] lines.
    pub fn trim(&self) {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return };
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= CAP {
            return;
        }
        let kept = lines[lines.len() - CAP..].join("\n") + "\n";
        let tmp = self.path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, kept).is_ok() && std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Every entry that reads; a damaged line is skipped, never an error.
    pub fn read(&self) -> Vec<Entry> {
        std::fs::read_to_string(&self.path)
            .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    }
}

fn journal() -> Option<&'static Journal> {
    static JOURNAL: OnceLock<Option<Journal>> = OnceLock::new();
    JOURNAL
        .get_or_init(|| {
            let journal = Journal::at(&dir()?);
            // The cap holds across sessions too: a process that records little never reaches
            // the periodic check.
            journal.trim();
            Some(journal)
        })
        .as_ref()
}

/// Writes a snag the agent met in the journal.
pub fn record(friction: &Friction) {
    if let Some(journal) = journal() {
        journal.append(&Entry::of(friction, now()));
    }
}

/// A snag only the host sees (a model that would not load), written the same way.
pub fn note(kind: FrictionKind, key: &str, detail: &str, model: &str, session: Option<String>) {
    record(&Friction { kind, key: key.to_owned(), detail: detail.to_owned(), model: model.to_owned(), session });
}

// ------------------------------------------------------------------------------- detection

/// What a user's own choice made (a refusal, a stop, an unanswered question) is not the harness
/// failing: journaled, never proposed for repair.
fn is_snag(kind: FrictionKind) -> bool {
    !matches!(kind, FrictionKind::Denied | FrictionKind::Interrupted | FrictionKind::QuestionSetAside)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurring {
    pub kind: FrictionKind,
    pub key: String,
    pub model: String,
    pub count: usize,
    pub sessions: usize,
    pub last_ts: u64,
    /// The detail of the last occurrence.
    pub detail: String,
}

impl Recurring {
    /// Stable name, for "seen" and "ignored".
    pub fn id(&self) -> String {
        format!("{}|{}|{}", self.kind.as_str(), self.key, self.model)
    }

    /// For a tool's snag, the tool and what it said.
    fn tool_and_signature(&self) -> (&str, &str) {
        self.key.split_once('|').unwrap_or((self.key.as_str(), ""))
    }
}

/// The frictions that recur: grouped by `(kind, key, model)`, at least [`RECURRING_COUNT`]
/// occurrences over at least [`RECURRING_SESSIONS`] sessions, since `since`. Most frequent first.
pub fn recurring_in(entries: &[Entry], since: u64) -> Vec<Recurring> {
    use std::collections::{BTreeMap, BTreeSet};
    /// Occurrences, sessions, last time, last detail.
    type Group = (usize, BTreeSet<String>, u64, String);
    let mut groups: BTreeMap<(String, String, String), Group> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.ts >= since) {
        let Some(kind) = FrictionKind::parse(&entry.kind).filter(|k| is_snag(*k)) else { continue };
        let group = groups.entry((kind.as_str().to_owned(), entry.key.clone(), entry.model.clone())).or_default();
        group.0 += 1;
        group.1.insert(entry.session.clone().unwrap_or_default());
        if entry.ts >= group.2 {
            group.2 = entry.ts;
            group.3 = entry.detail.clone();
        }
    }
    let mut out: Vec<Recurring> = groups
        .into_iter()
        .filter(|(_, (count, sessions, _, _))| *count >= RECURRING_COUNT && sessions.len() >= RECURRING_SESSIONS)
        .filter_map(|((kind, key, model), (count, sessions, last_ts, detail))| {
            Some(Recurring { kind: FrictionKind::parse(&kind)?, key, model, count, sessions: sessions.len(), last_ts, detail })
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_ts.cmp(&a.last_ts)).then(a.key.cmp(&b.key)));
    out
}

/// What the user chose to leave alone and what they were told of, kept next to the journal.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    pub announced: Vec<String>,
    pub ignored: Vec<String>,
}

impl Seen {
    pub fn load_from(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("friction-seen.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save_to(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("friction-seen.json"), serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

/// The line shown once at startup: how many recurring frictions Po noticed that it had not told
/// of yet. Marks them told, so it never comes twice for the same ones.
pub fn startup_line(dir: &Path, entries: &[Entry], now: u64) -> Option<String> {
    let mut seen = Seen::load_from(dir);
    let fresh: Vec<Recurring> = recurring_in(entries, now.saturating_sub(WINDOW_SECS))
        .into_iter()
        .filter(|r| !seen.ignored.contains(&r.id()) && !seen.announced.contains(&r.id()))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    seen.announced.extend(fresh.iter().map(Recurring::id));
    seen.save_to(dir);
    Some(po_i18n::t!("friction.startup", n = fresh.len()).to_string())
}

// ------------------------------------------------------------------------------- the model's side

/// What a recurring snag teaches the model, one line each: read at the turn start as part of
/// the state, so a model learns what keeps failing *for it* without being retrained.
pub fn hints_in(entries: &[Entry], model: &str, now: u64) -> Vec<String> {
    recurring_in(entries, now.saturating_sub(WINDOW_SECS))
        .into_iter()
        .filter(|r| r.model == model)
        .filter_map(|r| {
            let (tool, signature) = r.tool_and_signature();
            Some(match r.kind {
                FrictionKind::ToolError => {
                    let cure = if tool == "edit" && (signature.contains("not found") || signature.contains("match")) {
                        "read the file again before editing"
                    } else {
                        "find the cause before retrying"
                    };
                    format!("snag[{tool}]: {tool} often fails with \"{signature}\" for you: {cure}")
                }
                FrictionKind::InvalidArgs => {
                    format!("snag[{tool} args]: your arguments for {tool} often arrive invalid: keep them short, write big files in parts")
                }
                FrictionKind::OutputTruncated => "snag[output]: your replies often hit the output limit: keep each one short".to_owned(),
                _ => return None,
            })
        })
        .take(3)
        .collect()
}

/// [`hints_in`] over the real journal.
pub fn hints(model: &str) -> Vec<String> {
    let entries = dir().map(|d| Journal::at(&d).read()).unwrap_or_default();
    hints_in(&entries, model, now())
}

// ------------------------------------------------------------------------------- adjustments

/// One automatic change, kept so that it can be undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adjustment {
    pub id: String,
    pub ts: u64,
    pub model: String,
    /// What was changed: the closed catalog has one entry, `max_output`.
    pub setting: String,
    pub before: Option<u32>,
    pub after: u32,
    pub undone: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjustments {
    pub items: Vec<Adjustment>,
}

impl Adjustments {
    pub fn load_from(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("adjustments.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save_to(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("adjustments.json"), serde_json::to_string_pretty(self).unwrap_or_default() + "\n")
    }

    fn next_id(&self) -> String {
        let n = self.items.iter().filter_map(|a| a.id.strip_prefix('a').and_then(|n| n.parse::<u32>().ok())).max().unwrap_or(0);
        format!("a{}", n + 1)
    }

    pub fn active(&self) -> impl Iterator<Item = &Adjustment> {
        self.items.iter().filter(|a| !a.undone)
    }
}

/// What the adjustments need to know about a session's model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFacts {
    pub id: String,
    pub window: u32,
    /// What the model is asked for as output when its profile says nothing.
    pub max_output: u32,
}

/// Speaks louder where a model was cut by its output limit again and again: raises the output of
/// that model in the preferences, never past half its window. Only the
/// occurrences after the last raise count, so a raise that was not enough is followed by one more,
/// not by a runaway. Returns what was changed.
pub fn auto_adjust(entries: &[Entry], now: u64, models: &[ModelFacts], prefs: &mut LocalPrefs, adjustments: &mut Adjustments) -> Vec<Adjustment> {
    let mut made = Vec::new();
    for model in models {
        let last = adjustments.active().filter(|a| a.model == model.id && a.setting == "max_output").map(|a| a.ts).max().unwrap_or(0);
        let since = now.saturating_sub(WINDOW_SECS).max(last + u64::from(last > 0));
        let truncated = recurring_in(entries, since)
            .into_iter()
            .any(|r| r.kind == FrictionKind::OutputTruncated && r.model == model.id);
        if !truncated {
            continue;
        }
        let before = prefs.model(&model.id).max_output;
        let current = before.unwrap_or(model.max_output);
        let target = current.saturating_mul(2).min(model.window / 2).min(MAX_OUTPUT_CEILING) / 1024 * 1024;
        if target <= current {
            continue;
        }
        prefs.model_mut(&model.id).max_output = Some(target);
        let adjustment = Adjustment { id: adjustments.next_id(), ts: now, model: model.id.clone(), setting: "max_output".into(), before, after: target, undone: false };
        adjustments.items.push(adjustment.clone());
        made.push(adjustment);
    }
    made
}

/// Takes an adjustment back. A setting the user changed since is left as they set it.
pub fn undo(adjustments: &mut Adjustments, id: &str, prefs: &mut LocalPrefs) -> Result<String, String> {
    let adjustment = adjustments
        .items
        .iter_mut()
        .find(|a| a.id == id && !a.undone)
        .ok_or_else(|| po_i18n::t!("friction.undo.unknown", id = id).to_string())?;
    adjustment.undone = true;
    if prefs.model(&adjustment.model).max_output != Some(adjustment.after) {
        return Ok(po_i18n::t!("friction.undo.changed", model = adjustment.model).to_string());
    }
    prefs.model_mut(&adjustment.model).max_output = adjustment.before;
    Ok(po_i18n::t!("friction.undo.done", model = adjustment.model).to_string())
}

fn describe_adjustment(a: &Adjustment) -> String {
    po_i18n::t!(
        "friction.adjusted",
        model = a.model,
        before = a.before.map_or_else(|| "-".to_owned(), |b| b.to_string()),
        after = a.after,
        id = a.id
    )
    .to_string()
}

/// At the start of a session: speaks louder where the journal says to, and says so; and tells
/// once of the recurring frictions found. Returns the lines to show as notes.
pub fn at_startup(agent: &mut po_agent::Agent) -> Vec<String> {
    let Some(dir) = dir() else { return Vec::new() };
    let entries = Journal::at(&dir).read();
    let now = now();
    let mut notes = Vec::new();
    let models: Vec<ModelFacts> = agent
        .router()
        .models()
        .iter()
        .filter(|m| m.origin.is_local())
        .map(|m| ModelFacts { id: m.id.clone(), window: m.context_window, max_output: m.max_output_tokens })
        .collect();
    let mut prefs = LocalPrefs::load();
    let mut adjustments = Adjustments::load_from(&dir);
    let made = auto_adjust(&entries, now, &models, &mut prefs, &mut adjustments);
    if !made.is_empty() && prefs.save().is_ok() && adjustments.save_to(&dir).is_ok() {
        crate::engines_ops::apply_session_defaults(agent, &prefs);
        notes.extend(made.iter().map(describe_adjustment));
    }
    notes.extend(startup_line(&dir, &entries, now));
    notes
}

// ------------------------------------------------------------------------------- /frictions

/// What a recurring snag is, in the user's words.
pub fn describe(r: &Recurring) -> String {
    let (tool, signature) = r.tool_and_signature();
    match r.kind {
        FrictionKind::ToolError => po_i18n::t!("friction.desc.tool_error", tool = tool, signature = signature).to_string(),
        FrictionKind::InvalidArgs => po_i18n::t!("friction.desc.invalid_args", tool = tool).to_string(),
        FrictionKind::OutsideWrite => po_i18n::t!("friction.desc.outside_write", tool = tool).to_string(),
        FrictionKind::LoopWarning => po_i18n::t!("friction.desc.loop_warning").to_string(),
        FrictionKind::OutputTruncated => po_i18n::t!("friction.desc.output_truncated").to_string(),
        FrictionKind::ModelLoadFailed => po_i18n::t!("friction.desc.model_load_failed", detail = r.detail).to_string(),
        FrictionKind::ContextOverflow => po_i18n::t!("friction.desc.context_overflow").to_string(),
        FrictionKind::Denied | FrictionKind::Interrupted | FrictionKind::QuestionSetAside => r.key.clone(),
    }
}

fn model_label(r: &Recurring) -> String {
    if r.model.is_empty() { "-".to_owned() } else { r.model.clone() }
}

/// What goes in the project's `PO.md` for a snag: a fact for the next model that reads it.
pub fn note_line(r: &Recurring) -> String {
    let (tool, signature) = r.tool_and_signature();
    match r.kind {
        FrictionKind::ToolError => format!("With {}, `{tool}` often fails with \"{signature}\": find the cause before retrying.", model_label(r)),
        FrictionKind::InvalidArgs => format!("With {}, arguments for `{tool}` often arrive invalid: keep them short, write big files in parts.", model_label(r)),
        FrictionKind::OutputTruncated => format!("With {}, replies often hit the output limit: keep each one short.", model_label(r)),
        FrictionKind::OutsideWrite => format!("With {}, `{tool}` often reaches outside the workspace: stay in the project.", model_label(r)),
        _ => format!("With {}: {} ({} times).", model_label(r), describe(r), r.count),
    }
}

const NOTES_HEADING: &str = "## Known snags (noted by Po)";

/// Adds `line` to the project's `PO.md` under a heading of its own, creating the file if need be.
/// The same line is never added twice. Returns whether the file changed.
pub fn add_project_note(workspace: &Path, line: &str) -> std::io::Result<bool> {
    let path = workspace.join("PO.md");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let bullet = format!("- {line}");
    if text.lines().any(|l| l.trim() == bullet) {
        return Ok(false);
    }
    if !text.contains(NOTES_HEADING) {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(NOTES_HEADING);
        text.push('\n');
    } else if !text.ends_with('\n') {
        text.push('\n');
    }
    // The bullet goes at the end of the file when the heading is its last section, else after
    // the heading's own list: simplest and stable, at the end under a heading kept last.
    text.push_str(&bullet);
    text.push('\n');
    std::fs::write(&path, text)?;
    Ok(true)
}

fn labs_summary(r: &Recurring) -> String {
    format!(
        "Recurring snag with {}: {} ({} times over {} sessions). Make Po handle it better.",
        model_label(r),
        describe(r),
        r.count,
        r.sessions
    )
}

/// The recurring frictions the user may still act on, in the order `/frictions` numbers them.
fn open_list(dir: &Path) -> Vec<Recurring> {
    let seen = Seen::load_from(dir);
    let entries = Journal::at(dir).read();
    recurring_in(&entries, now().saturating_sub(WINDOW_SECS)).into_iter().filter(|r| !seen.ignored.contains(&r.id())).collect()
}

/// `/frictions`: the recurring snags, and what Po did about some. In line mode (and for the
/// phone's text), the list; the menus come from [`menu`].
pub fn command(argument: Option<&str>, rest: &[&str], workspace: &Path, reply: &mut crate::commands::Reply) {
    let Some(dir) = dir() else {
        reply.warn(po_i18n::t!("friction.no_dir"));
        return;
    };
    let Some(prefs_file) = po_local::prefs::path() else {
        reply.warn(po_i18n::t!("friction.no_dir"));
        return;
    };
    command_in(&dir, &prefs_file, argument, rest, workspace, crate::labs::enabled(), reply);
}

/// [`command`] with its places given, for the tests: where the journal's folder and the local
/// preferences file are, and whether Po Labs is on.
fn command_in(
    dir: &Path,
    prefs_file: &Path,
    argument: Option<&str>,
    rest: &[&str],
    workspace: &Path,
    labs: bool,
    reply: &mut crate::commands::Reply,
) {
    match argument {
        Some("undo" | "annuler") => {
            let Some(id) = rest.first() else {
                reply.warn(po_i18n::t!("friction.undo.usage"));
                return;
            };
            let mut adjustments = Adjustments::load_from(dir);
            let mut local = LocalPrefs::load_from(prefs_file);
            match undo(&mut adjustments, id, &mut local) {
                Ok(text) => {
                    if adjustments.save_to(dir).is_ok() && local.save_to(prefs_file).is_ok() {
                        reply.accent(text);
                    } else {
                        reply.warn(po_i18n::t!("friction.undo.not_saved"));
                    }
                }
                Err(text) => reply.warn(text),
            }
        }
        Some(number) if number.parse::<usize>().is_ok() => {
            let list = open_list(dir);
            let Some(r) = number.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| list.get(i)) else {
                reply.warn(po_i18n::t!("friction.unknown", n = number));
                return;
            };
            match rest.first().copied() {
                Some("note") => match add_project_note(workspace, &note_line(r)) {
                    Ok(true) => reply.accent(po_i18n::t!("friction.noted")),
                    Ok(false) => reply.dim(po_i18n::t!("friction.already_noted")),
                    Err(err) => reply.warn(err.to_string()),
                },
                Some("labs") if labs => {
                    let summary = labs_summary(r);
                    match po_tui::labs::Notebook::modify(|book| book.add(&summary, "improvement", false)) {
                        Ok((_, n)) => reply.accent(po_i18n::t!("friction.labs_added", n = n)),
                        Err(err) => reply.warn(err.to_string()),
                    }
                }
                Some("labs") => reply.warn(po_i18n::t!("friction.labs_off")),
                Some("ignore" | "ignorer") => {
                    let mut seen = Seen::load_from(dir);
                    if !seen.ignored.contains(&r.id()) {
                        seen.ignored.push(r.id());
                    }
                    seen.save_to(dir);
                    reply.accent(po_i18n::t!("friction.ignored"));
                }
                _ => reply.dim(po_i18n::t!("friction.choose", n = number)),
            }
        }
        _ => {
            let list = open_list(dir);
            let adjustments = Adjustments::load_from(dir);
            if list.is_empty() && adjustments.active().next().is_none() {
                reply.dim(po_i18n::t!("friction.none"));
                return;
            }
            if !list.is_empty() {
                reply.section(po_i18n::t!("friction.title", n = list.len()));
            }
            for (i, r) in list.iter().enumerate() {
                reply.item(format!("{}.", i + 1), po_i18n::t!("friction.row", what = describe(r), count = r.count, sessions = r.sessions, model = model_label(r)));
            }
            if !list.is_empty() {
                reply.dim(po_i18n::t!("friction.how"));
            }
            for a in adjustments.active() {
                reply.item(describe_adjustment(a), "");
            }
        }
    }
}

/// The menus of `/frictions`: the list, then the three proposals for the one chosen.
pub fn menu(cmd: &str, argument: Option<&str>, rest: &[&str]) -> Option<po_tui::menu::Menu> {
    use po_tui::menu::{Item, Menu};
    let dir = dir()?;
    match (argument, rest.first()) {
        (None, _) => {
            let list = open_list(&dir);
            let adjustments = Adjustments::load_from(&dir);
            if list.is_empty() && adjustments.active().next().is_none() {
                return None;
            }
            let mut items: Vec<Item> = list
                .iter()
                .enumerate()
                .map(|(i, r)| Item::run(describe(r), format!("{cmd} {}", i + 1)).detail(po_i18n::t!("friction.detail", count = r.count, sessions = r.sessions).to_string()))
                .collect();
            for a in adjustments.active() {
                items.push(Item::run(describe_adjustment(a), format!("{cmd} undo {}", a.id)).detail(po_i18n::t!("friction.undo.item").to_string()));
            }
            Some(Menu::new(po_i18n::t!("friction.menu"), items))
        }
        (Some(number), None) if number.parse::<usize>().is_ok() => {
            let list = open_list(&dir);
            let r = number.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| list.get(i))?;
            let mut items = vec![Item::run(po_i18n::t!("friction.menu.note"), format!("{cmd} {number} note")).detail(po_i18n::t!("friction.menu.note_hint").to_string())];
            if crate::labs::enabled() {
                items.push(Item::run(po_i18n::t!("friction.menu.labs"), format!("{cmd} {number} labs")).detail(po_i18n::t!("friction.menu.labs_hint").to_string()));
            }
            items.push(Item::run(po_i18n::t!("friction.menu.ignore"), format!("{cmd} {number} ignore")));
            Some(Menu::new(describe(r), items).back(cmd.to_owned()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 3600;

    fn entry(ts: u64, session: &str, kind: FrictionKind, key: &str, model: &str) -> Entry {
        Entry { ts, session: Some(session.into()), model: model.into(), kind: kind.as_str().into(), key: key.into(), detail: "d".into() }
    }

    fn journal_in(dir: &Path, entries: &[Entry]) -> Journal {
        let journal = Journal::at(dir);
        for e in entries {
            journal.append(e);
        }
        journal
    }

    #[test]
    fn an_entry_is_bounded_and_has_no_room_for_a_file() {
        let long = Friction {
            kind: FrictionKind::ToolError,
            key: "k".repeat(500),
            detail: "x".repeat(900),
            model: "m".into(),
            session: Some("s".into()),
        };
        let entry = Entry::of(&long, 7);
        assert_eq!(entry.detail.chars().count(), DETAIL_MAX);
        assert!(entry.key.chars().count() <= 120);
        assert_eq!((entry.kind.as_str(), entry.ts), ("tool_error", 7));
    }

    #[test]
    fn the_journal_appends_and_a_damaged_line_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let journal = journal_in(dir.path(), &[entry(1, "a", FrictionKind::ToolError, "k", "m")]);
        std::fs::write(dir.path().join("friction.jsonl"), format!("{}not json\n", std::fs::read_to_string(dir.path().join("friction.jsonl")).unwrap())).unwrap();
        journal.append(&entry(2, "a", FrictionKind::InvalidArgs, "edit", "m"));
        let read = journal.read();
        assert_eq!(read.len(), 2);
        assert_eq!(read[1].kind, "invalid_args");
    }

    #[test]
    fn the_journal_keeps_its_newest_five_thousand_lines() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        for n in 0..(CAP + 250) {
            journal.append(&entry(n as u64, "s", FrictionKind::ToolError, "k", "m"));
        }
        // The check runs every hundred appends: never far over, and a trim makes it exact.
        assert!(journal.read().len() <= CAP + CHECK_EVERY);
        journal.trim();
        let read = journal.read();
        assert_eq!(read.len(), CAP);
        assert_eq!(read.first().unwrap().ts, 250, "the oldest went");
        assert_eq!(read.last().unwrap().ts, (CAP + 249) as u64, "the newest stays");
    }

    #[test]
    fn a_friction_recurs_from_three_times_over_two_sessions_in_fourteen_days() {
        let now = 100 * DAY;
        let at = |days_ago: u64, session: &str| entry(now - days_ago * DAY, session, FrictionKind::ToolError, "edit|old not found", "qwen");
        let since = now - WINDOW_SECS;
        // Three, one session: no.
        assert!(recurring_in(&[at(1, "a"), at(1, "a"), at(2, "a")], since).is_empty());
        // Two sessions, two occurrences: no.
        assert!(recurring_in(&[at(1, "a"), at(1, "b")], since).is_empty());
        // Three over two sessions: yes.
        let found = recurring_in(&[at(1, "a"), at(2, "a"), at(3, "b")], since);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].count, found[0].sessions), (3, 2));
        // The oldest outside the window does not count.
        assert!(recurring_in(&[at(1, "a"), at(2, "b"), at(20, "b")], since).is_empty());
        // Another model is another snag.
        let mut other = at(3, "b");
        other.model = "gemma".into();
        assert!(recurring_in(&[at(1, "a"), at(2, "a"), other], since).is_empty());
    }

    #[test]
    fn what_the_user_chose_is_never_a_snag() {
        let now = 100 * DAY;
        let all: Vec<Entry> = [FrictionKind::Denied, FrictionKind::Interrupted, FrictionKind::QuestionSetAside]
            .into_iter()
            .flat_map(|k| (0..3).map(move |i| entry(now - DAY, if i == 0 { "a" } else { "b" }, k, "k", "m")))
            .collect();
        assert!(recurring_in(&all, 0).is_empty());
    }

    fn recurring_entries(now: u64, kind: FrictionKind, key: &str, model: &str) -> Vec<Entry> {
        (0..3).map(|i| entry(now - DAY, if i == 0 { "a" } else { "b" }, kind, key, model)).collect()
    }

    #[test]
    fn the_startup_line_is_shown_once_per_new_recurring_friction() {
        let dir = tempfile::tempdir().unwrap();
        let now = 100 * DAY;
        let mut entries = recurring_entries(now, FrictionKind::ToolError, "edit|old not found", "qwen");
        let line = startup_line(dir.path(), &entries, now).expect("a new recurring friction");
        assert!(line.contains('1') && line.contains("/frictions"), "{line}");
        assert_eq!(startup_line(dir.path(), &entries, now), None, "told once");
        // A second one is new: told, with only itself counted.
        entries.extend(recurring_entries(now, FrictionKind::OutputTruncated, "output_limit", "qwen"));
        assert!(startup_line(dir.path(), &entries, now).unwrap().contains('1'));
        assert_eq!(startup_line(dir.path(), &entries, now), None);
        // One the user ignored is never told.
        let dir = tempfile::tempdir().unwrap();
        let only = recurring_entries(now, FrictionKind::ToolError, "read|x", "m");
        Seen { announced: vec![], ignored: vec![recurring_in(&only, 0)[0].id()] }.save_to(dir.path());
        assert_eq!(startup_line(dir.path(), &only, now), None);
    }

    #[test]
    fn what_keeps_failing_for_a_model_becomes_a_line_for_that_model() {
        let now = 100 * DAY;
        let mut entries = recurring_entries(now, FrictionKind::ToolError, "edit|old_string not found", "qwen");
        entries.extend(recurring_entries(now, FrictionKind::InvalidArgs, "write", "qwen"));
        entries.extend(recurring_entries(now, FrictionKind::ToolError, "read|nope", "gemma"));
        let lines = hints_in(&entries, "qwen", now);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("snag[edit]:") && l.contains("read the file again before editing")));
        assert!(lines.iter().any(|l| l.starts_with("snag[write args]:")));
        assert!(hints_in(&entries, "mistral", now).is_empty());
        // The keys differ, so the state's diff tells the lines apart.
        assert_ne!(po_agent::awareness::key_of(&lines[0]), po_agent::awareness::key_of(&lines[1]));
    }

    fn facts() -> Vec<ModelFacts> {
        vec![ModelFacts { id: "qwen".into(), window: 32_768, max_output: 4096 }]
    }

    #[test]
    fn a_model_cut_again_and_again_gets_a_higher_output_and_the_change_can_be_undone() {
        let now = 100 * DAY;
        let entries = recurring_entries(now, FrictionKind::OutputTruncated, "output_limit", "qwen");
        let mut prefs = LocalPrefs::default();
        let mut adjustments = Adjustments::default();
        let made = auto_adjust(&entries, now, &facts(), &mut prefs, &mut adjustments);
        assert_eq!(made.len(), 1);
        assert_eq!((made[0].before, made[0].after, made[0].id.as_str()), (None, 8192, "a1"));
        assert_eq!(prefs.model("qwen").max_output, Some(8192));
        // The same occurrences do not raise it again.
        assert!(auto_adjust(&entries, now + 10, &facts(), &mut prefs, &mut adjustments).is_empty());
        // Undone: back to what it was (no profile value).
        let said = undo(&mut adjustments, "a1", &mut prefs).unwrap();
        assert!(said.contains("qwen"));
        assert_eq!(prefs.model("qwen").max_output, None);
        assert!(undo(&mut adjustments, "a1", &mut prefs).is_err(), "already undone");
        assert!(undo(&mut adjustments, "zz", &mut prefs).is_err());
    }

    #[test]
    fn the_raise_stops_at_half_the_window_and_at_the_ceiling() {
        let now = 100 * DAY;
        let entries = recurring_entries(now, FrictionKind::OutputTruncated, "output_limit", "qwen");
        let mut prefs = LocalPrefs::default();
        prefs.model_mut("qwen").max_output = Some(8192);
        let tight = vec![ModelFacts { id: "qwen".into(), window: 16_384, max_output: 4096 }];
        // Half of 16k is 8k: already there, nothing to raise.
        assert!(auto_adjust(&entries, now, &tight, &mut prefs, &mut Adjustments::default()).is_empty());
        let wide = vec![ModelFacts { id: "qwen".into(), window: 262_144, max_output: 4096 }];
        prefs.model_mut("qwen").max_output = Some(24_576);
        let made = auto_adjust(&entries, now, &wide, &mut prefs, &mut Adjustments::default());
        assert_eq!(made[0].after, MAX_OUTPUT_CEILING);
        // Another model's snag does not touch this one.
        let mut prefs = LocalPrefs::default();
        let other = recurring_entries(now, FrictionKind::OutputTruncated, "output_limit", "gemma");
        assert!(auto_adjust(&other, now, &facts(), &mut prefs, &mut Adjustments::default()).is_empty());
    }

    #[test]
    fn a_setting_the_user_changed_since_is_left_alone_on_undo() {
        let mut prefs = LocalPrefs::default();
        prefs.model_mut("qwen").max_output = Some(8192);
        let mut adjustments = Adjustments {
            items: vec![Adjustment { id: "a1".into(), ts: 1, model: "qwen".into(), setting: "max_output".into(), before: None, after: 8192, undone: false }],
        };
        prefs.model_mut("qwen").max_output = Some(16_384);
        undo(&mut adjustments, "a1", &mut prefs).unwrap();
        assert_eq!(prefs.model("qwen").max_output, Some(16_384));
        assert!(adjustments.items[0].undone);
    }

    #[test]
    fn adjustments_survive_in_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut adjustments = Adjustments::default();
        auto_adjust(&recurring_entries(100 * DAY, FrictionKind::OutputTruncated, "output_limit", "qwen"), 100 * DAY, &facts(), &mut LocalPrefs::default(), &mut adjustments);
        adjustments.save_to(dir.path()).unwrap();
        assert_eq!(Adjustments::load_from(dir.path()), adjustments);
        assert_eq!(Adjustments::load_from(&dir.path().join("none")), Adjustments::default());
    }

    #[test]
    fn a_note_goes_to_the_project_once_under_its_own_heading() {
        let dir = tempfile::tempdir().unwrap();
        let line = "With qwen, `edit` often fails.";
        std::fs::write(dir.path().join("PO.md"), "# Rules\nBe brief.").unwrap();
        assert!(add_project_note(dir.path(), line).unwrap());
        assert!(!add_project_note(dir.path(), line).unwrap(), "never twice");
        let text = std::fs::read_to_string(dir.path().join("PO.md")).unwrap();
        assert!(text.starts_with("# Rules\nBe brief.\n\n## Known snags (noted by Po)\n- With qwen"), "{text}");
        assert_eq!(text.matches("## Known snags").count(), 1);
        assert!(add_project_note(dir.path(), "Another.").unwrap());
        assert_eq!(std::fs::read_to_string(dir.path().join("PO.md")).unwrap().matches("## Known snags").count(), 1);
        // No file yet: created.
        let fresh = tempfile::tempdir().unwrap();
        assert!(add_project_note(fresh.path(), line).unwrap());
        assert!(std::fs::read_to_string(fresh.path().join("PO.md")).unwrap().starts_with("## Known snags"));
    }

    fn lines_of(reply: &crate::commands::Reply) -> String {
        reply.lines.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn the_command_lists_then_applies_only_what_the_user_picks() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let now = now();
        let entries: Vec<Entry> = (0..3)
            .map(|i| entry(now - 60, if i == 0 { "a" } else { "b" }, FrictionKind::ToolError, "edit|old_string not found", "qwen"))
            .collect();
        journal_in(dir.path(), &entries);
        let prefs_file = dir.path().join("local.json");
        let run = |argument: Option<&str>, rest: &[&str], labs: bool| {
            let mut reply = crate::commands::Reply::default();
            command_in(dir.path(), &prefs_file, argument, rest, workspace.path(), labs, &mut reply);
            lines_of(&reply)
        };
        let listed = run(None, &[], false);
        assert!(listed.contains("edit") && listed.contains("old_string not found") && listed.contains("3"), "{listed}");
        assert!(!workspace.path().join("PO.md").exists(), "listing changes nothing");
        // Picking a number without an action changes nothing either.
        run(Some("1"), &[], false);
        assert!(!workspace.path().join("PO.md").exists());
        run(Some("1"), &["note"], false);
        assert!(std::fs::read_to_string(workspace.path().join("PO.md")).unwrap().contains("`edit` often fails"));
        assert!(run(Some("1"), &["labs"], false).to_lowercase().contains("labs"), "Labs off: said so");
        run(Some("1"), &["ignore"], false);
        assert!(run(None, &[], false).contains("no recurring snag"), "ignored: gone from the list");
        assert!(run(Some("9"), &[], false).contains('9'));
    }

    #[test]
    fn the_command_undoes_an_adjustment_in_the_preferences_file() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let dir = tempfile::tempdir().unwrap();
        let prefs_file = dir.path().join("local.json");
        let mut prefs = LocalPrefs::default();
        let mut adjustments = Adjustments::default();
        let entries = recurring_entries(100 * DAY, FrictionKind::OutputTruncated, "output_limit", "qwen");
        auto_adjust(&entries, 100 * DAY, &facts(), &mut prefs, &mut adjustments);
        prefs.save_to(&prefs_file).unwrap();
        adjustments.save_to(dir.path()).unwrap();
        let mut reply = crate::commands::Reply::default();
        command_in(dir.path(), &prefs_file, Some("undo"), &["a1"], dir.path(), false, &mut reply);
        assert_eq!(LocalPrefs::load_from(&prefs_file).model("qwen").max_output, None);
        assert!(Adjustments::load_from(dir.path()).items[0].undone);
        let mut again = crate::commands::Reply::default();
        command_in(dir.path(), &prefs_file, Some("undo"), &["a1"], dir.path(), false, &mut again);
        assert!(again.lines[0].0 == crate::commands::Tone::Warn, "{:?}", again.lines);
    }
}
