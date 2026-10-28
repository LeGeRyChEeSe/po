//! Po Labs on the interface side: the notebook of requests about Po, and the shortcuts.
//!
//! During a session, the model writes down what the user says about Po itself — a display
//! issue, a bug, an idea — without acting on it: the notebook. When Po is idle, a discreet
//! proposal offers to have Po Labs implement it, in a session of its own. The notebook is a
//! file, so that a request outlives the session it was made in, and it stays in English: it
//! feeds the public repository's issues.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Pending,
    /// Implemented by a Po Labs session.
    Done,
    /// Withdrawn by the user.
    Dropped,
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

    /// Marks every pending request done (a Po Labs session applied them).
    pub fn complete_pending(&mut self) -> usize {
        let mut n = 0;
        for item in self.items.iter_mut().filter(|i| i.status == Status::Pending) {
            item.status = Status::Done;
            n += 1;
        }
        if n > 0 {
            self.revision += 1;
        }
        // The next requests start a fresh session.
        self.session = None;
        n
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
        let before = book.revision;
        assert_eq!(book.complete_pending(), 1);
        assert!(book.revision > before);
        assert!(book.pending().is_empty());
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
