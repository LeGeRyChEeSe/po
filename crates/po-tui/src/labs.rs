//! Po Labs on the interface side: the notebook of requests about Po, and the shortcuts.
//!
//! During a session, the model writes down what the user says about Po itself — a display
//! issue, a bug, an idea — without acting on it: the notebook. When Po is idle, a discreet
//! proposal offers to have Po Labs implement it, in a session of its own. The notebook is a
//! file, so that a request outlives the session it was made in, and it stays in English: it
//! feeds the public repository's issues.
//!
//! Po Labs works like a pupil with homework in several subjects: one change at a time is on
//! trial — built, installed, and in the user's hands — and the next request waits until the user
//! has said whether to keep it, undo it or have it adjusted. Meanwhile, everything the user says
//! about Po is sorted: a remark on the change on trial, an update of a request already noted, or
//! a new one.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Pending,
    /// Implemented, and on trial: the user has not said yet whether to keep it.
    Trial,
    /// Implemented and kept by the user.
    Done,
    /// Withdrawn by the user.
    Dropped,
    /// Implemented, tried, and undone by the user.
    Refused,
}

/// When the user can see a change at work, so that they are asked about it only then: at once
/// (a text on screen), after their next turn, or once tools were called (how calls look).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visible {
    AtOnce,
    #[default]
    NextTurn,
    ToolCalls,
}

impl Visible {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "at_once" => Self::AtOnce,
            "tool_calls" => Self::ToolCalls,
            _ => Self::NextTurn,
        }
    }
}

/// The change on trial: built, installed, committed in the Po Labs workspace between `base` and
/// `head`, waiting for the user's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trial {
    /// The requests it implements.
    pub ids: Vec<u32>,
    pub summary: String,
    /// The workspace's commit before it, and its own last one.
    pub base: String,
    pub head: String,
    #[serde(default)]
    pub visible: Visible,
    /// The binary it replaced: undoing the change puts it back without a build.
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default)]
    pub date: String,
    /// What the user wants adjusted, not yet taken up by Po Labs.
    #[serde(default)]
    pub revisions: Vec<String>,
    /// The `head` the user's session was told about.
    #[serde(default)]
    pub announced: String,
}

/// A change kept by the user: the material of a contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub ids: Vec<u32>,
    pub summary: String,
    pub base: String,
    pub head: String,
    /// One of its requests touches Po's core.
    #[serde(default)]
    pub core: bool,
    #[serde(default)]
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u32,
    pub summary: String,
    /// bug, improvement, idea.
    #[serde(default)]
    pub kind: String,
    /// Touches Po's core behavior rather than its appearance.
    #[serde(default)]
    pub core: bool,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub date: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notebook {
    #[serde(default)]
    pub items: Vec<Item>,
    /// Bumped at every change: a proposal set aside reappears only when the notebook changed.
    #[serde(default)]
    pub revision: u64,
    /// The Po Labs session implementing the notebook, until it is done: reopened where it was
    /// left, even after a restart.
    #[serde(default)]
    pub session: Option<String>,
    /// The change on trial, if any.
    #[serde(default)]
    pub trial: Option<Trial>,
    /// The changes kept, oldest first.
    #[serde(default)]
    pub kept: Vec<Kept>,
}

fn notebook_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("labs").join("notebook.json"))
}

impl Notebook {
    pub fn load() -> Self {
        notebook_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = notebook_path().ok_or_else(|| std::io::Error::other("configuration directory not found"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    pub fn pending(&self) -> Vec<&Item> {
        self.items.iter().filter(|i| i.status == Status::Pending).collect()
    }

    pub fn add(&mut self, summary: &str, kind: &str, core: bool) -> u32 {
        let id = self.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        self.items.push(Item {
            id,
            summary: summary.trim().to_owned(),
            kind: kind.to_owned(),
            core,
            status: Status::Pending,
            date: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        });
        self.revision += 1;
        id
    }

    /// Rewrites a pending request; false if there is none with this id.
    pub fn update(&mut self, id: u32, summary: Option<&str>, kind: Option<&str>, core: Option<bool>) -> bool {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id && i.status == Status::Pending) else {
            return false;
        };
        if let Some(summary) = summary.filter(|s| !s.trim().is_empty()) {
            item.summary = summary.trim().to_owned();
        }
        if let Some(kind) = kind {
            item.kind = kind.to_owned();
        }
        if let Some(core) = core {
            item.core = core;
        }
        self.revision += 1;
        true
    }

    pub fn set_status(&mut self, id: u32, status: Status) -> bool {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else { return false };
        item.status = status;
        self.revision += 1;
        true
    }

    /// Po Labs applied a change: it goes on trial, for the requests `ids` (the first pending one
    /// when none is named). Applied again while on trial (adjusted), the same trial goes on, with
    /// its new head, and the user is asked again.
    pub fn start_trial(&mut self, ids: &[u32], summary: &str, base: &str, head: &str, visible: Visible, previous: Option<String>) {
        let mut ids: Vec<u32> = ids.iter().copied().filter(|id| self.items.iter().any(|i| i.id == *id)).collect();
        if let Some(trial) = &self.trial {
            ids.extend(trial.ids.iter().copied());
        }
        if ids.is_empty() {
            ids.extend(self.pending().first().map(|i| i.id));
        }
        ids.sort_unstable();
        ids.dedup();
        for item in self.items.iter_mut().filter(|i| ids.contains(&i.id)) {
            item.status = Status::Trial;
        }
        let date = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        self.trial = Some(match self.trial.take() {
            Some(trial) => Trial { ids, summary: summary.trim().to_owned(), head: head.to_owned(), visible, date, revisions: Vec::new(), ..trial },
            None => Trial {
                ids,
                summary: summary.trim().to_owned(),
                base: base.to_owned(),
                head: head.to_owned(),
                visible,
                previous,
                date,
                revisions: Vec::new(),
                announced: String::new(),
            },
        });
        self.revision += 1;
    }

    /// The user keeps the change on trial: its requests are done, and it waits to be shared.
    pub fn keep_trial(&mut self) -> Option<Kept> {
        let trial = self.trial.take()?;
        let mut core = false;
        for item in self.items.iter_mut().filter(|i| trial.ids.contains(&i.id)) {
            item.status = Status::Done;
            core |= item.core;
        }
        let kept = Kept { ids: trial.ids, summary: trial.summary, base: trial.base, head: trial.head, core, date: trial.date };
        self.kept.push(kept.clone());
        // The next request starts a fresh session.
        self.session = None;
        self.revision += 1;
        Some(kept)
    }

    /// The user undoes the change on trial: its requests are marked refused.
    pub fn refuse_trial(&mut self) -> Option<Trial> {
        let trial = self.trial.take()?;
        for item in self.items.iter_mut().filter(|i| trial.ids.contains(&i.id)) {
            item.status = Status::Refused;
        }
        self.session = None;
        self.revision += 1;
        Some(trial)
    }

    /// A remark of the user on the change on trial, for Po Labs to take up; false if none is.
    pub fn revise(&mut self, remark: &str) -> bool {
        let Some(trial) = self.trial.as_mut() else { return false };
        trial.revisions.push(remark.trim().to_owned());
        self.revision += 1;
        true
    }

    /// A change on trial waits for the user's verdict (nothing left to adjust).
    pub fn awaiting_verdict(&self) -> bool {
        self.trial.as_ref().is_some_and(|t| t.revisions.is_empty())
    }

    /// Po Labs has something to do: adjust the change on trial, or else, with none on trial, take
    /// the next request. A change waiting for the verdict holds the rest back.
    pub fn has_work(&self) -> bool {
        match &self.trial {
            Some(trial) => !trial.revisions.is_empty(),
            None => !self.pending().is_empty(),
        }
    }

    /// The change on trial, as the models read it.
    pub fn trial_text(&self) -> String {
        match &self.trial {
            None => "(no change on trial)".into(),
            Some(trial) => {
                let ids: Vec<String> = trial.ids.iter().map(|id| format!("#{id}")).collect();
                let mut text = format!("{} {}", ids.join(", "), trial.summary);
                for remark in &trial.revisions {
                    text.push_str(&format!("\n  - to adjust: {remark}"));
                }
                text
            }
        }
    }

    /// The pending requests as the model reads them.
    pub fn pending_text(&self) -> String {
        let pending = self.pending();
        if pending.is_empty() {
            return "(no pending request)".into();
        }
        pending
            .iter()
            .map(|i| {
                format!("#{} [{}{}] {}", i.id, if i.kind.is_empty() { "idea" } else { &i.kind }, if i.core { ", core" } else { "" }, i.summary)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What the user says about the change on trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    Undo,
    Revise,
}

impl Verdict {
    pub const ALL: [Verdict; 3] = [Verdict::Keep, Verdict::Undo, Verdict::Revise];

    /// Its key on the card, in the interface language (o/n/m in French, y/n/e in English): a
    /// plain letter, only taken while the card is open and the input empty.
    pub fn key(self) -> char {
        let key = match self {
            Verdict::Keep => po_i18n::t!("tui.labs.key_keep"),
            Verdict::Undo => po_i18n::t!("tui.labs.key_undo"),
            Verdict::Revise => po_i18n::t!("tui.labs.key_revise"),
        };
        key.chars().next().unwrap_or('?')
    }

    pub fn from_key(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == c.to_ascii_lowercase())
    }

    pub fn label(self) -> &'static str {
        match self {
            Verdict::Keep => po_i18n::t!("tui.labs.keep"),
            Verdict::Undo => po_i18n::t!("tui.labs.undo"),
            Verdict::Revise => po_i18n::t!("tui.labs.revise"),
        }
    }
}

/// A key combination: modifiers and one key, up to four keys at once (`ctrl+shift+alt+y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl Shortcut {
    /// `ctrl+y`, `alt+shift+p`, `f3`, `ctrl+space`… Case does not matter.
    pub fn parse(raw: &str) -> Option<Self> {
        let parts: Vec<String> = raw.split('+').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        let mut modifiers = KeyModifiers::NONE;
        for part in &parts[..parts.len() - 1] {
            modifiers |= match part.as_str() {
                "ctrl" | "control" | "ctl" => KeyModifiers::CONTROL,
                "alt" | "option" => KeyModifiers::ALT,
                "shift" | "maj" => KeyModifiers::SHIFT,
                _ => return None,
            };
        }
        let key = parts.last()?;
        let code = match key.as_str() {
            "space" | "espace" => KeyCode::Char(' '),
            "enter" | "entree" | "entrée" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" | "echap" | "échap" => KeyCode::Esc,
            f if f.starts_with('f') && f.len() > 1 => KeyCode::F(f[1..].parse().ok().filter(|n| (1..=24).contains(n))?),
            c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?),
            _ => return None,
        };
        Some(Self { modifiers, code })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        let code = match key.code {
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        // Shift is part of a letter's case: compare it only when the shortcut asks for it.
        let mut modifiers = key.modifiers;
        if !self.modifiers.contains(KeyModifiers::SHIFT) {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        code == self.code && modifiers == self.modifiers
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift".to_owned());
        }
        parts.push(match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            _ => "?".to_owned(),
        });
        parts.join("+")
    }
}

/// The actions with a configurable shortcut, with their default.
pub const ACTIONS: &[(&str, &str)] = &[("labs", "ctrl+y"), ("pause", "ctrl+t")];

/// The shortcut of an action: the user's (`ui.json`, `keys.<action>`), else the default.
pub fn shortcut(action: &str) -> Shortcut {
    po_i18n::prefs::get_str(&format!("keys.{action}"))
        .and_then(|s| Shortcut::parse(&s))
        .or_else(|| ACTIONS.iter().find(|(a, _)| *a == action).and_then(|(_, d)| Shortcut::parse(d)))
        .unwrap_or(Shortcut { modifiers: KeyModifiers::CONTROL, code: KeyCode::Char('y') })
}

/// Minutes Po stays idle before Po Labs starts on its own (`ui.json`, `labs_idle_minutes`);
/// 0 waits for the user.
pub fn idle_minutes() -> u64 {
    po_i18n::prefs::get_str("labs_idle_minutes").and_then(|v| v.trim().parse().ok()).unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_added_rewritten_withdrawn_and_completed() {
        let mut book = Notebook::default();
        let a = book.add("make tool calls orange", "improvement", false);
        let b = book.add("the status line forgets the branch", "bug", true);
        assert_eq!((a, b), (1, 2));
        assert!(book.update(a, Some("make tool calls orange, failures stay red"), None, None));
        assert!(book.set_status(b, Status::Dropped));
        assert_eq!(book.pending().len(), 1);
        assert!(book.pending_text().contains("#1 [improvement] make tool calls orange, failures stay red"));
        assert!(!book.update(b, Some("x"), None, None), "a withdrawn request is not rewritten");
    }

    #[test]
    fn one_change_on_trial_holds_the_next_requests_until_the_verdict() {
        let mut book = Notebook::default();
        let a = book.add("make tool calls orange", "improvement", false);
        let b = book.add("show the branch", "improvement", true);
        assert!(book.has_work());
        book.start_trial(&[a], "tool calls are orange", "base", "c1", Visible::ToolCalls, Some("po.old".into()));
        assert!(book.awaiting_verdict());
        assert!(!book.has_work(), "the next request waits for the verdict");
        assert_eq!(book.pending().len(), 1);
        // The user wants it adjusted: Po Labs has work again, on the same trial.
        assert!(book.revise("a lighter orange"));
        assert!(book.has_work() && !book.awaiting_verdict());
        assert!(book.trial_text().contains("a lighter orange"));
        book.start_trial(&[], "tool calls are light orange", "ignored", "c2", Visible::ToolCalls, None);
        let trial = book.trial.clone().unwrap();
        assert_eq!((trial.base.as_str(), trial.head.as_str(), trial.ids.clone()), ("base", "c2", vec![a]));
        assert_eq!(trial.previous.as_deref(), Some("po.old"), "the binary to go back to stays the first one");
        assert!(trial.revisions.is_empty());
        let kept = book.keep_trial().unwrap();
        assert_eq!(kept.head, "c2");
        assert!(book.trial.is_none() && book.has_work());
        // The next one is undone.
        book.start_trial(&[b], "branch shown", "c2", "c3", Visible::AtOnce, None);
        assert!(book.refuse_trial().is_some());
        assert_eq!(book.items.iter().find(|i| i.id == b).unwrap().status, Status::Refused);
        assert!(!book.has_work());
        assert!(!book.revise("x"), "nothing on trial to adjust");
    }

    #[test]
    fn shortcuts_of_one_to_four_keys() {
        let y = Shortcut::parse("Ctrl+Y").unwrap();
        assert!(y.matches(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)));
        assert!(!y.matches(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)));
        let combo = Shortcut::parse("ctrl+alt+shift+p").unwrap();
        assert!(combo.matches(&KeyEvent::new(KeyCode::Char('P'), KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)));
        assert_eq!(combo.label(), "Ctrl+Alt+Shift+P");
        assert_eq!(Shortcut::parse("f3").unwrap().code, KeyCode::F(3));
        assert!(Shortcut::parse("ctrl+alt+shift+meta+y").is_none(), "five keys");
        assert!(Shortcut::parse("hyper+y").is_none());
    }
}
