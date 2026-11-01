//! Live tracking of subagents, in a frame apart from the conversation.
//!
//! A subagent sometimes works for a long time, and sometimes while the main agent keeps
//! answering (background task). The small frame only shows what it says it is doing, at a
//! glance: its "Progress: …" lines. Its full conversation — reasoning, text, tools — is built
//! alongside, with the same lines as the main agent's, for the big card.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use po_agent::SubagentUpdate;

use crate::state::{Line, LineKind, Transcript};

/// How long a finished subagent stays displayed.
const LINGER: Duration = Duration::from_secs(8);

#[derive(Debug)]
pub struct SubagentView {
    pub id: String,
    pub agent: String,
    pub model: String,
    /// Full brief.
    pub task: String,
    /// Last progress line written by the subagent ("Progress: …").
    pub progress: Option<String>,
    pub background: bool,
    /// Color of its card, stable while it is displayed: a subagent is recognized by its color
    /// in the stack as well as in the big card.
    pub color: usize,
    pub started: Instant,
    /// End and success.
    pub finished: Option<(Instant, bool)>,
    /// Paused since then; the time spent paused does not count in its duration.
    pub paused: Option<Instant>,
    paused_for: Duration,
    /// Its conversation, laid out like the main agent's in the big card.
    pub transcript: Transcript,
    /// Blocks toggled by touch in the big card, and the block to keep under the finger.
    pub toggled: HashSet<usize>,
    pub anchor: std::cell::Cell<Option<(usize, u16)>>,
    /// Text line being written, and whether it is already on screen.
    partial: String,
    partial_shown: bool,
    /// Reasoning in progress: position of its header, start, volume, unfinished line.
    thinking: Option<(usize, Instant, usize)>,
    thinking_partial: String,
    /// Tools started: name, position of their line, start.
    running: Vec<(String, usize, Instant)>,
}

impl SubagentView {
    fn new(id: String, agent: String, model: String, task: String, background: bool, color: usize) -> Self {
        let mut transcript = Transcript::default();
        // The brief opens its conversation, collapsed: a touch expands it.
        let brief = if id == crate::LABS_VIEW { po_i18n::t!("tui.monitor.labs_brief") } else { po_i18n::t!("tui.monitor.brief") };
        transcript.push(Line::new(LineKind::Delegation, brief).with_detail(task.clone()));
        Self {
            id,
            agent,
            model,
            task,
            progress: None,
            background,
            color,
            started: Instant::now(),
            finished: None,
            paused: None,
            paused_for: Duration::ZERO,
            transcript,
            toggled: HashSet::new(),
            anchor: std::cell::Cell::new(None),
            partial: String::new(),
            partial_shown: false,
            thinking: None,
            thinking_partial: String::new(),
            running: Vec::new(),
        }
    }

    /// Time spent working: up to its end, or up to its pause, without the time spent paused.
    pub fn elapsed(&self) -> Duration {
        let until = self.finished.map(|(at, _)| at).or(self.paused).unwrap_or_else(Instant::now);
        until.duration_since(self.started).saturating_sub(self.paused_for)
    }

    /// Still working: neither finished nor paused.
    pub fn running(&self) -> bool {
        self.finished.is_none() && self.paused.is_none()
    }

    /// What the small frame says, and the subagent's line in the main conversation.
    pub fn status_line(&self) -> String {
        if self.finished.is_none() && self.paused.is_some() {
            return po_i18n::t!("tui.monitor.paused").to_owned();
        }
        match self.finished {
            Some((_, ok)) => {
                let secs = self.elapsed().as_secs();
                if ok {
                    po_i18n::t!("tui.monitor.done_in", secs = secs)
                } else {
                    po_i18n::t!("tui.monitor.stopped_after", secs = secs)
                }
            }
            None => self.progress.clone().unwrap_or_else(|| po_i18n::t!("tui.monitor.preparing").to_owned()),
        }
    }

    /// Still on screen: running, or finished a short while ago.
    pub fn visible(&self, now: Instant) -> bool {
        self.finished.is_none_or(|(at, _)| now.duration_since(at) < LINGER)
    }

    fn think(&mut self, text: &str) {
        if !self.partial.is_empty() {
            self.end_text_line();
        }
        if self.thinking.is_none() {
            let position = self.transcript.next_position();
            self.transcript.push(Line::new(LineKind::ThinkingSummary, po_i18n::t!("tui.thinking.ellipsis")));
            self.transcript.push(Line::new(LineKind::Thinking, String::new()));
            self.thinking = Some((position, Instant::now(), 0));
            self.thinking_partial.clear();
        }
        if let Some((_, _, chars)) = self.thinking.as_mut() {
            *chars += text.chars().count();
        }
        for c in text.chars() {
            if c == '\n' {
                self.transcript.replace_last(std::mem::take(&mut self.thinking_partial));
                self.transcript.push(Line::new(LineKind::Thinking, String::new()));
            } else {
                self.thinking_partial.push(c);
            }
        }
        self.transcript.replace_last(self.thinking_partial.clone());
    }

    /// Closes the reasoning in progress: its header gets duration and volume.
    fn end_thinking(&mut self) {
        let Some((position, started, chars)) = self.thinking.take() else {
            return;
        };
        let secs = started.elapsed().as_secs_f64();
        let secs = crate::seconds_label(secs);
        self.transcript.replace_at(
            position,
            Line::new(LineKind::ThinkingSummary, po_i18n::t!("tui.monitor.thought", secs = secs, tokens = chars.div_ceil(4))),
        );
        self.thinking_partial.clear();
    }

    fn write(&mut self, text: &str) {
        self.end_thinking();
        for c in text.chars() {
            if c == '\n' {
                self.end_text_line();
            } else {
                self.partial.push(c);
            }
        }
        // A line that could become "Progress: …" (or "En cours : …") waits to be complete: it goes
        // to the small frame, not into the conversation.
        if !self.partial.is_empty() && !may_be_progress(&self.partial) {
            self.show_partial();
        }
    }

    fn show_partial(&mut self) {
        if self.partial_shown {
            self.transcript.replace_last(self.partial.clone());
        } else {
            self.transcript.push(Line::new(LineKind::Assistant, self.partial.clone()));
            self.partial_shown = true;
        }
    }

    /// Ends the text line in progress.
    fn end_text_line(&mut self) {
        let line = std::mem::take(&mut self.partial);
        if let Some(progress) = po_agent::progress_line(&line).filter(|_| !self.partial_shown) {
            self.progress = Some(progress.to_owned());
        } else if self.partial_shown {
            self.transcript.replace_last(line);
        } else if !line.trim().is_empty() || self.last_is_text() {
            self.transcript.push(Line::new(LineKind::Assistant, line));
        }
        self.partial_shown = false;
    }

    fn last_is_text(&self) -> bool {
        self.transcript.lines().last().is_some_and(|l| l.kind == LineKind::Assistant && !l.text.trim().is_empty())
    }

    fn tool(&mut self, name: String, summary: String) {
        self.end_thinking();
        if !self.partial.is_empty() {
            self.end_text_line();
        }
        let position = self.transcript.next_position();
        self.transcript.push(Line::new(LineKind::ToolRunning, format!("{name} {summary}")));
        self.running.push((name, position, Instant::now()));
    }

    fn tool_done(&mut self, name: &str, ok: bool) {
        let Some(index) = self.running.iter().position(|(n, ..)| n == name) else {
            return;
        };
        let (_, position, started) = self.running.remove(index);
        let text = self.transcript.get(position).map(|l| l.text.clone()).unwrap_or_else(|| name.to_owned());
        let kind = if ok { LineKind::Tool } else { LineKind::ToolError };
        self.transcript.replace_at(position, Line::new(kind, text).with_secs(started.elapsed().as_secs_f64()));
    }
}

/// Start of a line that can still become a progress line.
fn may_be_progress(partial: &str) -> bool {
    let start = partial.trim_start_matches(['*', '_', '-', '>', ' ']).to_lowercase();
    ["progress", "en cours"].iter().any(|target| {
        if start.chars().count() < target.chars().count() { target.starts_with(&start) } else { start.starts_with(target) }
    })
}

#[derive(Debug, Default)]
pub struct Monitor {
    pub views: Vec<SubagentView>,
    /// Color of the next subagent: they rotate, so two neighboring cards differ.
    next_color: usize,
}

impl Monitor {
    pub fn apply(&mut self, update: SubagentUpdate) {
        // The phone follows the subagents too.
        crate::control::mirror().subagent(&update);
        match update {
            SubagentUpdate::Started { id, agent, model, task, background } => {
                // A paused subagent resumes in its own card, its conversation kept.
                if let Some(view) = self.find(&id)
                    && let Some(since) = view.paused.take()
                    && view.finished.is_none()
                {
                    view.paused_for += since.elapsed();
                    return;
                }
                self.views.retain(|v| v.id != id);
                let color = self.next_color;
                self.next_color += 1;
                self.views.push(SubagentView::new(id, agent, model, task, background, color));
            }
            SubagentUpdate::Thinking { id, text } => {
                if let Some(view) = self.find(&id) {
                    view.think(&text);
                }
            }
            SubagentUpdate::Text { id, text } => {
                if let Some(view) = self.find(&id) {
                    view.write(&text);
                }
            }
            SubagentUpdate::Tool { id, name, summary } => {
                if let Some(view) = self.find(&id) {
                    view.tool(name, summary);
                }
            }
            SubagentUpdate::ToolDone { id, name, ok } => {
                if let Some(view) = self.find(&id) {
                    view.tool_done(&name, ok);
                }
            }
            SubagentUpdate::Paused { id } => {
                if let Some(view) = self.find(&id) {
                    view.end_thinking();
                    if !view.partial.is_empty() {
                        view.end_text_line();
                    }
                    view.paused = Some(Instant::now());
                }
            }
            SubagentUpdate::Finished { id, ok } => {
                if let Some(view) = self.find(&id) {
                    view.end_thinking();
                    if !view.partial.is_empty() {
                        view.end_text_line();
                    }
                    if let Some(since) = view.paused.take() {
                        view.paused_for += since.elapsed();
                    }
                    view.finished = Some((Instant::now(), ok));
                }
            }
        }
    }

    /// What a card says it is doing, set from outside (Po Labs working alongside).
    pub fn set_progress(&mut self, id: &str, text: &str) {
        if let Some(view) = self.find(id)
            && view.progress.as_deref() != Some(text)
        {
            view.progress = Some(text.to_owned());
            crate::control::mirror()
                .push(po_wire::Event::Subagent { update: Box::new(po_wire::SubagentEvent::Progress { id: id.to_owned(), text: text.to_owned() }) });
        }
    }

    /// Marks every still-running subagent as stopped (stop requested with Esc). Po Labs working
    /// alongside is not the turn's: it goes on.
    pub fn stop_all(&mut self) {
        let now = Instant::now();
        for view in self.views.iter_mut().filter(|v| v.finished.is_none() && v.id != crate::LABS_VIEW) {
            view.finished = Some((now, false));
        }
    }

    /// Marks every running subagent as paused (its turn was stopped, its work kept): it takes up
    /// in its own card when the turn goes on.
    pub fn pause_running(&mut self) {
        let running: Vec<String> = self.views.iter().filter(|v| v.running() && v.id != crate::LABS_VIEW).map(|v| v.id.clone()).collect();
        for id in running {
            self.apply(SubagentUpdate::Paused { id });
        }
    }

    pub fn find(&mut self, id: &str) -> Option<&mut SubagentView> {
        self.views.iter_mut().find(|v| v.id == id)
    }

    /// Subagents to display; old ones disappear.
    pub fn visible(&mut self) -> Vec<&SubagentView> {
        let now = Instant::now();
        self.views.retain(|v| v.visible(now));
        self.views.iter().collect()
    }

    pub fn any_visible(&self) -> bool {
        let now = Instant::now();
        self.views.iter().any(|v| v.visible(now))
    }
}

/// Identifier of the subagent an update concerns.
pub fn update_id(update: &SubagentUpdate) -> &str {
    match update {
        SubagentUpdate::Started { id, .. }
        | SubagentUpdate::Thinking { id, .. }
        | SubagentUpdate::Text { id, .. }
        | SubagentUpdate::Tool { id, .. }
        | SubagentUpdate::ToolDone { id, .. }
        | SubagentUpdate::Paused { id }
        | SubagentUpdate::Finished { id, .. } => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(m: &mut Monitor) {
        m.apply(SubagentUpdate::Started {
            id: "t1".into(),
            agent: "explorer".into(),
            model: "qwen".into(),
            task: "Explore the project. Then report back.".into(),
            background: true,
        });
    }

    fn text(m: &mut Monitor, t: &str) {
        m.apply(SubagentUpdate::Text { id: "t1".into(), text: t.into() });
    }

    fn kinds(m: &Monitor) -> Vec<(LineKind, String)> {
        m.views[0].transcript.lines().iter().map(|l| (l.kind, l.text.clone())).collect()
    }

    #[test]
    fn the_subagent_conversation_is_built_like_the_main_agents() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut m = Monitor::default();
        start(&mut m);
        m.apply(SubagentUpdate::Thinking { id: "t1".into(), text: "I look\nat the folder".into() });
        text(&mut m, "En cours : I read the sources\n");
        m.apply(SubagentUpdate::Tool { id: "t1".into(), name: "read".into(), summary: "a.rs".into() });
        m.apply(SubagentUpdate::ToolDone { id: "t1".into(), name: "read".into(), ok: true });
        text(&mut m, "Two modules.\nNothing to report");
        let lines = kinds(&m);
        assert_eq!(lines[0].0, LineKind::Delegation, "the brief opens the card");
        assert_eq!(lines[1].0, LineKind::ThinkingSummary);
        assert!(lines[1].1.starts_with("Réflexion · "), "closed header: {:?}", lines[1]);
        assert_eq!(lines[2], (LineKind::Thinking, "I look".into()));
        assert_eq!(lines[3], (LineKind::Thinking, "at the folder".into()));
        assert_eq!(lines[4], (LineKind::Tool, "read a.rs".into()));
        assert_eq!(lines[5], (LineKind::Assistant, "Two modules.".into()));
        assert_eq!(lines[6], (LineKind::Assistant, "Nothing to report".into()), "the line being written is visible");
        assert_eq!(m.views[0].status_line(), "I read the sources");
        assert!(!lines.iter().any(|(_, t)| t.contains("En cours")), "progress stays out of the card");
    }

    #[test]
    fn an_english_progress_line_is_not_displayed_either() {
        let mut m = Monitor::default();
        start(&mut m);
        text(&mut m, "Pro");
        text(&mut m, "gress: I compare");
        assert_eq!(kinds(&m).len(), 1, "nothing displayed until the line is finished");
        text(&mut m, " the modules\nHere");
        assert_eq!(m.views[0].status_line(), "I compare the modules");
        assert_eq!(kinds(&m).last().unwrap(), &(LineKind::Assistant, "Here".into()));
        assert!(may_be_progress("**progress"));
        assert!(may_be_progress("En c"));
        assert!(!may_be_progress("Here"));
    }

    #[test]
    fn a_progress_line_being_written_is_not_displayed() {
        let mut m = Monitor::default();
        start(&mut m);
        text(&mut m, "En co");
        text(&mut m, "urs : I compare");
        assert_eq!(kinds(&m).len(), 1, "nothing displayed until the line is finished");
        text(&mut m, " the modules\nHere");
        assert_eq!(m.views[0].status_line(), "I compare the modules");
        assert_eq!(kinds(&m).last().unwrap(), &(LineKind::Assistant, "Here".into()));
    }

    #[test]
    fn a_finished_subagent_stays_a_while_then_disappears() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut m = Monitor::default();
        start(&mut m);
        m.apply(SubagentUpdate::Finished { id: "t1".into(), ok: true });
        assert!(m.views[0].status_line().starts_with("terminé en "));
        assert_eq!(m.visible().len(), 1, "visible right after the end");
        m.views[0].finished = Some((Instant::now() - LINGER - Duration::from_secs(1), true));
        assert!(m.visible().is_empty());
    }

    #[test]
    fn a_paused_subagent_stops_its_clock_and_resumes_in_its_own_card() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut m = Monitor::default();
        start(&mut m);
        text(&mut m, "Read a.rs.\n");
        m.apply(SubagentUpdate::Paused { id: "t1".into() });
        assert!(!m.views[0].running());
        assert_eq!(m.views[0].status_line(), po_i18n::t!("tui.monitor.paused"));
        m.views[0].paused = Some(Instant::now() - Duration::from_secs(60));
        let lines = m.views[0].transcript.lines().len();
        start(&mut m);
        assert_eq!(m.views.len(), 1);
        assert!(m.views[0].running());
        assert_eq!(m.views[0].transcript.lines().len(), lines, "its conversation is kept");
        assert!(m.views[0].elapsed() < Duration::from_secs(5), "the pause does not count");
    }
}
