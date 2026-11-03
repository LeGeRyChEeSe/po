//! Session state: history, accounting, compaction.

use po_core::{ContentBlock, Ledger, Message, Pricing, Usage};
use serde::{Deserialize, Serialize};

/// Rough estimate of a text's token count. Four characters per token is an acceptable
/// approximation to decide *when* to compact; the bill always comes from the figures the
/// provider returns, never from this estimate.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32).div_ceil(4)
}

#[derive(Debug, Clone)]
pub struct Session {
    pub system: String,
    pub messages: Vec<Message>,
    pub ledger: Ledger,
    /// What compaction removed, word for word: the `recall` tool searches it, so an exact
    /// detail (a file read, a command output) is never lost for good.
    pub archive: Vec<Message>,
    /// What the server reported reading, against what the estimate counted, on the last call.
    pub base: Option<PromptBase>,
}

/// The part of a prompt the estimate cannot see: a chat template's markup around every message,
/// and the tool declarations rendered into the system prompt. Kept from the server's own figures
/// so that the next decision is taken on a real size.
///
/// Measured on a session of 292 messages: 106,373 tokens counted from the history, 127,166 read
/// by the server. The session believed it had 25k tokens of room when it had none - which is
/// where a reply gets cut in the middle of a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptBase {
    /// Model whose figures produced this base: a chat template belongs to a model, and the
    /// markup one adds says nothing about another's.
    model: String,
    measured: u32,
    estimated: u32,
}

impl PromptBase {
    /// What the estimate missed on this conversation. Never negative: an estimate above the
    /// measured size means nothing to add.
    pub fn overhead(&self) -> u32 {
        self.measured.saturating_sub(self.estimated)
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

impl Session {
    pub fn new(system: impl Into<String>) -> Self {
        Self { system: system.into(), messages: Vec::new(), ledger: Ledger::default(), archive: Vec::new(), base: None }
    }

    /// Where to cut so that the most recent messages, up to `budget` tokens, stay word for
    /// word - at least the last exchange, and never between a call and its result.
    pub fn cut_for_budget(&self, budget: u32) -> usize {
        let mut kept = 0u32;
        let mut index = self.messages.len();
        while index > 0 {
            let size = message_tokens(&self.messages[index - 1]);
            if kept + size > budget && self.messages.len() - index >= 2 {
                break;
            }
            kept += size;
            index -= 1;
        }
        self.safe_cut_point(index)
    }

    /// Replaces the messages before `cut` with the summary; they go to the archive.
    pub fn compact_at(&mut self, summary: impl Into<String>, cut: usize) {
        let cut = self.safe_cut_point(cut);
        if cut == 0 {
            return;
        }
        let tail = self.messages.split_off(cut);
        let dropped = std::mem::take(&mut self.messages);
        self.messages = vec![Message::user(compacted_note(&summary.into(), &dropped, &tail))];
        self.messages.extend(tail);
        // The previous summary note is not archived: what it summed up is already there.
        self.archive.extend(dropped.into_iter().filter(|m| !m.visible_text().starts_with(SUMMARY_MARKER)));
    }

    /// Passages of the archive that contain all the words of `query` (case ignored), most
    /// recent first, each cut around the match.
    pub fn recall(&self, query: &str, max: usize) -> Vec<String> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (index, message) in self.archive.iter().enumerate().rev() {
            for line in transcript_of(std::slice::from_ref(message), usize::MAX).lines() {
                let lower = line.to_lowercase();
                if words.iter().all(|w| lower.contains(w)) {
                    let at = lower.find(&words[0]).unwrap_or(0);
                    let start = line.char_indices().map(|(i, _)| i).take_while(|i| *i + 600 <= at).last().unwrap_or(0);
                    let excerpt: String = line[start..].chars().take(1600).collect();
                    found.push(format!("[archived message {}] {excerpt}", index + 1));
                    if found.len() >= max {
                        return found;
                    }
                }
            }
        }
        found
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn record(&mut self, model: &str, usage: Usage, pricing: &Pricing, seconds: f64) {
        self.ledger.record(model, usage, pricing, seconds);
    }

    /// Estimated size of the prompt that will be sent on the next turn - history and system
    /// prompt only, counted the way [`estimate_messages`] counts.
    pub fn estimated_prompt_tokens(&self) -> u32 {
        estimate_messages(&self.system, &self.messages)
    }

    /// What the estimate missed on this conversation, taken from the server's own figures at the
    /// last call on `model_id`: a chat template's markup around every message, and the difference
    /// between how Po counts a tool declaration and how the server renders it. Zero until a call
    /// on that model has reported its figures - the markup is the model's, not a general truth.
    pub fn prompt_overhead(&self, model_id: &str) -> u32 {
        self.base.as_ref().filter(|b| b.model == model_id).map(|b| b.overhead()).unwrap_or(0)
    }

    /// Size of the request about to be sent: the estimate, corrected by [`Self::prompt_overhead`].
    /// `tools` is what that request will declare.
    pub fn request_tokens(&self, model_id: &str, tools: u32) -> u32 {
        self.estimated_prompt_tokens() + tools + self.prompt_overhead(model_id)
    }

    /// Records what a call actually weighed, server side: `measured` is the whole prompt the
    /// server read (cache included), `estimated` what Po's own count gave for the same request,
    /// tools included. Their difference is what the estimate cannot see, and the next decision
    /// has to account for it.
    pub fn note_prompt_size(&mut self, model_id: &str, measured: u32, estimated: u32) {
        if measured > 0 {
            self.base = Some(PromptBase { model: model_id.to_owned(), measured, estimated });
        }
    }

    /// Should we compact before the next turn?
    pub fn needs_compaction(&self, context_window: u32, reserve: u32) -> bool {
        self.estimated_prompt_tokens() + reserve > context_window
    }

    /// Compacts the history, keeping the last `keep` messages and replacing the earlier ones
    /// with the supplied summary.
    ///
    /// The cut cannot fall just anywhere: an assistant message that calls a tool must stay
    /// together with the matching result, otherwise the provider rejects the turn for an
    /// orphan call. The cut is therefore moved back to a safe boundary.
    pub fn compact(&mut self, summary: impl Into<String>, keep: usize) {
        if self.messages.len() <= keep {
            return;
        }
        self.compact_at(summary, self.messages.len().saturating_sub(keep));
    }

    /// Moves `index` back to a point where no tool call is left without its result.
    fn safe_cut_point(&self, index: usize) -> usize {
        let mut cut = index.min(self.messages.len());
        while cut > 0 {
            let pending_tool_use = self.messages[..cut]
                .last()
                .is_some_and(|message| message.has_tool_use());
            if !pending_tool_use {
                let orphan_result = self.messages.get(cut).is_some_and(|message| {
                    message
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
                });
                if !orphan_result {
                    return cut;
                }
            }
            cut -= 1;
        }
        0
    }

    /// Shrinks the history until it fits in `budget` tokens.
    ///
    /// Compaction by dropping old messages is not always enough: a two-message history where
    /// one weighs a hundred thousand tokens is still too big, and `compact` has nothing left
    /// to remove. Without this second step, the session would stay above the limit for good,
    /// hence blocked for good.
    ///
    /// The content of the largest messages is therefore truncated, keeping their beginning and
    /// end - where the instruction and the conclusion are - and flagging the cut so the model
    /// knows something is missing.
    pub fn shrink_to(&mut self, budget: u32) -> bool {
        if self.estimated_prompt_tokens() <= budget {
            return false;
        }

        // Two increasingly aggressive passes; the second leaves only an excerpt. Tool results
        // and the text a call carries (a file to write) count too: they are what weighs most in
        // an agentic turn, and clipping text alone left four files read in full untouched
        // (observed: "no model available" right after "large messages reduced").
        for max_chars in [8_000usize, 2_000] {
            for message in &mut self.messages {
                for block in &mut message.content {
                    match block {
                        ContentBlock::Text { text } | ContentBlock::ToolResult { content: text, .. } if text.len() > max_chars => {
                            *text = clip(text, max_chars);
                        }
                        ContentBlock::ToolUse { input, .. } => clip_strings(input, max_chars),
                        _ => {}
                    }
                }
            }
            if self.estimated_prompt_tokens() <= budget {
                return true;
            }
        }
        true
    }

    /// Plain text of the history, to produce a compaction summary.
    ///
    /// Tool calls and an excerpt of their results are part of it: in an agentic turn, the
    /// assistant writes almost no text, and a summary made of the text alone did not know what
    /// had been read or changed. Observed: "no concrete change yet" after a whole window of
    /// exploration, which the model then started over.
    pub fn transcript(&self) -> String {
        transcript_of(&self.messages, 400)
    }
}

/// Size of one message as counted in the prompt.
fn message_tokens(message: &Message) -> u32 {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::ToolUse { input, name, .. } => estimate_tokens(&input.to_string()) + estimate_tokens(name),
            ContentBlock::Image { .. } => 1500,
            ContentBlock::Thinking { signature: None, .. } => 0,
            other => estimate_tokens(other.as_text()),
        })
        .sum()
}

/// Size of a system prompt plus a history, the way [`Session::estimated_prompt_tokens`] counts it.
/// Exposed for a caller holding its own message list (a side request reusing the conversation).
pub fn estimate_messages(system: &str, messages: &[Message]) -> u32 {
    let mut total = estimate_tokens(system);
    for message in messages {
        total += message_tokens(message);
    }
    total
}

/// Size of the tool declarations sent with every request.
///
/// They are part of the prompt - the server reads them before the first message - and they are
/// the part the history estimate ignored. Measured on a real session of 292 messages: the history
/// counted 106,373 tokens, the server had read 127,166. The gap was the tools. A session can
/// therefore sit well under the compaction threshold while the server has no room left for the
/// answer, and cuts the reply in the middle of a call.
pub fn tools_tokens(specs: &[po_core::ToolSpec]) -> u32 {
    specs
        .iter()
        .map(|spec| {
            estimate_tokens(&serde_json::to_string(&spec.input_schema).unwrap_or_default())
                + estimate_tokens(&spec.description)
                + estimate_tokens(&spec.name)
        })
        .sum()
}

/// Plain text of messages, tool calls and results included; results cut to `result_chars`.
pub fn transcript_of(messages: &[Message], result_chars: usize) -> String {
    {
        let mut lines = Vec::new();
        for message in messages {
            let role = match message.role {
                po_core::Role::User => "User",
                po_core::Role::Assistant => "Assistant",
            };
            let text = message.visible_text();
            if !text.trim().is_empty() {
                lines.push(format!("{role}: {text}"));
            }
            for block in &message.content {
                match block {
                    ContentBlock::ToolUse { name, input, .. } => {
                        lines.push(format!("Assistant called {name}: {}", clip(&input.to_string(), 300)));
                    }
                    ContentBlock::ToolResult { content, is_error, .. } => {
                        let label = if *is_error { "Tool error" } else { "Tool result" };
                        let content = if content.len() > result_chars { clip(content, result_chars) } else { content.clone() };
                        // One line per result: `recall` matches line by line.
                        lines.push(format!("{label}: {}", if result_chars == usize::MAX { content.replace('\n', " ⏎ ") } else { content }));
                    }
                    _ => {}
                }
            }
        }
        lines.join("\n")
    }
}

/// Opening of the message that replaces the compacted history.
pub const SUMMARY_MARKER: &str = "[Summary of the previous exchanges]";
const REQUEST_MARKER: &str = "[The user's current request, verbatim]";
const CONTINUE_NOTE: &str = "[The history above was compacted to fit the context window. Continue the current \
     request from where the work stopped: the files listed were already read or changed, so do not read them again \
     unless you need an exact passage, and do not start the exploration over. The removed part is archived word for \
     word: the `recall` tool finds an exact detail in it (a file content, a command output, a decision).]";
const READ_PREFIX: &str = "Files already read: ";
const CHANGED_PREFIX: &str = "Files already changed: ";

/// What the user typed in a message, without the notes Po put before it (work mode, settings:
/// paragraphs that start with a bracket) or after it (a message sent during a turn). Shown when
/// a session is displayed again, and used for its title.
pub fn typed_text(text: &str) -> String {
    let text = text.trim_end().strip_suffix(INJECTED_NOTE).unwrap_or(text).trim_end();
    let paragraphs: Vec<&str> = text.split("

").collect();
    let first = paragraphs.iter().position(|p| !p.trim_start().starts_with('[')).unwrap_or(paragraphs.len());
    paragraphs[first..].join("

").trim().to_owned()
}

/// Follows a message the user sent during a turn (see `Agent::inject_user_message`).
pub const INJECTED_NOTE: &str =
    "[The user sent this message while you were working. Take it into account from now on.]";

/// A message the user typed, as opposed to tool results and the notes Po adds (summaries,
/// continuation prompts, subagent results), which all start with a bracket.
fn typed_request(message: &Message) -> Option<String> {
    if message.role != po_core::Role::User {
        return None;
    }
    if message.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })) {
        // Results, unless the user spoke in the middle of the turn.
        return message.content.iter().rev().find_map(|block| match block {
            ContentBlock::Text { text } => text.strip_suffix(INJECTED_NOTE).map(|t| t.trim().to_owned()),
            _ => None,
        });
    }
    let text = message.visible_text();
    (!text.trim().is_empty() && !text.trim_start().starts_with('[')).then_some(text)
}

/// The request carried by an earlier summary, so that it survives a second compaction.
fn carried_request(message: &Message) -> Option<String> {
    let text = message.visible_text();
    let start = text.find(REQUEST_MARKER)? + REQUEST_MARKER.len();
    let rest = &text[start..];
    let end = rest.find(CONTINUE_NOTE).unwrap_or(rest.len());
    Some(rest[..end].trim().to_owned()).filter(|r| !r.is_empty())
}

fn add_unique(list: &mut Vec<String>, path: &str) {
    if !list.iter().any(|p| p == path) {
        list.push(path.to_owned());
    }
}

/// Builds the message that stands for the dropped history: the summary, the files the work
/// already went through, and - when the tail kept does not show it - the user's request word
/// for word. A model that only gets a summary cannot tell what is being asked of it: after a
/// compaction mid-turn, one answered "the user hasn't asked a question yet".
fn compacted_note(summary: &str, dropped: &[Message], tail: &[Message]) -> String {
    let mut read = Vec::new();
    let mut changed = Vec::new();
    for message in dropped {
        // Files carried by an earlier summary stay listed.
        let text = message.visible_text();
        if text.starts_with(SUMMARY_MARKER) {
            for line in text.lines() {
                let (list, paths) = if let Some(paths) = line.strip_prefix(READ_PREFIX) {
                    (&mut read, paths)
                } else if let Some(paths) = line.strip_prefix(CHANGED_PREFIX) {
                    (&mut changed, paths)
                } else {
                    continue;
                };
                for path in paths.split(", ").map(str::trim).filter(|p| !p.is_empty()) {
                    add_unique(list, path);
                }
            }
        }
        for block in &message.content {
            if let ContentBlock::ToolUse { name, input, .. } = block
                && let Some(path) = input.get("path").and_then(|p| p.as_str())
            {
                match name.as_str() {
                    "read" => add_unique(&mut read, path),
                    "edit" | "write" => add_unique(&mut changed, path),
                    _ => {}
                }
            }
        }
    }
    read.retain(|path| !changed.contains(path));

    let mut note = format!("{SUMMARY_MARKER}\n{}", summary.trim());
    if !read.is_empty() || !changed.is_empty() {
        note.push('\n');
    }
    if !read.is_empty() {
        note.push_str(&format!("\n{READ_PREFIX}{}", read.join(", ")));
    }
    if !changed.is_empty() {
        note.push_str(&format!("\n{CHANGED_PREFIX}{}", changed.join(", ")));
    }
    if !tail.iter().any(|m| typed_request(m).is_some()) {
        let request = dropped.iter().rev().find_map(|m| typed_request(m).or_else(|| carried_request(m)));
        if let Some(request) = request {
            note.push_str(&format!("\n\n{REQUEST_MARKER}\n{}\n\n{CONTINUE_NOTE}", clip(request.trim(), 6000)));
        }
    }
    note
}

/// Keeps the beginning and end of a text that is too long, announcing what is missing.
/// Boundaries are snapped to whole characters: cutting in the middle of a multibyte
/// character would panic.
/// Clips the long strings of a call's arguments, its structure kept.
fn clip_strings(value: &mut serde_json::Value, max_chars: usize) {
    match value {
        serde_json::Value::String(text) if text.len() > max_chars => *text = clip(text, max_chars),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| clip_strings(v, max_chars)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| clip_strings(v, max_chars)),
        _ => {}
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars - head_len;

    let head_end = text
        .char_indices()
        .nth(head_len)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let total_chars = text.chars().count();
    let tail_start = text
        .char_indices()
        .nth(total_chars.saturating_sub(tail_len))
        .map(|(index, _)| index)
        .unwrap_or(text.len());

    if tail_start <= head_end {
        return text[..head_end].to_owned();
    }
    format!(
        "{}\n[… {} characters removed to fit the window …]\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::Role;
    use serde_json::json;

    fn tool_call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "read".into(),
                input: json!({"path": "a.rs"}),
            }],
        }
    }

    #[test]
    fn the_estimate_grows_with_the_content() {
        let mut session = Session::new("system");
        let base = session.estimated_prompt_tokens();
        session.push(Message::user("a question long enough to weigh something"));
        assert!(session.estimated_prompt_tokens() > base);
    }

    #[test]
    fn compaction_triggers_at_the_right_threshold() {
        let mut session = Session::new("");
        session.push(Message::user("x".repeat(40_000)));
        // ~10,000 estimated tokens.
        assert!(!session.needs_compaction(32_768, 4096));
        assert!(session.needs_compaction(8192, 4096));
    }

    #[test]
    fn compaction_keeps_the_end_of_the_history() {
        let mut session = Session::new("");
        for i in 0..10 {
            session.push(Message::user(format!("message {i}")));
        }
        session.compact("summary of the exchanges", 3);

        assert_eq!(session.messages.len(), 4, "summary + 3 messages");
        assert!(session.messages[0].visible_text().contains("summary of the exchanges"));
        assert!(session.messages[3].visible_text().contains("message 9"));
    }

    #[test]
    fn compaction_never_separates_a_call_from_its_result() {
        // Cutting between the call and its result would produce an orphan call, rejected by
        // the provider on the next turn.
        let mut session = Session::new("");
        session.push(Message::user("do something"));
        session.push(tool_call("call_1"));
        session.push(Message::tool_results(vec![ContentBlock::tool_result("call_1", "ok")]));
        session.push(Message::assistant("done"));

        session.compact("summary", 2);

        // We check the invariant rather than the exact length: every call kept must have its
        // result after it.
        for (index, message) in session.messages.iter().enumerate() {
            for (id, _, _) in message.tool_uses() {
                let result_present = session.messages[index + 1..].iter().any(|next| {
                    next.content.iter().any(|block| {
                        matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)
                    })
                });
                assert!(result_present, "call {id} left orphaned by compaction");
            }
        }
        // And no result must end up without its call.
        for (index, message) in session.messages.iter().enumerate() {
            for block in &message.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    let call_present = session.messages[..index]
                        .iter()
                        .any(|prev| prev.tool_uses().any(|(id, _, _)| id == tool_use_id));
                    assert!(call_present, "result {tool_use_id} without a matching call");
                }
            }
        }
    }

    #[test]
    fn a_single_huge_message_is_shrunk() {
        // `compact` can remove nothing when the history is short; without `shrink_to`, the
        // session would stay above the limit indefinitely.
        let mut session = Session::new("");
        session.push(Message::user("z".repeat(400_000)));
        assert!(session.estimated_prompt_tokens() > 50_000);

        assert!(session.shrink_to(10_000), "the reduction must happen");
        assert!(
            session.estimated_prompt_tokens() <= 10_000,
            "remaining: {} tokens",
            session.estimated_prompt_tokens()
        );
        // The message is still there, but cut and flagged as such.
        assert!(session.messages[0].visible_text().contains("removed"));
    }

    #[test]
    fn tool_results_and_call_arguments_are_shrunk_too() {
        let mut session = Session::new("");
        session.push(Message::user("read them"));
        session.push(Message {
            role: po_core::Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "w".into(),
                name: "write".into(),
                input: serde_json::json!({"path": "a.txt", "content": "y".repeat(100_000)}),
            }],
        });
        session.push(Message::tool_results(
            (0..4).map(|i| ContentBlock::ToolResult { tool_use_id: format!("r{i}"), content: "x".repeat(60_000), is_error: false }).collect(),
        ));
        assert!(session.shrink_to(10_000));
        assert!(session.estimated_prompt_tokens() <= 10_000, "remaining: {} tokens", session.estimated_prompt_tokens());
        let input = session.messages[1].tool_uses().next().unwrap().2;
        assert_eq!(input["path"], "a.txt", "the call keeps its shape");
    }

    #[test]
    fn a_session_that_already_fits_is_untouched() {
        let mut session = Session::new("");
        session.push(Message::user("short"));
        let before = session.messages.clone();
        assert!(!session.shrink_to(10_000));
        assert_eq!(session.messages, before);
    }

    #[test]
    fn clipping_respects_multibyte_characters() {
        let text = "é".repeat(20_000);
        let clipped = clip(&text, 1_000);
        assert!(clipped.contains("removed"));
        // The string must remain valid text: building it is proof enough in Rust, but we
        // check that both ends are kept.
        assert!(clipped.starts_with('é'));
        assert!(clipped.ends_with('é'));
    }

    #[test]
    fn unsigned_reasoning_does_not_count_in_the_prompt() {
        let mut session = Session::new("");
        session.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking { text: "x".repeat(40_000), signature: None }],
        });
        assert!(session.estimated_prompt_tokens() < 100, "never sent back over the OpenAI format");
        session.messages[0].content =
            vec![ContentBlock::Thinking { text: "x".repeat(40_000), signature: Some("sig".into()) }];
        assert!(session.estimated_prompt_tokens() > 9000, "a signed one is sent back");
    }

    #[test]
    fn a_compaction_mid_turn_keeps_the_request_and_the_files_seen() {
        let mut session = Session::new("");
        session.push(Message::user("add verbosity levels"));
        for i in 0..6 {
            let id = format!("call_{i}");
            session.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.clone(),
                    name: if i == 1 { "edit".into() } else { "read".into() },
                    input: json!({"path": format!("src/f{i}.rs")}),
                }],
            });
            session.push(Message::tool_results(vec![ContentBlock::tool_result(&id, "contents")]));
        }
        session.compact("explored the renderer", 4);

        let note = session.messages[0].visible_text();
        assert!(note.starts_with(SUMMARY_MARKER));
        assert!(note.contains("add verbosity levels"), "the request must survive word for word");
        assert!(note.contains("Files already read: src/f0.rs"), "{note}");
        assert!(note.contains("Files already changed: src/f1.rs"), "{note}");

        // A second compaction still carries both.
        for i in 6..10 {
            let id = format!("call_{i}");
            session.push(tool_call(&id));
            session.push(Message::tool_results(vec![ContentBlock::tool_result(&id, "contents")]));
        }
        session.compact("went on exploring", 2);
        let note = session.messages[0].visible_text();
        assert!(note.contains("add verbosity levels"), "{note}");
        assert!(note.contains("src/f1.rs") && note.contains("src/f0.rs"), "{note}");
        assert_eq!(note.matches(SUMMARY_MARKER).count(), 1);
    }

    #[test]
    fn compaction_keeps_a_token_budget_of_recent_messages_and_archives_the_rest() {
        let mut session = Session::new("");
        session.push(Message::user("refactor the parser"));
        for i in 0..20 {
            let id = format!("call_{i}");
            session.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse { id: id.clone(), name: "read".into(), input: json!({"path": format!("f{i}.rs")}) }],
            });
            let body = format!("fn parse_{i}() {{}}\n{}", "x".repeat(400));
            session.push(Message::tool_results(vec![ContentBlock::tool_result(&id, body)]));
        }
        let cut = session.cut_for_budget(1000);
        assert!(cut > 1 && cut < session.messages.len() - 2, "keeps several steps, not everything: {cut}");
        assert!(!session.messages[cut].content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })), "never an orphan result");
        session.compact_at("read the parser files", cut);

        assert_eq!(session.archive.len(), cut, "everything removed is archived");
        let found = session.recall("parse_3", 5);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("fn parse_3()"));
        assert!(session.recall("parse_3 nowhere", 5).is_empty(), "all the words must match");
        // A second compaction does not archive the first summary.
        let cut = session.cut_for_budget(500);
        session.compact_at("more", cut);
        assert!(!session.archive.iter().any(|m| m.visible_text().starts_with(SUMMARY_MARKER)));
    }

    #[test]
    fn the_typed_text_drops_po_s_notes() {
        let text = format!("[Bypass mode enabled by the user] Your actions are allowed.

Rename the label

please

{INJECTED_NOTE}");
        assert_eq!(typed_text(&text), "Rename the label

please");
        assert_eq!(typed_text("plain"), "plain");
        assert_eq!(typed_text("[only a note]"), "");
    }

    #[test]
    fn the_transcript_shows_the_tool_calls() {
        let mut session = Session::new("");
        session.push(tool_call("call_1"));
        session.push(Message::tool_results(vec![ContentBlock::tool_result("call_1", "fn main() {}")]));
        let transcript = session.transcript();
        assert!(transcript.contains("Assistant called read"), "{transcript}");
        assert!(transcript.contains("fn main() {}"));
    }

    #[test]
    fn compacting_a_short_history_does_nothing() {
        let mut session = Session::new("");
        session.push(Message::user("a"));
        let before = session.messages.clone();
        session.compact("summary", 5);
        assert_eq!(session.messages, before);
    }

    #[test]
    fn accounting_follows_the_turns() {
        let mut session = Session::new("");
        let usage = Usage { input_tokens: 100, output_tokens: 50, ..Default::default() };
        session.record("qwen", usage, &Pricing::FREE, 2.0);
        session.record(
            "opus",
            usage,
            &Pricing { input_per_mtok: 15.0, output_per_mtok: 75.0, ..Default::default() },
            0.0,
        );
        assert_eq!(session.ledger.total_tokens(), 300);
        assert!(session.ledger.total_cost() > 0.0);
        assert!((session.ledger.local_share() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn an_image_weighs_more_than_its_encoding() {
        let mut session = Session::new("");
        session.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Image { media_type: "image/png".into(), data: "iVBO".into() }],
        });
        assert!(session.estimated_prompt_tokens() >= 1500);
    }
}
