//! A session without a terminal: what Po's service opens when the phone asks for a new session
//! (or to resume a saved one while no Po has it open).
//!
//! It is the same session as in a terminal — same agent, same commands, same approvals — driven
//! through the local control server instead of the keyboard. Everything it does goes to the
//! [`po_tui::control::Mirror`], which the service forwards to the phone; the phone's message,
//! command, approval or answer comes back as a control request.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use po_agent::Agent;
use po_tui::control::{self, Action};
use po_tui::question::QuestionRequest;
use po_tui::{ApprovalRequest, Host};
use tokio::sync::mpsc;

/// A session nobody has followed or used for this long ends by itself: it would otherwise hold
/// its model and memory for nothing.
const IDLE_LIMIT: Duration = Duration::from_secs(6 * 3600);

/// What the phone's status shows, read from the agent itself.
fn publish(agent: &Agent, queued: usize, busy: bool) {
    let model = agent.session_model();
    control::mirror().status(po_wire::Status {
        title: agent.title.clone(),
        model: model.as_ref().map(|m| m.id.clone()).unwrap_or_default(),
        pinned: agent.router().pinned().map(str::to_owned),
        models: agent.router().models().iter().map(|m| m.id.clone()).collect(),
        work_mode: agent.work_mode().get().label().to_owned(),
        budget: agent.router().mode().label().to_owned(),
        effort: agent.settings.reasoning_effort.clone(),
        busy,
        context_used: agent.estimated_request_tokens(),
        context_window: model.map(|m| m.context_window).unwrap_or(0),
        cost: agent.session.ledger.total_cost(),
        queued,
        labs: false,
        workspace: agent.workspace_root().display().to_string(),
    });
}

fn commands_json(commands: &[(String, String)]) -> String {
    let list: Vec<po_wire::CommandInfo> = commands.iter().map(|(n, d)| po_wire::CommandInfo { name: n.clone(), description: d.clone() }).collect();
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
}

/// An answer to the approval or question waiting, if it is the one meant.
fn answer(input: &po_wire::Input, approval: &mut Option<ApprovalRequest>, question: &mut Option<QuestionRequest>) -> (String, bool) {
    match input {
        po_wire::Input::Approve { id, granted } => match approval.take() {
            Some(request) if *id == 0 || request.id == *id => {
                request.answer(*granted);
                (po_i18n::t!("common.done").to_owned(), false)
            }
            other => {
                *approval = other;
                (po_i18n::t!("tui.control.nothing_pending").to_owned(), true)
            }
        },
        po_wire::Input::Answer { id, choice, text } => match question.take() {
            Some(request) if *id == 0 || request.id == *id => {
                let options = request.question.options.len();
                let answer = match (choice, text) {
                    (Some(i), _) if (*i as usize) < options => po_agent::question::Answer::Choice(*i as usize),
                    (_, Some(t)) if !t.trim().is_empty() => po_agent::question::Answer::Text(t.trim().to_owned()),
                    _ => po_agent::question::Answer::Declined,
                };
                request.answer(answer);
                (po_i18n::t!("common.done").to_owned(), false)
            }
            other => {
                *question = other;
                (po_i18n::t!("tui.control.nothing_pending").to_owned(), true)
            }
        },
        _ => (po_i18n::t!("tui.control.nothing_running").to_owned(), false),
    }
}

/// Runs the session until the phone closes it, `/quit`, or a long idle.
pub async fn run(
    agent: &mut Agent,
    host: &mut impl Host,
    mut approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    mut questions: mpsc::UnboundedReceiver<QuestionRequest>,
) -> Result<(), String> {
    let (tx, mut requests) = mpsc::unbounded_channel::<control::Request>();
    let _server = control::start_as(host.workspace(), tx, "headless").await.map_err(|e| e.to_string())?;
    let mirror = control::mirror();
    mirror.sync(agent.session_key.as_deref(), &agent.session.messages);
    publish(agent, 0, false);
    let commands = host.completions();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut last_activity = Instant::now();
    let mut closing = false;
    while !closing {
        if let Some(next) = queue.pop_front() {
            last_activity = Instant::now();
            if next.starts_with('/') {
                let output = host.command(&next, agent).await;
                mirror.push(po_wire::Event::CommandOutput { command: next.clone(), lines: control::wire_lines(&output.lines) });
                mirror.sync(agent.session_key.as_deref(), &agent.session.messages);
                publish(agent, queue.len(), false);
                if output.quit {
                    break;
                }
            } else {
                closing = turn(agent, host, &next, &mut requests, &mut approvals, &mut questions, &mut queue, &commands).await;
            }
            continue;
        }
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { break };
                last_activity = Instant::now();
                let reply = match request.action {
                    Action::Prompt(text) => {
                        queue.push_back(text);
                        (po_i18n::t!("tui.control.prompt_sent").to_owned(), false)
                    }
                    // A command runs at once, and its report is the answer.
                    Action::Command(command) => {
                        let output = host.command(&command, agent).await;
                        mirror.push(po_wire::Event::CommandOutput { command: command.clone(), lines: control::wire_lines(&output.lines) });
                        mirror.sync(agent.session_key.as_deref(), &agent.session.messages);
                        publish(agent, 0, false);
                        closing = output.quit;
                        let text: Vec<String> = output.lines.iter().map(po_tui::CardLine::text).collect();
                        (if text.is_empty() { po_i18n::t!("common.done").to_owned() } else { text.join("\n") }, output.lines.iter().any(po_tui::CardLine::is_warn))
                    }
                    Action::Input(input) => answer(&input, &mut None, &mut None),
                    Action::Menu(line) => {
                        let menu = host.command_menu(&line, agent).await;
                        (serde_json::to_string(&menu.as_ref().map(control::wire_menu)).unwrap_or_else(|_| "null".into()), false)
                    }
                    Action::Commands => (commands_json(&commands), false),
                    Action::Close => {
                        closing = true;
                        (po_i18n::t!("tui.control.closing").to_owned(), false)
                    }
                    Action::Restart => (po_i18n::t!("headless.no_restart").to_owned(), true),
                    Action::Status | Action::Screen | Action::Transcript { .. } => (control::mirror().info().to_string(), false),
                };
                let _ = request.reply.send(reply);
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                if last_activity.elapsed() >= IDLE_LIMIT {
                    break;
                }
            }
        }
    }
    mirror.push(po_wire::Event::Closed);
    // Followers get the end before the server goes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

/// One turn, kept answerable from the phone: approvals, questions, a stop. True if the phone
/// asked to close the session meanwhile.
#[allow(clippy::too_many_arguments)]
async fn turn(
    agent: &mut Agent,
    host: &mut impl Host,
    prompt: &str,
    requests: &mut mpsc::UnboundedReceiver<control::Request>,
    approvals: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: &mut mpsc::UnboundedReceiver<QuestionRequest>,
    queue: &mut VecDeque<String>,
    commands: &[(String, String)],
) -> bool {
    let mirror = control::mirror();
    let started = Instant::now();
    let checkpoint = agent.session.messages.len();
    let first_exchange = agent.title.is_none();
    publish(agent, queue.len(), true);
    let (tx, mut events) = mpsc::unbounded_channel();
    let mut sink = move |event| {
        let _ = tx.send(event);
    };
    let mut approval: Option<ApprovalRequest> = None;
    let mut question: Option<QuestionRequest> = None;
    let mut interrupted = false;
    let mut closing = false;
    let mut finished = None;
    // A paused session (saved mid-turn) takes the message where it stopped.
    let paused = agent.is_paused();
    if paused {
        agent.inject_user_message(prompt);
        mirror.push(po_wire::Event::User { text: prompt.to_owned() });
        mirror.turn_started(None);
    } else {
        mirror.turn_started(Some(prompt));
    }
    {
        let work = async {
            if paused { agent.resume(&mut sink).await } else { agent.turn(prompt, &mut sink).await }
        };
        tokio::pin!(work);
        while finished.is_none() && !interrupted {
            tokio::select! {
                result = &mut work => finished = Some(result),
                Some(event) = events.recv() => mirror.agent(&event),
                Some(request) = approvals.recv(), if approval.is_none() => approval = Some(request),
                Some(request) = questions.recv(), if question.is_none() => question = Some(request),
                Some(request) = requests.recv() => {
                    let reply = match request.action {
                        Action::Input(po_wire::Input::Interrupt) => {
                            interrupted = true;
                            (po_i18n::t!("tui.control.interrupted").to_owned(), false)
                        }
                        Action::Input(input) => answer(&input, &mut approval, &mut question),
                        Action::Prompt(text) => {
                            queue.push_back(text);
                            (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                        }
                        Action::Command(command) => {
                            queue.push_back(command);
                            (po_i18n::t!("tui.control.queued_command").to_owned(), false)
                        }
                        // The agent is busy: the menus wait for the end of the turn.
                        Action::Menu(_) => ("null".to_owned(), false),
                        Action::Commands => (commands_json(commands), false),
                        Action::Close => {
                            interrupted = true;
                            closing = true;
                            (po_i18n::t!("tui.control.closing").to_owned(), false)
                        }
                        Action::Restart => (po_i18n::t!("headless.no_restart").to_owned(), true),
                        Action::Status | Action::Screen | Action::Transcript { .. } => (control::mirror().info().to_string(), false),
                    };
                    let _ = request.reply.send(reply);
                }
            }
        }
    }
    while let Ok(event) = events.try_recv() {
        mirror.agent(&event);
    }
    // Unanswered, they are refused (and the phone is told) when dropped.
    drop(approval);
    drop(question);
    agent.end_turn_slot();
    if interrupted {
        po_tui::restore_after_interruption(&mut agent.session.messages, checkpoint);
        agent.cancel_background();
    }
    let error = match &finished {
        Some(Err(err)) if !interrupted => Some(err.to_string()),
        _ => None,
    };
    mirror.turn_ended(interrupted, error, started.elapsed().as_secs_f64());
    if first_exchange
        && !interrupted
        && let Some(job) = agent.title_job()
        && let Some(title) = job.await
    {
        agent.title = Some(title);
    }
    host.after_turn(agent);
    mirror.sync(agent.session_key.as_deref(), &agent.session.messages);
    publish(agent, queue.len(), false);
    closing
}
