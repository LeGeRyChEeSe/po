//! Questions from the model to the user, full screen.
//!
//! As with approvals, the agent does not read the keyboard itself: it sends its question on a
//! channel, and the interface shows the proposed answers, collects the choice and sends it back.

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use po_agent::question::{Answer, Asker, Question};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub struct QuestionRequest {
    /// Its number, shared with the phone.
    pub id: u64,
    pub question: Question,
    responder: Option<oneshot::Sender<Answer>>,
}

impl QuestionRequest {
    pub fn answer(mut self, answer: Answer) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.send(answer);
        }
        crate::control::mirror().push(po_wire::Event::QuestionDone { id: self.id });
    }
}

impl Drop for QuestionRequest {
    fn drop(&mut self) {
        if self.responder.take().is_some() {
            crate::control::mirror().push(po_wire::Event::QuestionDone { id: self.id });
        }
    }
}

pub struct ChannelAsker {
    sender: mpsc::UnboundedSender<QuestionRequest>,
}

#[async_trait]
impl Asker for ChannelAsker {
    async fn ask(&self, question: &Question) -> Option<Answer> {
        let (responder, receiver) = oneshot::channel();
        let mirror = crate::control::mirror();
        let id = mirror.next_id();
        let options = question.options.iter().map(|o| po_wire::QuestionOption { label: o.label.clone(), description: o.description.clone() }).collect();
        mirror.push(po_wire::Event::Question { id, question: question.question.clone(), options });
        if self.sender.send(QuestionRequest { id, question: question.clone(), responder: Some(responder) }).is_err() {
            // The interface has closed: nobody to answer.
            return None;
        }
        Some(receiver.await.unwrap_or(Answer::Declined))
    }
}

pub fn channel() -> (std::sync::Arc<ChannelAsker>, mpsc::UnboundedReceiver<QuestionRequest>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (std::sync::Arc::new(ChannelAsker { sender }), receiver)
}

/// Displayed question: the highlighted answer, and the free answer being typed.
#[derive(Debug)]
pub struct Pending {
    pub request: QuestionRequest,
    pub selected: usize,
    /// `Some`: the user is writing their own answer.
    pub typing: Option<String>,
}

impl Pending {
    pub fn new(request: QuestionRequest) -> Self {
        Self { request, selected: 0, typing: None }
    }

    /// Proposed answers, plus "Other…" last.
    pub fn choices(&self) -> usize {
        self.request.question.options.len() + 1
    }

    pub fn is_other(&self) -> bool {
        self.selected + 1 == self.choices()
    }

    /// Handles a key; returns the answer once the user has decided.
    pub fn key(&mut self, key: KeyEvent) -> Option<Answer> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let options = self.request.question.options.len();
        if let Some(text) = self.typing.as_mut() {
            match key.code {
                KeyCode::Enter if !text.trim().is_empty() => return Some(Answer::Text(text.trim().to_owned())),
                KeyCode::Esc => self.typing = None,
                KeyCode::Backspace => {
                    if text.pop().is_none() {
                        self.typing = None;
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) => {
                    text.push(c)
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.selected = (self.selected + 1).min(self.choices() - 1),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let index = c as usize - '1' as usize;
                if index < options {
                    return Some(Answer::Choice(index));
                }
                if index == options {
                    self.selected = options;
                    self.typing = Some(String::new());
                }
            }
            KeyCode::Enter => {
                if self.is_other() {
                    self.typing = Some(String::new());
                } else {
                    return Some(Answer::Choice(self.selected));
                }
            }
            KeyCode::Esc => return Some(Answer::Declined),
            // Typing a letter directly starts a free answer.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected = options;
                self.typing = Some(c.to_string());
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_agent::question::QuestionOption;

    fn pending() -> (Pending, oneshot::Receiver<Answer>) {
        let (responder, receiver) = oneshot::channel();
        let question = Question {
            question: "Which format?".into(),
            options: ["JSON", "TOML"]
                .iter()
                .map(|l| QuestionOption { label: (*l).into(), description: String::new(), preview: None })
                .collect(),
        };
        (Pending::new(QuestionRequest { id: 1, question, responder: Some(responder) }), receiver)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn arrows_then_enter_choose() {
        let (mut p, _r) = pending();
        assert_eq!(p.key(key(KeyCode::Down)), None);
        assert_eq!(p.key(key(KeyCode::Enter)), Some(Answer::Choice(1)));
    }

    #[test]
    fn a_digit_chooses_at_once() {
        let (mut p, _r) = pending();
        assert_eq!(p.key(key(KeyCode::Char('2'))), Some(Answer::Choice(1)));
    }

    #[test]
    fn other_opens_a_free_answer() {
        let (mut p, _r) = pending();
        p.key(key(KeyCode::Down));
        p.key(key(KeyCode::Down));
        assert!(p.is_other());
        assert_eq!(p.key(key(KeyCode::Enter)), None);
        for c in "YAML".chars() {
            p.key(key(KeyCode::Char(c)));
        }
        assert_eq!(p.key(key(KeyCode::Enter)), Some(Answer::Text("YAML".into())));
    }

    #[test]
    fn typing_a_letter_starts_a_free_answer_and_escape_leaves_it() {
        let (mut p, _r) = pending();
        p.key(key(KeyCode::Char('x')));
        assert_eq!(p.typing.as_deref(), Some("x"));
        p.key(key(KeyCode::Esc));
        assert!(p.typing.is_none());
        assert_eq!(p.key(key(KeyCode::Esc)), Some(Answer::Declined));
    }

    #[tokio::test]
    async fn the_answer_returns_to_the_agent() {
        let (asker, mut receiver) = channel();
        let question = Question {
            question: "?".into(),
            options: vec![
                QuestionOption { label: "a".into(), description: String::new(), preview: None },
                QuestionOption { label: "b".into(), description: String::new(), preview: None },
            ],
        };
        let waiting = tokio::spawn(async move { asker.ask(&question).await });
        receiver.recv().await.unwrap().answer(Answer::Choice(1));
        assert_eq!(waiting.await.unwrap(), Some(Answer::Choice(1)));
    }
}
