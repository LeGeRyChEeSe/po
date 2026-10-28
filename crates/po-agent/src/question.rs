//! The `question` tool: the model asks the user to decide.
//!
//! Without it, a hesitating model writes its question in its reply and ends its turn: the user
//! has to reread and rephrase, and the context of the decision is lost. With it, the turn stays
//! open, the user picks among proposed answers (or answers freely), and the model resumes with
//! the answer as a tool result.

use async_trait::async_trait;
use po_core::ToolSpec;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    /// Optional preview (code excerpt, mockup) shown when the option is highlighted.
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
}

/// What the user answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Index of the chosen option.
    Choice(usize),
    /// Free-form answer.
    Text(String),
    /// The user closed the question without answering.
    Declined,
}

/// Who puts the question to the user. `None`: nobody to answer (non-interactive session) —
/// the model must then decide alone.
#[async_trait]
pub trait Asker: Send + Sync {
    async fn ask(&self, question: &Question) -> Option<Answer>;
}

/// Bounds on the number of proposed answers: fewer than two is not a choice; beyond four, the
/// list can no longer be read at a glance on a phone.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;

pub fn tool_spec() -> ToolSpec {
    ToolSpec::new(
        "question",
        "Asks the user a question when a decision is theirs and you cannot settle it alone: choice of \
         approach, preference, missing information. Offer 2 to 4 short, distinct answers, the most \
         recommendable first; the user can also answer freely. Do not overuse it: when a reasonable \
         default exists, take it and say so.",
        json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question, complete, ending with a question mark"},
                "options": {
                    "type": "array",
                    "minItems": MIN_OPTIONS,
                    "maxItems": MAX_OPTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {"type": "string", "description": "Answer in 1 to 5 words"},
                            "description": {"type": "string", "description": "What this choice implies"},
                            "preview": {"type": "string", "description": "Optional: code excerpt or mockup to compare"}
                        },
                        "required": ["label", "description"]
                    }
                }
            },
            "required": ["question", "options"]
        }),
    )
}

/// Reads the model's call. Tolerates options given as plain strings, which small models often
/// do.
pub fn parse(input: &Value) -> Result<Question, String> {
    // Shapes seen with local models: a `questions` list in the style of other harnesses (we
    // take the first), options serialized as a JSON string, other field names. Seen on Strata:
    // a first call rejected for "missing question".
    let input = match input.get("questions").and_then(Value::as_array).and_then(|q| q.first()) {
        Some(first) if input.get("question").is_none() => first,
        _ => input,
    };
    let field = |names: &[&str]| names.iter().find_map(|n| input.get(*n)).cloned();
    let question = field(&["question", "text", "prompt", "title"])
        .and_then(|v| v.as_str().map(str::trim).map(str::to_owned))
        .filter(|q| !q.is_empty())
        .ok_or("parameter \"question\" missing: a string, with \"options\" next to it")?;
    let options = match field(&["options", "choices", "answers"]) {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null),
        Some(other) => other,
        None => Value::Null,
    };
    let options: Vec<QuestionOption> = options
        .as_array()
        .ok_or("parameter \"options\" missing: a list of 2 to 4 answers")?
        .iter()
        .filter_map(|o| match o {
            Value::String(label) => Some(QuestionOption { label: label.trim().to_owned(), description: String::new(), preview: None }),
            Value::Object(_) => Some(QuestionOption {
                label: o.get("label").and_then(Value::as_str)?.trim().to_owned(),
                description: o.get("description").and_then(Value::as_str).unwrap_or("").trim().to_owned(),
                preview: o.get("preview").and_then(Value::as_str).map(str::to_owned).filter(|p| !p.trim().is_empty()),
            }),
            _ => None,
        })
        .filter(|o| !o.label.is_empty())
        .collect();
    if options.len() < MIN_OPTIONS {
        return Err(format!("offer at least {MIN_OPTIONS} distinct answers"));
    }
    let mut options = options;
    options.truncate(MAX_OPTIONS);
    Ok(Question { question, options })
}

/// Result handed back to the model, and a readable answer for the interface.
pub fn outcome(question: &Question, answer: &Option<Answer>) -> (String, String) {
    match answer {
        Some(Answer::Choice(i)) => match question.options.get(*i) {
            Some(option) => (format!("The user chose: \"{}\".", option.label), option.label.clone()),
            None => ("The user did not answer.".into(), po_i18n::t!("agent.question.no_answer").into()),
        },
        Some(Answer::Text(text)) => (format!("The user answered: \"{}\".", text.trim()), text.trim().to_owned()),
        Some(Answer::Declined) => (
            "The user closed the question without answering: do not ask it again, continue with the most \
             cautious choice or ask them in text what they want."
                .into(),
            po_i18n::t!("agent.question.no_answer").into(),
        ),
        None => (
            "Nobody can answer (non-interactive session): pick the most reasonable option yourself and state it \
             clearly in your reply."
                .into(),
            po_i18n::t!("agent.question.nobody").into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_question_is_read() {
        let q = parse(&json!({
            "question": "Which format?",
            "options": [
                {"label": "JSON", "description": "readable", "preview": "{}"},
                {"label": "TOML", "description": "like Cargo"}
            ]
        }))
        .unwrap();
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].preview.as_deref(), Some("{}"));
        assert_eq!(outcome(&q, &Some(Answer::Choice(1))).1, "TOML");
    }

    #[test]
    fn plain_string_options_are_accepted_and_bounded() {
        let q = parse(&json!({"question": "Which one?", "options": ["a", "b", "c", "d", "e"]})).unwrap();
        assert_eq!(q.options.len(), MAX_OPTIONS);
        assert!(parse(&json!({"question": "Which one?", "options": ["alone"]})).is_err());
        assert!(parse(&json!({"options": ["a", "b"]})).is_err());
    }

    #[test]
    fn near_miss_shapes_are_understood() {
        let list = parse(&json!({"questions": [{"question": "Which one?", "options": ["a", "b"]}]})).unwrap();
        assert_eq!(list.question, "Which one?");
        let string = parse(&json!({"question": "Which one?", "options": "[{\"label\": \"a\", \"description\": \"\"}, \"b\"]"})).unwrap();
        assert_eq!(string.options.len(), 2);
        let other_names = parse(&json!({"text": "Which one?", "choices": ["a", "b"]})).unwrap();
        assert_eq!(other_names.options[1].label, "b");
    }

    #[test]
    fn without_anyone_the_model_must_decide_alone() {
        let q = parse(&json!({"question": "?", "options": ["a", "b"]})).unwrap();
        assert!(outcome(&q, &None).0.contains("pick the most reasonable"));
        assert!(outcome(&q, &Some(Answer::Text(" free ".into()))).0.contains("\"free\""));
    }
}
