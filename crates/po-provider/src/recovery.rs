//! Recovering tool calls emitted as text.
//!
//! Not all servers turn tool calls into structured `tool_calls`: `llama-server` only does so
//! when launched with `--jinja` and a template that handles them, and many open-weight models
//! were trained to write their calls in textual markup. The result, left untreated, is an
//! agent that *describes* the tool it would like to call instead of calling it — behaviour
//! observed in real conditions on this codebase.
//!
//! Claiming to "support every LLM source" therefore requires recognizing these formats. We do
//! not guess: each recognized pattern is explicit markup, and text that matches none is left
//! intact.

use po_core::ContentBlock;
use serde_json::{Map, Value, json};

/// A call found in text.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredCall {
    pub name: String,
    pub arguments: Value,
}

/// Extracts the tool calls from a text, and returns the text stripped of their markup.
///
/// `known_tools` filters out false positives: a model that *talks about* a tool in a sentence
/// must not trigger its execution. Only the names actually declared are kept.
pub fn recover(text: &str, known_tools: &[String]) -> (String, Vec<RecoveredCall>) {
    let mut calls = Vec::new();
    let mut cleaned = text.to_owned();

    for extractor in [extract_hermes, extract_function_xml, extract_fenced_json] {
        let (rest, found) = extractor(&cleaned);
        cleaned = rest;
        calls.extend(found);
    }

    calls.retain(|call| known_tools.iter().any(|known| known == &call.name));
    (cleaned.trim().to_owned(), calls)
}

/// Converts the recovered calls into canonical blocks, ready to join the message.
pub fn to_blocks(calls: Vec<RecoveredCall>, offset: usize) -> Vec<ContentBlock> {
    calls
        .into_iter()
        .enumerate()
        .map(|(index, call)| ContentBlock::ToolUse {
            // Since the server provided no identifier, we make one up: it is what will attach the
            // result to the call on the next turn.
            id: format!("recovered_{}", offset + index),
            name: call.name,
            input: call.arguments,
        })
        .collect()
}

/// `<tool_call>{"name": "glob", "arguments": {...}}</tool_call>` — Hermes/Qwen format.
fn extract_hermes(text: &str) -> (String, Vec<RecoveredCall>) {
    extract_between(text, "<tool_call>", "</tool_call>", |body| {
        let value: Value = serde_json::from_str(body.trim()).ok()?;
        let name = value.get("name")?.as_str()?.to_owned();
        let arguments = value
            .get("arguments")
            .or_else(|| value.get("parameters"))
            .cloned()
            // Some templates serialize the arguments as a string rather than an object.
            .map(|args| match args {
                Value::String(raw) => serde_json::from_str(&raw).unwrap_or(json!({})),
                other => other,
            })
            .unwrap_or_else(|| json!({}));
        Some(RecoveredCall { name, arguments })
    })
}

/// `<function=name><parameter=key>value</parameter></function>` — Llama/Hermes variant.
/// This is the format actually encountered on a llama.cpp server without `--jinja`.
fn extract_function_xml(text: &str) -> (String, Vec<RecoveredCall>) {
    extract_between(text, "<function=", "</function>", |body| {
        let (name, rest) = body.split_once('>')?;
        let name = name.trim().trim_end_matches('>').to_owned();
        if name.is_empty() {
            return None;
        }

        let mut arguments = Map::new();
        let mut remainder = rest;
        while let Some(start) = remainder.find("<parameter=") {
            let after = &remainder[start + "<parameter=".len()..];
            let Some((key, tail)) = after.split_once('>') else { break };
            let Some((value, next)) = tail.split_once("</parameter>") else { break };
            arguments.insert(key.trim().to_owned(), coerce(value.trim()));
            remainder = next;
        }
        Some(RecoveredCall { name, arguments: Value::Object(arguments) })
    })
}

/// Code block containing a `{"name": ..., "arguments": ...}` object, without markup of its own.
fn extract_fenced_json(text: &str) -> (String, Vec<RecoveredCall>) {
    extract_between(text, "```json", "```", |body| {
        let value: Value = serde_json::from_str(body.trim()).ok()?;
        let name = value.get("name")?.as_str()?.to_owned();
        // Any old JSON block must not be mistaken for a call: without an explicit arguments
        // field, we abstain.
        let arguments = value.get("arguments").or_else(|| value.get("parameters"))?.clone();
        Some(RecoveredCall { name, arguments })
    })
}

/// Scans the text for delimited sections, applies the parser to them, and removes from the
/// text those that produced a call.
fn extract_between(
    text: &str,
    open: &str,
    close: &str,
    parse: impl Fn(&str) -> Option<RecoveredCall>,
) -> (String, Vec<RecoveredCall>) {
    let mut calls = Vec::new();
    let mut cleaned = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find(open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(close) else {
            break; // tag opened but never closed: we leave the text as is
        };
        let body = &after_open[..end];

        cleaned.push_str(&rest[..start]);
        match parse(body) {
            Some(call) => calls.push(call),
            // Unusable section: we keep it so as not to erase useful content.
            None => {
                cleaned.push_str(&rest[start..start + open.len() + end + close.len()]);
            }
        }
        rest = &after_open[end + close.len()..];
    }
    cleaned.push_str(rest);
    (cleaned, calls)
}

/// Converts a textual value to its natural type: a `limit` received as `"5"` must validate a
/// schema that expects an integer.
fn coerce(raw: &str) -> Value {
    if let Ok(number) = raw.parse::<i64>() {
        return json!(number);
    }
    if let Ok(number) = raw.parse::<f64>() {
        return json!(number);
    }
    match raw {
        "true" => json!(true),
        "false" => json!(false),
        other => json!(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<String> {
        ["glob", "read", "grep", "bash"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recovers_the_hermes_format() {
        let text = r#"I will search.
<tool_call>{"name": "glob", "arguments": {"pattern": "**/*.rs"}}</tool_call>"#;
        let (rest, calls) = recover(text, &tools());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "glob");
        assert_eq!(calls[0].arguments["pattern"], "**/*.rs");
        assert_eq!(rest, "I will search.");
    }

    #[test]
    fn recovers_the_xml_format_observed_in_real_conditions() {
        // Exact output of a llama.cpp server launched without `--jinja`.
        let text = "<tool_call></tool_call>\n<function=glob>\n<parameter=pattern>\n**/*.rs\n</parameter>\n</function>\n</tool_call>";
        let (_, calls) = recover(text, &tools());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "glob");
        assert_eq!(calls[0].arguments["pattern"], "**/*.rs");
    }

    #[test]
    fn recovers_several_parameters() {
        let text = "<function=grep><parameter=pattern>fn main</parameter>\
                    <parameter=ignore_case>true</parameter>\
                    <parameter=limit>5</parameter></function>";
        let (_, calls) = recover(text, &tools());
        assert_eq!(calls[0].arguments["pattern"], "fn main");
        assert_eq!(calls[0].arguments["ignore_case"], true, "the boolean must be typed");
        assert_eq!(calls[0].arguments["limit"], 5, "the integer must be typed");
    }

    #[test]
    fn recovers_a_tagged_json_block() {
        let text = "Here is the call:\n```json\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}\n```";
        let (_, calls) = recover(text, &tools());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["path"], "a.rs");
    }

    #[test]
    fn arguments_serialized_as_a_string_are_reread() {
        let text = r#"<tool_call>{"name":"read","arguments":"{\"path\":\"a.rs\"}"}</tool_call>"#;
        let (_, calls) = recover(text, &tools());
        assert_eq!(calls[0].arguments["path"], "a.rs");
    }

    #[test]
    fn an_unknown_tool_is_not_executed() {
        // The decisive safeguard: a model that invents a name, or mentions a tool in a
        // sentence, must trigger nothing.
        let text = r#"<tool_call>{"name": "delete_everything", "arguments": {}}</tool_call>"#;
        let (_, calls) = recover(text, &tools());
        assert!(calls.is_empty());
    }

    #[test]
    fn ordinary_text_stays_intact() {
        let text = "Here is my answer, without any tool call.";
        let (rest, calls) = recover(text, &tools());
        assert!(calls.is_empty());
        assert_eq!(rest, text);
    }

    #[test]
    fn an_unclosed_tag_erases_nothing() {
        let text = "<tool_call>{\"name\": \"glob\"";
        let (rest, calls) = recover(text, &tools());
        assert!(calls.is_empty());
        assert!(rest.contains("tool_call"), "the text must not be lost");
    }

    #[test]
    fn an_unreadable_section_is_kept_in_the_text() {
        let text = "<tool_call>this is not json</tool_call>";
        let (rest, calls) = recover(text, &tools());
        assert!(calls.is_empty());
        assert!(rest.contains("this is not json"));
    }

    #[test]
    fn several_successive_calls_are_all_recovered() {
        let text = r#"<tool_call>{"name":"read","arguments":{"path":"a.rs"}}</tool_call>
<tool_call>{"name":"read","arguments":{"path":"b.rs"}}</tool_call>"#;
        let (_, calls) = recover(text, &tools());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].arguments["path"], "b.rs");
    }

    #[test]
    fn produced_blocks_have_distinct_identifiers() {
        let calls = vec![
            RecoveredCall { name: "read".into(), arguments: json!({}) },
            RecoveredCall { name: "glob".into(), arguments: json!({}) },
        ];
        let blocks = to_blocks(calls, 0);
        let ids: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["recovered_0", "recovered_1"]);
    }

    #[test]
    fn type_conversion_stays_conservative() {
        assert_eq!(coerce("42"), json!(42));
        assert_eq!(coerce("3.5"), json!(3.5));
        assert_eq!(coerce("true"), json!(true));
        assert_eq!(coerce("**/*.rs"), json!("**/*.rs"));
        // An identifier that looks like a number but is not one stays text.
        assert_eq!(coerce("1.2.3"), json!("1.2.3"));
    }
}
