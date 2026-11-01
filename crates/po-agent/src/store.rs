//! Session persistence.
//!
//! An agent session represents real work: files read, decisions made, a budget spent. Losing
//! it because a terminal closed is a regression compared with any editor. We therefore save the
//! full history, the accounting and the budget mode, so that one can resume exactly where one
//! left off.
//!
//! Two choices guide the implementation:
//! - the format is readable, versioned JSON: a session must remain usable after an update, and
//!   inspectable by hand when something is off;
//! - writing goes through a renamed temporary file, so that an interruption at the wrong moment
//!   does not leave a truncated record in place of a valid one.

use std::path::{Path, PathBuf};

use po_core::{Ledger, Message};
use serde::{Deserialize, Serialize};

use crate::Session;

/// Format version. Incremented whenever an old session stops being readable as is; lets us
/// refuse cleanly rather than misinterpret.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}", io_text(.0))]
    Io(#[from] std::io::Error),
    #[error("{}", decode_text(.0))]
    Decode(#[from] serde_json::Error),
    #[error("{}", incompatible_text(.found, .expected))]
    Incompatible { found: u32, expected: u32 },
    #[error("{}", po_i18n::t!("agent.store.not_found"))]
    NotFound,
}

fn io_text(err: &std::io::Error) -> String {
    po_i18n::t!("agent.store.io", err = err)
}

fn decode_text(err: &serde_json::Error) -> String {
    po_i18n::t!("agent.store.decode", err = err)
}

fn incompatible_text(found: &u32, expected: &u32) -> String {
    po_i18n::t!("agent.store.incompatible", found = found, expected = expected)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub id: String,
    /// ISO 8601 timestamp, for sorting and display.
    pub saved_at: String,
    /// Working directory the session relates to: resuming a session in another project would
    /// make no sense, and the paths in the history would be misleading.
    pub workspace: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub ledger: Ledger,
    pub budget_mode: Option<String>,
    /// First line of the first request: used as the title in the list.
    pub title: String,
    /// Session settings. Absent from sessions saved before they existed.
    #[serde(default)]
    pub settings: crate::SessionSettings,
    /// What compaction removed, for `recall`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archive: Vec<Message>,
    /// The turn was paused when saved: it resumes from there, with its paused subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Vec<crate::PausedSubagent>>,
}

impl SavedSession {
    pub fn from_session(session: &Session, workspace: &Path, budget_mode: Option<&str>) -> Self {
        Self {
            version: FORMAT_VERSION,
            id: generate_id(),
            saved_at: timestamp(),
            workspace: workspace.display().to_string(),
            system: session.system.clone(),
            messages: session.messages.clone(),
            ledger: session.ledger.clone(),
            budget_mode: budget_mode.map(str::to_owned),
            title: derive_title(&session.messages),
            settings: crate::SessionSettings::default(),
            archive: session.archive.clone(),
            paused: None,
        }
    }

    pub fn with_settings(mut self, settings: crate::SessionSettings) -> Self {
        self.settings = settings;
        self
    }

    pub fn into_session(self) -> Session {
        let mut session = Session::new(self.system);
        session.messages = self.messages;
        session.ledger = self.ledger;
        session.archive = self.archive;
        session
    }

    /// Number of exchanges, for display in the list.
    pub fn exchange_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| message.role == po_core::Role::User && !message.content.is_empty())
            .count()
    }
}

/// Title derived from the user's first request.
fn derive_title(messages: &[Message]) -> String {
    let first = messages
        .iter()
        .find(|message| message.role == po_core::Role::User)
        .map(|message| crate::session::typed_text(&message.visible_text()))
        .unwrap_or_default();

    let line = first.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return po_i18n::t!("agent.store.untitled").into();
    }
    // Cut on characters, not bytes: an accented title must not cause a slice panic.
    let truncated: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        format!("{truncated}…")
    } else {
        truncated
    }
}

pub fn generate_id() -> String {
    // A readable, chronologically sorted identifier beats an opaque UUID when browsing the
    // directory by hand.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Conversion without a calendar dependency: enough to sort and display.
    let days = seconds / 86_400;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Converts days-since-1970 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Two spellings of one folder. On Windows the case and the separator do not matter, and the
/// shell keeps the folder as typed: `cd d:\projets\po` then `po -c` found none of the sessions
/// saved from `D:\Projets\Po`.
fn same_folder(a: &str, b: &str) -> bool {
    let tidy = |p: &str| p.trim_end_matches(['\\', '/']).replace('\\', "/");
    if cfg!(windows) { tidy(a).eq_ignore_ascii_case(&tidy(b)) } else { tidy(a) == tidy(b) }
}

/// Storage directory of the sessions.
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default location, in the user's data directory.
    pub fn default_location() -> Self {
        // `PO_SESSIONS_DIR` moves them (tests, a service tried on a copy of the sessions).
        if let Some(dir) = std::env::var_os("PO_SESSIONS_DIR").filter(|d| !d.is_empty()) {
            return Self::new(PathBuf::from(dir));
        }
        let root = dirs::data_dir()
            .map(|dir| dir.join("po").join("sessions"))
            .unwrap_or_else(|| PathBuf::from(".po/sessions"));
        Self::new(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Saves a session. The write is atomic: either the old version stays or the new one is
    /// complete, never a half-written file.
    pub fn save(&self, session: &SavedSession) -> Result<PathBuf, StoreError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!("{}.json", session.id));
        let temporary = path.with_extension("json.tmp");

        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, &path)?;
        Ok(path)
    }

    pub fn load(&self, id: &str) -> Result<SavedSession, StoreError> {
        let path = self.root.join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound
            } else {
                StoreError::Io(err)
            }
        })?;
        let saved: SavedSession = serde_json::from_str(&text)?;
        if saved.version > FORMAT_VERSION {
            return Err(StoreError::Incompatible {
                found: saved.version,
                expected: FORMAT_VERSION,
            });
        }
        Ok(saved)
    }

    /// Saved sessions, from newest to oldest.
    ///
    /// An unreadable file is skipped rather than fatal: a corrupted session must not prevent
    /// access to all the others.
    pub fn list(&self) -> Vec<SavedSession> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut sessions: Vec<SavedSession> = entries
            .flatten()
            .filter(|entry| {
                entry.path().extension().is_some_and(|ext| ext == "json")
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str::<SavedSession>(&text).ok())
            .filter(|saved| saved.version <= FORMAT_VERSION)
            .collect();

        sessions.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then(b.id.cmp(&a.id)));
        sessions
    }

    /// Sessions attached to a given working directory.
    pub fn list_for(&self, workspace: &Path) -> Vec<SavedSession> {
        let target = workspace.display().to_string();
        self.list().into_iter().filter(|saved| same_folder(&saved.workspace, &target)).collect()
    }

    /// The most recent session of the current directory — what `--continue` resumes.
    pub fn latest_for(&self, workspace: &Path) -> Option<SavedSession> {
        self.list_for(workspace).into_iter().next()
    }

    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        std::fs::remove_file(self.root.join(format!("{id}.json")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::{ContentBlock, Pricing, Role, Usage};

    fn sample_session() -> Session {
        let mut session = Session::new("system instruction");
        session.push(Message::user("Refactor the rendering module"));
        session.push(Message::assistant("Okay, taking a look."));
        session.record(
            "opus",
            Usage { input_tokens: 100, output_tokens: 20, ..Default::default() },
            &Pricing { input_per_mtok: 15.0, output_per_mtok: 75.0, ..Default::default() },
            0.0,
        );
        session
    }

    #[test]
    fn a_session_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let saved = SavedSession::from_session(&sample_session(), Path::new("/project"), Some("balanced"));

        store.save(&saved).unwrap();
        let reloaded = store.load(&saved.id).unwrap();

        assert_eq!(reloaded.messages.len(), 2);
        assert_eq!(reloaded.system, "system instruction");
        assert_eq!(reloaded.budget_mode.as_deref(), Some("balanced"));
        assert_eq!(reloaded.ledger.total_tokens(), 120);

        let session = reloaded.into_session();
        assert_eq!(session.messages[0].visible_text(), "Refactor the rendering module");
        assert!(session.ledger.total_cost() > 0.0, "the accounting must be restored");
    }

    #[test]
    fn tool_calls_survive_persistence() {
        // This is the most fragile content: a lost identifier would break resumption.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());

        let mut session = Session::new("sys");
        session.push(Message::user("read a.rs"));
        session.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_42".into(),
                name: "read".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
        });
        session.push(Message::tool_results(vec![ContentBlock::tool_result("call_42", "content")]));

        let saved = SavedSession::from_session(&session, Path::new("/p"), None);
        store.save(&saved).unwrap();
        let reloaded = store.load(&saved.id).unwrap().into_session();

        let (id, name, _) = reloaded.messages[1].tool_uses().next().unwrap();
        assert_eq!(id, "call_42");
        assert_eq!(name, "read");
        match &reloaded.messages[2].content[0] {
            ContentBlock::ToolResult { tool_use_id, .. } => assert_eq!(tool_use_id, "call_42"),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn the_title_comes_from_the_first_request() {
        let saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        assert_eq!(saved.title, "Refactor the rendering module");
        assert_eq!(saved.exchange_count(), 1);
    }

    #[test]
    fn a_long_title_is_cut_without_breaking_utf8() {
        let mut session = Session::new("");
        session.push(Message::user("é".repeat(200)));
        let saved = SavedSession::from_session(&session, Path::new("/p"), None);
        assert!(saved.title.chars().count() <= 61);
        assert!(saved.title.ends_with('…'));
    }

    #[test]
    fn a_session_without_a_request_has_a_fallback_title() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let saved = SavedSession::from_session(&Session::new(""), Path::new("/p"), None);
        assert_eq!(saved.title, "untitled session");
    }

    #[test]
    fn sessions_are_listed_from_newest_to_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());

        for index in 0..3 {
            let mut saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
            saved.id = format!("id{index}");
            saved.saved_at = format!("2026-08-2{index}T10:00:00Z");
            store.save(&saved).unwrap();
        }
        let listed = store.list();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, "id2");
        assert_eq!(listed[2].id, "id0");
    }

    #[test]
    fn sessions_are_filtered_by_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());

        let mut here = SavedSession::from_session(&sample_session(), Path::new("/project-a"), None);
        here.id = "a".into();
        let mut elsewhere = SavedSession::from_session(&sample_session(), Path::new("/project-b"), None);
        elsewhere.id = "b".into();
        store.save(&here).unwrap();
        store.save(&elsewhere).unwrap();

        let found = store.list_for(Path::new("/project-a"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "a");
        assert_eq!(store.latest_for(Path::new("/project-a")).unwrap().id, "a");
        assert!(store.latest_for(Path::new("/unknown-project")).is_none());
    }

    #[test]
    fn a_folder_typed_in_another_case_finds_its_sessions_on_windows() {
        assert!(same_folder(r"D:\Projets\Po", r"D:\Projets\Po\"));
        assert_eq!(same_folder(r"D:\Projets\Po", r"d:\projets\po"), cfg!(windows));
        assert!(!same_folder(r"D:\Projets\Po", r"D:\Projets\Pomme"));
    }

    #[test]
    fn a_corrupted_file_does_not_prevent_reading_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        store.save(&saved).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ this is not json").unwrap();

        assert_eq!(store.list().len(), 1, "the valid session must remain accessible");
    }

    #[test]
    fn a_future_version_is_explicitly_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let mut saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        saved.version = FORMAT_VERSION + 1;
        store.save(&saved).unwrap();

        // Better to refuse clearly than to misread a newer session.
        assert!(matches!(store.load(&saved.id), Err(StoreError::Incompatible { .. })));
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_missing_session_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        assert!(matches!(store.load("nonexistent"), Err(StoreError::NotFound)));
    }

    #[test]
    fn writing_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        store.save(&saved).unwrap();

        let temporaries = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporaries, 0);
    }

    #[test]
    fn a_session_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let saved = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        store.save(&saved).unwrap();
        store.delete(&saved.id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn the_timestamp_is_a_plausible_iso_date() {
        let stamp = timestamp();
        assert!(stamp.ends_with('Z'));
        assert_eq!(stamp.len(), 20);
        // The date must be later than the writing of this test.
        assert!(stamp.starts_with("202") || stamp.starts_with("203"));
    }

    #[test]
    fn date_conversion_is_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // Leap year: February 29 must exist.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn two_sessions_created_back_to_back_have_distinct_identifiers() {
        let a = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = SavedSession::from_session(&sample_session(), Path::new("/p"), None);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn settings_travel_with_the_session_and_old_ones_stay_readable() {
        let mut session = Session::new("sys");
        session.push(po_core::Message::user("hello"));
        let settings = crate::SessionSettings {
            reasoning_effort: Some("low".into()),
            temperature: Some(0.2),
            criteria: vec!["compile".into()],
            show_thinking: true,
            thinking_budget: Some(4096),
            work_mode: Some("bypass".into()),
            model: Some("qwen".into()),
            last_model: Some("qwen".into()),
        };
        let saved = SavedSession::from_session(&session, Path::new("/tmp"), None).with_settings(settings.clone());
        let text = serde_json::to_string(&saved).unwrap();
        let reread: SavedSession = serde_json::from_str(&text).unwrap();
        assert_eq!(reread.settings, settings);

        // A session saved before settings existed is read back without error.
        let mut old: serde_json::Value = serde_json::from_str(&text).unwrap();
        old.as_object_mut().unwrap().remove("settings");
        let reread: SavedSession = serde_json::from_value(old).unwrap();
        assert_eq!(reread.settings, crate::SessionSettings::default());
    }
}
