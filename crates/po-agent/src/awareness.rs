//! Po's perception of its own session: what the model is told about the harness around it.
//!
//! Po is the body and the senses, the model is the judgment. What Po can know for certain (the
//! context filling up, a tool failing the same way twice, the phone leaving, a question the model
//! set aside) is computed here as typed facts, with no free text, and handed to the model as a
//! few short lines. An error of judgment on the model's side then costs nothing: it only reads.
//!
//! The state travels like the work mode does (see `Agent::turn`): ahead of the user's message,
//! and only what changed since it was last announced. Local models live on their server's prefix
//! cache, so nothing that varies may enter the system prompt; and a state repeated at every turn
//! would cost tokens for nothing.

use std::sync::Arc;

use async_trait::async_trait;

/// The interface the user is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    Terminal,
    /// A session opened from the phone: the PC runs it without a screen.
    Phone,
    /// Nobody to answer: a single prompt, a script.
    Headless,
}

impl Interface {
    fn label(self) -> &'static str {
        match self {
            Interface::Terminal => "terminal",
            Interface::Phone => "phone",
            Interface::Headless => "headless (no one to ask: decide alone)",
        }
    }
}

/// What only the host (the terminal, the service behind the phone) can perceive.
#[async_trait]
pub trait Senses: Send + Sync {
    fn interface(&self) -> Interface;
    /// Whether a phone follows this session now.
    fn phone_connected(&self) -> bool;
    /// The models loaded on this PC other than `own`, the session's. The host caches the answer:
    /// asking the servers at every turn would delay the model for nothing.
    async fn other_models(&self, own: &str) -> Vec<String>;
    /// Lines about snags that keep coming back with this model, from the host's journal. Each
    /// line starts with its own stable key (`snag[edit]: ...`) so that [`diff`] tells them apart.
    fn friction_hints(&self, _model: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A question the model set aside (the user did not answer it), and where its reminders stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    pub question: String,
    /// Turns begun since it was set aside.
    pub turns: u32,
    pub reminders: u32,
    /// Turns since the last reminder.
    since_reminder: u32,
}

/// The reminders of a set-aside question: the next turn, then every third, at most three times.
pub const REMINDERS: u32 = 3;
const REMINDER_EVERY: u32 = 3;

impl Deferred {
    pub fn new(question: impl Into<String>) -> Self {
        Self { question: question.into(), turns: 0, reminders: 0, since_reminder: 0 }
    }

    /// A turn begins: time passes. Returns whether a reminder is due now.
    pub fn tick(&mut self) -> bool {
        self.turns += 1;
        self.since_reminder += 1;
        let due = self.reminders == 0 || self.since_reminder >= REMINDER_EVERY;
        if due {
            self.reminders += 1;
            self.since_reminder = 0;
        }
        due
    }

    /// Reminded as many times as it will be.
    pub fn exhausted(&self) -> bool {
        self.reminders >= REMINDERS
    }
}

/// Context use worth announcing, in percent of the window: below the first, nothing is said.
const CONTEXT_STEPS: [u8; 3] = [60, 80, 90];
/// Silence from the user that is worth mentioning, in minutes.
const AWAY_MINUTES: u64 = 10;
/// Failures of one tool with one signature before it is called repeated.
const REPEATED_FAILURES: usize = 2;

/// The step of the context gauge reached, if any.
pub fn context_step(used: u32, window: u32) -> Option<u8> {
    if window == 0 {
        return None;
    }
    let pct = (u64::from(used) * 100 / u64::from(window)).min(100) as u8;
    CONTEXT_STEPS.iter().rev().copied().find(|step| pct >= *step)
}

/// A tool failure as the loop remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub tool: String,
    pub signature: String,
    pub turn: u32,
}

/// The same tool failing with the same signature at least twice, in this turn or the previous
/// one: one `(tool, signature)` per tool, the most frequent.
pub fn repeated_failures(failures: &[Failure], turn: u32) -> Vec<(String, String)> {
    let recent = || failures.iter().filter(|f| f.turn + 1 >= turn);
    let mut out: Vec<(String, String, usize)> = Vec::new();
    for f in recent() {
        if out.iter().any(|(t, s, _)| *t == f.tool && *s == f.signature) {
            continue;
        }
        let count = recent().filter(|g| g.tool == f.tool && g.signature == f.signature).count();
        if count >= REPEATED_FAILURES {
            match out.iter_mut().find(|(t, _, _)| *t == f.tool) {
                Some(known) if known.2 >= count => {}
                Some(known) => *known = (f.tool.clone(), f.signature.clone(), count),
                None => out.push((f.tool.clone(), f.signature.clone(), count)),
            }
        }
    }
    out.into_iter().map(|(tool, signature, _)| (tool, signature)).collect()
}

/// What Po perceives, as facts. Nothing here is free text from the model or the user, except the
/// text of a set-aside question, which the model wrote itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfState {
    pub mode: String,
    pub interface: Option<Interface>,
    pub phone_connected: Option<bool>,
    pub other_models: Vec<String>,
    /// [`context_step`] of the request.
    pub context_step: Option<u8>,
    pub background: usize,
    pub failing: Vec<(String, String)>,
    pub refused_files: Vec<String>,
    pub hints: Vec<String>,
    /// Set-aside questions due for a reminder this turn, with the turns since: an event, not a
    /// lasting fact, so never part of the diff.
    pub reminders: Vec<(String, u32)>,
    /// Minutes the user was away before this message, if worth saying: an event too.
    pub away_minutes: Option<u64>,
}

impl SelfState {
    /// The lasting facts, one line each, each starting with its stable key before the first
    /// colon.
    pub fn render(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.mode.is_empty() {
            lines.push(format!("mode: {}", self.mode));
        }
        if let Some(interface) = self.interface {
            lines.push(format!("interface: {}", interface.label()));
        }
        if let Some(connected) = self.phone_connected {
            lines.push(format!("phone: {}", if connected { "connected" } else { "not connected" }));
        }
        if !self.other_models.is_empty() {
            let shown: Vec<&str> = self.other_models.iter().take(4).map(String::as_str).collect();
            lines.push(format!("loaded elsewhere on this PC: {}", shown.join(", ")));
        }
        if let Some(step) = self.context_step {
            lines.push(match step {
                90.. => format!("context: over {step}% full, compaction is imminent"),
                _ => format!("context: over {step}% full"),
            });
        }
        if self.background > 0 {
            lines.push(format!("background: {} subagent(s) running", self.background));
        }
        for (tool, signature) in self.failing.iter().take(3) {
            lines.push(format!("failing[{tool}]: repeated \"{signature}\" - change approach before retrying"));
        }
        if !self.refused_files.is_empty() {
            let shown: Vec<&str> = self.refused_files.iter().take(5).map(String::as_str).collect();
            lines.push(format!("refused files: {} (outside the workspace)", shown.join(", ")));
        }
        lines.extend(self.hints.iter().take(3).cloned());
        lines
    }

    /// The event lines of this turn (not diffed: they are due now, and only now).
    pub fn events(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(minutes) = self.away_minutes {
            lines.push(format!("away: the user was gone {minutes} min before this message"));
        }
        for (question, turns) in &self.reminders {
            lines.push(format!(
                "set aside: you left \"{question}\" unanswered ({turns} turn{} ago) - ask again when it matters, or drop it",
                if *turns == 1 { "" } else { "s" }
            ));
        }
        lines
    }

    /// Whether the user was away long enough to be worth saying.
    pub fn away_for(elapsed: std::time::Duration) -> Option<u64> {
        let minutes = elapsed.as_secs() / 60;
        (minutes >= AWAY_MINUTES).then_some(minutes)
    }
}

/// The key of a rendered line: what stands before its first colon.
pub fn key_of(line: &str) -> &str {
    line.split_once(':').map_or(line, |(key, _)| key)
}

/// What changed from `prev` to `next`: the lines that are new or reworded, and a "no longer" for
/// each fact that is gone. Nothing changed, nothing said.
pub fn diff(prev: &[String], next: &[String]) -> Vec<String> {
    let mut out: Vec<String> = next.iter().filter(|line| !prev.contains(line)).cloned().collect();
    for old in prev {
        let key = key_of(old);
        if !next.iter().any(|line| key_of(line) == key) {
            out.push(format!("no longer: {key}"));
        }
    }
    out
}

/// The header of the block the model reads: it must take it for a perception, not an order.
pub const HEADER: &str = "[Po state] what Po perceives, not a request from the user:";

/// The block put ahead of the user's message, or `None` when there is nothing to say.
pub fn block(lines: &[String]) -> Option<String> {
    (!lines.is_empty()).then(|| format!("{HEADER}\n{}", lines.join("\n")))
}

/// A failure's signature: the error with what varies from one occurrence to the next replaced
/// (paths, numbers, identifiers), so that "file 12.txt not found" and "file 7.txt not found" are
/// one snag. First line only, bounded.
pub fn normalize_signature(error: &str) -> String {
    let first = error.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let words: Vec<String> = first.split_whitespace().map(normalize_word).collect();
    let mut text = words.join(" ");
    if text.chars().count() > 80 {
        text = text.chars().take(80).collect::<String>() + "...";
    }
    text
}

fn normalize_word(word: &str) -> String {
    let edge = |c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '.');
    let core = word.trim_matches(edge);
    if core.is_empty() {
        return word.to_owned();
    }
    let lead = &word[..word.find(core).unwrap_or(0)];
    let tail = &word[lead.len() + core.len()..];
    let bytes = core.as_bytes();
    let windows_drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    // `notes.txt`, `lib.rs`: a bare file name varies as much as a full path does.
    let file_name = core.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    let replacement = if core.contains(['/', '\\']) || windows_drive || file_name {
        Some("<path>")
    } else if core.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | ',')) && core.chars().any(|c| c.is_ascii_digit()) {
        Some("<n>")
    } else if core.len() >= 8 && core.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && core.chars().any(|c| c.is_ascii_digit()) {
        Some("<id>")
    } else if core.chars().any(|c| c.is_ascii_digit()) && core.chars().any(|c| c.is_ascii_alphabetic()) && core.len() >= 6 {
        // `call_8f3a`, `abc123def`: an identifier, not a word of the message.
        Some("<id>")
    } else {
        None
    };
    match replacement {
        Some(r) => format!("{lead}{r}{tail}"),
        None => word.to_owned(),
    }
}

/// What kind of snag a friction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrictionKind {
    ToolError,
    InvalidArgs,
    Denied,
    OutsideWrite,
    LoopWarning,
    Interrupted,
    OutputTruncated,
    ModelLoadFailed,
    ContextOverflow,
    QuestionSetAside,
}

impl FrictionKind {
    pub const ALL: [FrictionKind; 10] = [
        FrictionKind::ToolError,
        FrictionKind::InvalidArgs,
        FrictionKind::Denied,
        FrictionKind::OutsideWrite,
        FrictionKind::LoopWarning,
        FrictionKind::Interrupted,
        FrictionKind::OutputTruncated,
        FrictionKind::ModelLoadFailed,
        FrictionKind::ContextOverflow,
        FrictionKind::QuestionSetAside,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FrictionKind::ToolError => "tool_error",
            FrictionKind::InvalidArgs => "invalid_args",
            FrictionKind::Denied => "denied",
            FrictionKind::OutsideWrite => "outside_write",
            FrictionKind::LoopWarning => "loop_warning",
            FrictionKind::Interrupted => "interrupted",
            FrictionKind::OutputTruncated => "output_truncated",
            FrictionKind::ModelLoadFailed => "model_load_failed",
            FrictionKind::ContextOverflow => "context_overflow",
            FrictionKind::QuestionSetAside => "question_set_aside",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

/// A snag the session met, to be written down by the host: no disk here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Friction {
    pub kind: FrictionKind,
    /// What makes two occurrences the same snag (a tool and its normalized signature).
    pub key: String,
    /// A few words for the human reading the journal; never file contents.
    pub detail: String,
    /// The model of the session when it happened.
    pub model: String,
    pub session: Option<String>,
}

/// Where the host receives the frictions.
pub type FrictionRecorder = Arc<dyn Fn(&Friction) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn nothing_is_announced_when_nothing_changed() {
        let state = SelfState { mode: "build".into(), interface: Some(Interface::Terminal), ..Default::default() };
        let rendered = state.render();
        assert!(diff(&rendered, &rendered).is_empty());
        assert_eq!(block(&diff(&rendered, &rendered)), None);
    }

    #[test]
    fn a_changed_fact_is_announced_and_a_vanished_one_once() {
        let before = lines(&["phone: connected", "context: over 60% full"]);
        let after = lines(&["phone: not connected"]);
        let said = diff(&before, &after);
        assert_eq!(said, ["phone: not connected", "no longer: context"]);
        // Announced: the next turn compares with what was announced, and has nothing to add.
        assert!(diff(&after, &after).is_empty());
    }

    #[test]
    fn the_first_state_is_all_new() {
        let state = SelfState { mode: "plan".into(), phone_connected: Some(true), ..Default::default() };
        let said = diff(&[], &state.render());
        assert_eq!(said, ["mode: plan", "phone: connected"]);
        assert!(block(&said).unwrap().starts_with(HEADER));
    }

    #[test]
    fn the_context_is_reported_past_its_steps_only() {
        assert_eq!(context_step(5_000, 10_000), None);
        assert_eq!(context_step(5_999, 10_000), None);
        assert_eq!(context_step(6_000, 10_000), Some(60));
        assert_eq!(context_step(8_500, 10_000), Some(80));
        assert_eq!(context_step(9_000, 10_000), Some(90));
        assert_eq!(context_step(20_000, 10_000), Some(90));
        assert_eq!(context_step(1, 0), None);
    }

    #[test]
    fn a_context_step_crossed_is_one_new_line() {
        let at = |used| SelfState { context_step: context_step(used, 100), ..Default::default() }.render();
        assert!(diff(&at(59), &at(59)).is_empty());
        assert_eq!(diff(&at(59), &at(61)), ["context: over 60% full"]);
        assert!(diff(&at(61), &at(70)).is_empty(), "same step: nothing new");
        assert_eq!(diff(&at(70), &at(81)), ["context: over 80% full"]);
    }

    #[test]
    fn a_set_aside_question_is_reminded_next_turn_then_every_third_at_most_thrice() {
        let mut deferred = Deferred::new("Which database?");
        let mut due = Vec::new();
        for turn in 1..=12 {
            if deferred.tick() {
                due.push(turn);
            }
            if deferred.exhausted() {
                break;
            }
        }
        assert_eq!(due, [1, 4, 7]);
        assert!(deferred.exhausted());
    }

    #[test]
    fn a_reminder_states_the_question_and_its_age() {
        let state = SelfState { reminders: vec![("Which database?".into(), 4)], ..Default::default() };
        let events = state.events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("Which database?") && events[0].contains("4 turns ago"));
        let one = SelfState { reminders: vec![("Q?".into(), 1)], ..Default::default() };
        assert!(one.events()[0].contains("1 turn ago"));
    }

    #[test]
    fn the_user_away_is_an_event_past_ten_minutes() {
        use std::time::Duration;
        assert_eq!(SelfState::away_for(Duration::from_secs(9 * 60)), None);
        assert_eq!(SelfState::away_for(Duration::from_secs(25 * 60)), Some(25));
        let state = SelfState { away_minutes: Some(25), ..Default::default() };
        assert!(state.events()[0].contains("25 min"));
        assert!(state.render().is_empty(), "an event is not a lasting fact");
    }

    fn failure(tool: &str, signature: &str, turn: u32) -> Failure {
        Failure { tool: tool.into(), signature: signature.into(), turn }
    }

    #[test]
    fn a_failure_is_repeated_from_two_occurrences_over_this_turn_and_the_last() {
        let log = [failure("edit", "old not found", 4), failure("edit", "old not found", 5), failure("read", "missing", 5)];
        assert_eq!(repeated_failures(&log, 5), [("edit".to_owned(), "old not found".to_owned())]);
        // Two turns later the first has aged out.
        assert!(repeated_failures(&log, 7).is_empty());
        // A different signature is a different snag.
        let mixed = [failure("edit", "a", 5), failure("edit", "b", 5)];
        assert!(repeated_failures(&mixed, 5).is_empty());
    }

    #[test]
    fn signatures_drop_what_varies() {
        assert_eq!(normalize_signature("file C:\\Users\\me\\a.txt not found"), "file <path> not found");
        assert_eq!(normalize_signature("cannot open /home/x/y.rs: no such file"), "cannot open <path>: no such file");
        assert_eq!(normalize_signature("notes.txt: file not found"), "<path>: file not found");
        assert_eq!(normalize_signature("old_string not found at line 42"), "old_string not found at line <n>");
        assert_eq!(normalize_signature("call_8f3a1b2c failed with 3 errors"), "<id> failed with <n> errors");
        assert_eq!(normalize_signature("timed out after 120 s\nsecond line"), "timed out after <n> s");
        // Two occurrences differing only by what varies are one signature.
        assert_eq!(normalize_signature("no match in src/a.rs (3 hits)"), normalize_signature("no match in src/b.rs (7 hits)"));
        assert!(normalize_signature(&"word ".repeat(100)).chars().count() <= 83);
    }

    #[test]
    fn friction_kinds_round_trip() {
        for kind in FrictionKind::ALL {
            assert_eq!(FrictionKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(FrictionKind::parse("nope"), None);
    }

    #[test]
    fn the_rendered_state_stays_short() {
        let state = SelfState {
            mode: "auto".into(),
            interface: Some(Interface::Phone),
            phone_connected: Some(true),
            other_models: vec!["Qwen3.5-9B-Q4_K_M".into()],
            context_step: Some(80),
            background: 2,
            failing: vec![("edit".into(), "old_string not found".into())],
            refused_files: vec!["secrets.txt".into()],
            ..Default::default()
        };
        let text = block(&state.render()).unwrap();
        // About 120 tokens at most: a rough four characters per token.
        assert!(text.len() < 520, "{} chars: {text}", text.len());
    }
}
