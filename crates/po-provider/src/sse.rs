//! Incremental streaming.
//!
//! Without it, a thirty-second response is thirty seconds of silence: the user cannot tell
//! whether the model is working, the local server is struggling or the harness is stuck. With
//! a local model on CPU, where throughput is counted in a few tokens per second, this is not a
//! comfort but the difference between usable and unusable.
//!
//! Two difficulties motivate this module:
//! - the `text/event-stream` format does not guarantee that an event arrives in a single TCP
//!   packet; we therefore need an accumulator that only returns complete events;
//! - tool calls arrive **in pieces**, the name in one fragment and the arguments spread over
//!   the following ones, indexed. Reassembling them badly produces a silently truncated call,
//!   which nothing signals afterwards.

use std::collections::BTreeMap;

use po_core::{ContentBlock, Message, Role, StopReason, Usage};
use serde_json::Value;

/// Fragment of a response, as the interface consumes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// Piece of text to display immediately.
    Text(String),
    /// Piece of reasoning.
    Thinking(String),
    /// A tool call has just been announced (name known, arguments not yet complete).
    ToolCallStarted { index: usize, name: String },
    /// The stream is finished.
    Done,
    /// The local server is already processing another request: this one waits its turn.
    Queued,
}

/// Splits a byte stream into complete SSE events.
///
/// An event ends with an empty line. Until it arrives, the bytes stay pending: returning a
/// partial event would produce truncated JSON.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    /// Adds bytes and returns the `data:` payloads that are now complete.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();

        // Event separators are `\n\n`, but a server may emit `\r\n\r\n`.
        while let Some(position) = find_separator(&self.buffer) {
            let (raw, rest) = self.buffer.split_at(position.0);
            let event = raw.to_owned();
            self.buffer = rest[position.1..].to_owned();

            for line in event.lines() {
                let line = line.trim_end_matches('\r');
                // Comment lines (leading `:`) serve as a heartbeat.
                if let Some(data) = line.strip_prefix("data:") {
                    events.push(data.trim().to_owned());
                }
            }
        }
        events
    }

    /// Unconsumed remainder - useful for diagnosing an interrupted stream.
    pub fn pending(&self) -> &str {
        &self.buffer
    }
}

/// Turns packets of bytes into text without cutting a character: the bytes of one left
/// incomplete at the end of a packet wait for the next.
#[derive(Debug, Default)]
pub struct Utf8Joiner {
    pending: Vec<u8>,
}

impl Utf8Joiner {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let complete = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            // Only a character cut at the very end waits; an invalid byte elsewhere is decoded
            // lossily, as before.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let rest = self.pending.split_off(complete);
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending = rest;
        text
    }
}

fn find_separator(text: &str) -> Option<(usize, usize)> {
    let lf = text.find("\n\n").map(|index| (index, 2));
    let crlf = text.find("\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Reassembles a response from the fragments of an OpenAI-format stream.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    text: String,
    thinking: String,
    /// Calls being built, indexed as the protocol does.
    tools: BTreeMap<usize, PartialToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    model_id: String,
}

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    /// Accumulated arguments, as text: they only make sense once complete.
    arguments: String,
}

impl StreamAccumulator {
    /// Absorbs a `data:` payload. Returns the first fragment to display, if there is one.
    pub fn absorb(&mut self, payload: &str) -> Option<Delta> {
        self.absorb_all(payload).into_iter().next()
    }

    /// Absorbs a `data:` payload. Returns every fragment to display: one event can carry
    /// reasoning, text and calls together, and none of them may be lost.
    pub fn absorb_all(&mut self, payload: &str) -> Vec<Delta> {
        if payload == "[DONE]" {
            return vec![Delta::Done];
        }
        let Ok(value) = serde_json::from_str::<Value>(payload) else { return Vec::new() };

        if let Some(model) = value["model"].as_str()
            && self.model_id.is_empty()
        {
            self.model_id = model.to_owned();
        }
        if let Some(usage) = parse_usage(&value["usage"]) {
            self.usage = Some(usage);
        }

        let mut out = Vec::new();
        let Some(choice) = value["choices"].get(0) else { return out };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_owned());
        }

        let delta = &choice["delta"];

        if let Some(reasoning) = delta["reasoning_content"]
            .as_str()
            .or_else(|| delta["reasoning"].as_str())
            && !reasoning.is_empty()
        {
            self.thinking.push_str(reasoning);
            out.push(Delta::Thinking(reasoning.to_owned()));
        }

        if let Some(text) = delta["content"].as_str()
            && !text.is_empty()
        {
            self.text.push_str(text);
            out.push(Delta::Text(text.to_owned()));
        }

        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                // The index is the reassembly key: without it, two parallel calls would see their
                // arguments get mixed up.
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                let entry = self.tools.entry(index).or_default();

                if let Some(id) = call["id"].as_str()
                    && !id.is_empty()
                {
                    entry.id = id.to_owned();
                }
                if let Some(name) = call["function"]["name"].as_str()
                    && !name.is_empty()
                {
                    entry.name.push_str(name);
                    out.push(Delta::ToolCallStarted { index, name: entry.name.clone() });
                }
                if let Some(arguments) = call["function"]["arguments"].as_str() {
                    entry.arguments.push_str(arguments);
                }
            }
        }
        out
    }

    /// Builds the final response. Called once the stream is finished.
    pub fn finish(self, known_tools: &[String]) -> (Message, StopReason, Usage, String) {
        let mut content = Vec::new();
        if !self.thinking.is_empty() {
            content.push(ContentBlock::Thinking { text: self.thinking, signature: None });
        }

        // Even when streaming, a local model may write its calls as text rather than in the
        // intended field: we apply the same recovery as in non-streamed mode.
        let mut recovered = Vec::new();
        let text = if self.tools.is_empty() && !known_tools.is_empty() {
            let (cleaned, calls) = crate::recovery::recover(&self.text, known_tools);
            recovered = calls;
            cleaned
        } else {
            self.text
        };
        if !text.is_empty() {
            content.push(ContentBlock::text(text));
        }
        content.extend(crate::recovery::to_blocks(recovered, 0));

        for (index, call) in self.tools {
            if call.name.is_empty() {
                continue; // orphan fragment: nothing to run
            }
            content.push(ContentBlock::ToolUse {
                id: if call.id.is_empty() { format!("call_{index}") } else { call.id },
                name: call.name,
                input: po_core::tool::parse_tool_arguments(&call.arguments),
            });
        }

        let has_tool_use = content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let stop_reason = match self.finish_reason.as_deref() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") | Some("max_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            _ if has_tool_use => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };

        (
            Message { role: Role::Assistant, content },
            stop_reason,
            self.usage.unwrap_or_default(),
            self.model_id,
        )
    }
}

fn parse_usage(value: &Value) -> Option<Usage> {
    if !value.is_object() {
        return None;
    }
    let cached = value["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
    let prompt = value["prompt_tokens"].as_u64().unwrap_or(0);
    Some(Usage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: value["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_decoder_waits_for_a_complete_event() {
        let mut decoder = SseDecoder::default();
        // An event split across two packets must produce nothing while it is partial.
        assert!(decoder.push("data: {\"a\":").is_empty());
        assert!(decoder.push("1}").is_empty());
        assert_eq!(decoder.push("\n\n"), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn the_decoder_accepts_windows_line_endings() {
        let mut decoder = SseDecoder::default();
        assert_eq!(decoder.push("data: one\r\n\r\n"), vec!["one"]);
    }

    #[test]
    fn several_events_in_one_packet_are_all_returned() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("data: one\n\ndata: two\n\ndata: three\n\n");
        assert_eq!(events, ["one", "two", "three"]);
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn heartbeats_are_ignored() {
        let mut decoder = SseDecoder::default();
        // Servers emit comment lines to keep the connection alive.
        assert!(decoder.push(": keep-alive\n\n").is_empty());
        assert_eq!(decoder.push("data: useful\n\n"), vec!["useful"]);
    }

    #[test]
    fn text_is_returned_as_it_flows() {
        let mut acc = StreamAccumulator::default();
        let fragments = ["Hel", "lo", " the", " world"];
        let mut seen = Vec::new();
        for fragment in fragments {
            let payload = format!(r#"{{"choices":[{{"delta":{{"content":"{fragment}"}}}}]}}"#);
            if let Some(Delta::Text(text)) = acc.absorb(&payload) {
                seen.push(text);
            }
        }
        assert_eq!(seen, fragments);

        let (message, stop, _, _) = acc.finish(&[]);
        assert_eq!(message.visible_text(), "Hello the world");
        assert_eq!(stop, StopReason::EndTurn);
    }

    #[test]
    fn a_fragmented_tool_call_is_reassembled() {
        // This is the case that breaks silently when implemented naively: the name arrives
        // alone, then the arguments in pieces.
        let mut acc = StreamAccumulator::default();
        let fragments = [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        for fragment in fragments {
            acc.absorb(fragment);
        }

        let (message, stop, _, _) = acc.finish(&[]);
        assert_eq!(stop, StopReason::ToolUse);
        let (id, name, input) = message.tool_uses().next().unwrap();
        assert_eq!(id, "call_1");
        assert_eq!(name, "read");
        assert_eq!(input["path"], "a.rs", "the arguments must be glued back in order");
    }

    #[test]
    fn two_parallel_calls_do_not_get_mixed_up() {
        let mut acc = StreamAccumulator::default();
        for fragment in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{\"path\":\"one.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"read","arguments":"{\"path\":\"two.rs\"}"}}]}}]}"#,
        ] {
            acc.absorb(fragment);
        }
        let (message, _, _, _) = acc.finish(&[]);
        let paths: Vec<_> = message
            .tool_uses()
            .map(|(_, _, input)| input["path"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(paths, ["one.rs", "two.rs"]);
    }

    #[test]
    fn the_start_of_a_call_is_signaled_as_soon_as_the_name_is_known() {
        // The interface can then display "reading in progress" without waiting for the arguments.
        let mut acc = StreamAccumulator::default();
        let delta = acc.absorb(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"grep"}}]}}]}"#,
        );
        assert_eq!(delta, Some(Delta::ToolCallStarted { index: 0, name: "grep".into() }));
    }

    #[test]
    fn one_event_carrying_reasoning_text_and_a_call_loses_none_of_them() {
        let mut acc = StreamAccumulator::default();
        let deltas = acc.absorb_all(
            r#"{"choices":[{"delta":{"reasoning_content":"hmm","content":"Je lis.","tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]}}]}"#,
        );
        assert_eq!(deltas.len(), 3);
        let (message, stop, _, _) = acc.finish(&["read".into()]);
        assert_eq!(message.visible_text(), "Je lis.");
        assert_eq!(message.tool_uses().count(), 1);
        assert_eq!(stop, StopReason::ToolUse);
    }

    #[test]
    fn a_character_cut_between_two_packets_comes_out_whole() {
        let bytes = "créé → ✓".as_bytes();
        for cut in 0..=bytes.len() {
            let mut joiner = Utf8Joiner::default();
            let text = joiner.push(&bytes[..cut]) + &joiner.push(&bytes[cut..]);
            assert_eq!(text, "créé → ✓", "cut at {cut}");
        }
        // An invalid byte in the middle is still decoded, not held forever.
        assert_eq!(Utf8Joiner::default().push(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn reasoning_is_distinguished_from_text() {
        let mut acc = StreamAccumulator::default();
        assert_eq!(
            acc.absorb(r#"{"choices":[{"delta":{"reasoning_content":"I think"}}]}"#),
            Some(Delta::Thinking("I think".into()))
        );
        acc.absorb(r#"{"choices":[{"delta":{"content":"answer"}}]}"#);

        let (message, _, _, _) = acc.finish(&[]);
        assert_eq!(message.visible_text(), "answer");
        assert!(matches!(message.content[0], ContentBlock::Thinking { .. }));
    }

    #[test]
    fn usage_is_taken_from_the_last_fragment() {
        let mut acc = StreamAccumulator::default();
        acc.absorb(r#"{"choices":[{"delta":{"content":"x"}}]}"#);
        acc.absorb(
            r#"{"choices":[{"delta":{}}],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":40}}}"#,
        );
        let (_, _, usage, _) = acc.finish(&[]);
        assert_eq!(usage.input_tokens, 60, "the cache must not be counted twice");
        assert_eq!(usage.cache_read_tokens, 40);
        assert_eq!(usage.output_tokens, 5);
    }

    #[test]
    fn the_end_of_stream_is_recognized() {
        let mut acc = StreamAccumulator::default();
        assert_eq!(acc.absorb("[DONE]"), Some(Delta::Done));
    }

    #[test]
    fn an_unreadable_fragment_does_not_interrupt_the_stream() {
        let mut acc = StreamAccumulator::default();
        assert_eq!(acc.absorb("not json"), None);
        acc.absorb(r#"{"choices":[{"delta":{"content":"next"}}]}"#);
        let (message, _, _, _) = acc.finish(&[]);
        assert_eq!(message.visible_text(), "next");
    }

    #[test]
    fn a_call_without_a_name_is_discarded() {
        // An orphan fragment must not produce an empty tool call, which the loop would then
        // try to run.
        let mut acc = StreamAccumulator::default();
        acc.absorb(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#);
        let (message, _, _, _) = acc.finish(&[]);
        assert!(!message.has_tool_use());
    }

    #[test]
    fn calls_written_as_text_are_also_recovered_when_streaming() {
        let mut acc = StreamAccumulator::default();
        for fragment in ["<tool_call>{\"name\":", "\"glob\",\"arguments\":{\"pattern\":\"**/*.rs\"}}", "</tool_call>"] {
            let payload = serde_json::json!({"choices":[{"delta":{"content": fragment}}]});
            acc.absorb(&payload.to_string());
        }
        let (message, stop, _, _) = acc.finish(&["glob".to_string()]);
        assert_eq!(stop, StopReason::ToolUse);
        assert_eq!(message.tool_uses().next().unwrap().1, "glob");
    }
}
