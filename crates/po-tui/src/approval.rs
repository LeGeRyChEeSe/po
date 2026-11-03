//! Approval requests going through a channel.
//!
//! The agent runs inside the event loop, so it cannot read the keyboard itself without
//! competing with the interface. It sends its request on a channel and waits for an answer:
//! the interface shows the dialog, collects the key, and replies.

use std::sync::Arc;

use async_trait::async_trait;
use po_agent::Approver;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// A request waiting for an answer.
#[derive(Debug)]
pub struct ApprovalRequest {
    /// Its number, shared with the phone: an answer names the request it answers.
    pub id: u64,
    pub tool: String,
    /// What the user must read to decide: the command, the path, the pattern.
    pub summary: String,
    responder: Option<oneshot::Sender<bool>>,
}

impl ApprovalRequest {
    /// Answers the request. Consuming the struct guarantees it is answered only once.
    pub fn answer(mut self, granted: bool) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.send(granted);
        }
        crate::control::mirror().push(po_wire::Event::ApprovalDone { id: self.id, granted });
    }

    pub fn prompt(&self) -> String {
        po_i18n::t!("tui.approval.prompt", tool = self.tool, summary = self.summary)
    }
}

/// Dropped unanswered (the turn stopped, the window closed): refused, and the phone is told.
impl Drop for ApprovalRequest {
    fn drop(&mut self) {
        if self.responder.take().is_some() {
            crate::control::mirror().push(po_wire::Event::ApprovalDone { id: self.id, granted: false });
        }
    }
}

pub struct ChannelApprover {
    sender: mpsc::UnboundedSender<ApprovalRequest>,
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn approve(&self, tool: &str, input: &Value) -> bool {
        let (responder, receiver) = oneshot::channel();
        let mirror = crate::control::mirror();
        let request = ApprovalRequest {
            id: mirror.next_id(),
            tool: tool.to_owned(),
            summary: summarize(input),
            responder: Some(responder),
        };
        mirror.push(po_wire::Event::Approval { id: request.id, tool: request.tool.clone(), summary: request.summary.clone() });
        if self.sender.send(request).is_err() {
            // Nobody on the other end: the interface has closed. Refusing is the only safe
            // choice - running a write that nobody can see anymore would be worse.
            return false;
        }
        receiver.await.unwrap_or(false)
    }
}

/// Extracts what is needed to decide, without drowning the user in the full JSON.
fn summarize(input: &Value) -> String {
    for key in ["command", "path", "pattern"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return value.to_owned();
        }
    }
    let raw = input.to_string();
    if raw.chars().count() > 120 {
        format!("{}…", raw.chars().take(120).collect::<String>())
    } else {
        raw
    }
}

pub fn channel() -> (Arc<ChannelApprover>, mpsc::UnboundedReceiver<ApprovalRequest>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (Arc::new(ChannelApprover { sender }), receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_positive_answer_allows() {
        let (approver, mut receiver) = channel();
        let handle = tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            assert_eq!(request.tool, "bash");
            assert_eq!(request.summary, "ls -la");
            request.answer(true);
        });
        assert!(approver.approve("bash", &json!({"command": "ls -la"})).await);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn a_negative_answer_refuses() {
        let (approver, mut receiver) = channel();
        tokio::spawn(async move {
            receiver.recv().await.unwrap().answer(false);
        });
        assert!(!approver.approve("write", &json!({"path": "a.rs"})).await);
    }

    #[tokio::test]
    async fn a_closed_interface_refuses_instead_of_waiting() {
        // Without this behavior the agent would stay blocked forever after the window closes.
        let (approver, receiver) = channel();
        drop(receiver);
        assert!(!approver.approve("write", &json!({"path": "a.rs"})).await);
    }

    #[tokio::test]
    async fn an_abandoned_request_is_refused() {
        let (approver, mut receiver) = channel();
        tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            drop(request); // the interface closes without answering
        });
        assert!(!approver.approve("bash", &json!({"command": "x"})).await);
    }

    #[test]
    fn the_summary_prefers_readable_fields() {
        assert_eq!(summarize(&json!({"command": "cargo test"})), "cargo test");
        assert_eq!(summarize(&json!({"path": "src/main.rs"})), "src/main.rs");
        assert_eq!(summarize(&json!({"pattern": "fn main"})), "fn main");
    }

    #[test]
    fn a_summary_without_a_known_field_stays_bounded() {
        let big = json!({"data": "x".repeat(1000)});
        let summary = summarize(&big);
        assert!(summary.chars().count() <= 121);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn the_question_is_worded_clearly() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let (_sender, receiver) = oneshot::channel();
        let request = ApprovalRequest {
            id: 1,
            tool: "bash".into(),
            summary: "rm build/".into(),
            responder: Some(receiver_sender(receiver)),
        };
        assert_eq!(request.prompt(), "Autoriser bash - rm build/ ?");
    }

    /// Small test helper: rebuilds a sender from an unused receiver.
    fn receiver_sender(_receiver: oneshot::Receiver<bool>) -> oneshot::Sender<bool> {
        let (sender, _) = oneshot::channel();
        sender
    }
}
