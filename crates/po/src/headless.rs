//! A session without a terminal: what Po's service opens when the phone asks for a new session
//! (or to resume a saved one while no Po has it open).
//!
//! It is the same session as in a terminal - same agent, same commands, same approvals - driven
//! through the local control server instead of the keyboard. Everything it does goes to the
//! [`po_tui::control::Mirror`], which the service forwards to the phone; the phone's message,
//! command, approval or answer comes back as a control request.
//!
//! During a turn it behaves as the terminal does: a setting is applied at the turn's next safe
//! point (after the tools of the step in progress, before the model is called again), a message
//! reaches the model there too, a command's menu opens from those prepared at the turn's start,
//! and the turn can be paused and resumed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use po_agent::Agent;
use po_tui::control::{self, Action};
use po_tui::menu::Menu;
use po_tui::question::QuestionRequest;
use po_tui::{ApprovalRequest, Host};
use tokio::sync::mpsc;

/// A session nobody has followed or used for this long ends by itself: it would otherwise hold
/// its model and memory for nothing.
const IDLE_LIMIT: Duration = Duration::from_secs(6 * 3600);

/// What waits: messages and commands for later, the settings for the next safe point, and
/// whether the user paused the turn.
#[derive(Default)]
struct Waiting {
    queue: VecDeque<String>,
    deferred: Vec<String>,
    paused: bool,
}

/// What the phone's status shows, read from the agent itself.
fn publish(agent: &Agent, waiting: &Waiting, busy: bool) {
    control::mirror().status(status(agent, waiting, busy));
}

fn status(agent: &Agent, waiting: &Waiting, busy: bool) -> po_wire::Status {
    let model = agent.session_model();
    let prefs = po_local::prefs::LocalPrefs::load();
    let current_loading = po_tui::control::mirror().current_loading();
    let model_name = model.as_ref().map(|m| m.id.clone())
        .or_else(|| agent.router().pinned().map(str::to_owned))
        .or_else(|| current_loading.as_ref().map(|l| l.model.clone()))
        .or_else(|| prefs.default_model.clone())
        .unwrap_or_default();
    let placement = if !model_name.is_empty() {
        let hardware = po_local::Hardware::detect();
        if let Some(place) = prefs.model(&model_name).placement.as_ref() {
            Some(place.describe(&hardware))
        } else if !hardware.gpus.is_empty() {
            let gpu_names: Vec<String> = hardware.gpus.iter()
                .filter_map(|g| g.index.map(|i| format!("GPU {i} ({})", po_local::placement::short_name(&g.name))))
                .collect();
            if gpu_names.is_empty() {
                Some("GPU".into())
            } else {
                Some(gpu_names.join(", "))
            }
        } else {
            Some("CPU".into())
        }
    } else {
        None
    };
    let bd = agent.context_breakdown();
    po_wire::Status {
        title: agent.title.clone(),
        model: model_name,
        pinned: agent.router().pinned().map(str::to_owned),
        models: agent.router().models().iter().map(|m| m.id.clone()).collect(),
        work_mode: agent.work_mode().get().label().to_owned(),
        budget: agent.router().mode().label().to_owned(),
        effort: agent.settings.reasoning_effort.clone(),
        busy,
        context_used: agent.estimated_request_tokens(),
        context_window: model.map(|m| m.context_window).unwrap_or(0),
        cost: agent.session.ledger.total_cost(),
        queued: waiting.queue.len(),
        queue: waiting.queue.iter().cloned().collect(),
        paused: waiting.paused || (!busy && agent.is_paused()),
        labs: false,
        engine: po_tui::engine_of(agent),
        generation_tok_s: 0.0,
        prefill_tok_s: 0.0,
        // The mirror puts it in.
        offer: None,
        loading: current_loading,
        workspace: agent.workspace_root().display().to_string(),
        placement,
        branch: po_tui::statusline::git().and_then(|g| g.branch),
        git_added: po_tui::statusline::git().map(|g| g.added).unwrap_or(0),
        git_removed: po_tui::statusline::git().map(|g| g.removed).unwrap_or(0),
        context_tools: bd.tools,
        context_thinking: bd.thinking,
        context_assistant: bd.assistant,
        context_user: bd.user,
        context_system: bd.system,
        context_warning: None,
        loop_warning: None,
        instant_tok_s: 0.0,
        last_prefill_tok_s: 0.0,
        model_vram_mib: None,
        model_ram_mib: None,
        model_gpu_name: None,
        secondary_model: None,
        dynamic_action: None,
        subagent_parallel: 1,
        subagent_model: None,
        secondary_candidates: Vec::new(),
    }
}

fn commands_json(commands: &[(String, String)]) -> String {
    let list: Vec<po_wire::CommandInfo> = commands.iter().map(|(n, d)| po_wire::CommandInfo { name: n.clone(), description: d.clone() }).collect();
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
}

fn menu_json(menu: Option<&Menu>) -> String {
    serde_json::to_string(&menu.map(control::wire_menu)).unwrap_or_else(|_| "null".into())
}

/// The menu prepared at the turn's start for `line`.
fn turn_menu<'a>(menus: &'a [(String, Menu)], line: &str) -> Option<&'a Menu> {
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    menus.iter().find(|(l, _)| *l == line).map(|(_, m)| m)
}

/// An answer to the approval or question waiting, if it is the one meant; or a change to the
/// messages waiting.
fn answer(input: &po_wire::Input, approval: &mut Option<ApprovalRequest>, question: &mut Option<QuestionRequest>, waiting: &mut Waiting) -> (String, bool) {
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
        po_wire::Input::Unqueue { index } => match waiting.queue.remove(*index as usize) {
            Some(text) => (po_i18n::t!("tui.toast.queued_cancelled", text = text), false),
            None => (po_i18n::t!("tui.control.queue_gone").to_owned(), true),
        },
        po_wire::Input::Requeue { index, text } if !text.trim().is_empty() => match waiting.queue.get_mut(*index as usize) {
            Some(slot) => {
                *slot = text.trim().to_owned();
                (po_i18n::t!("tui.control.queue_edited").to_owned(), false)
            }
            None => (po_i18n::t!("tui.control.queue_gone").to_owned(), true),
        },
        _ => (po_i18n::t!("tui.control.nothing_running").to_owned(), false),
    }
}

/// Runs a command between turns: its report goes to the phone; true if it asks to quit.
async fn command(agent: &mut Agent, host: &mut impl Host, line: &str) -> (bool, (String, bool)) {
    let mirror = control::mirror();
    let output = host.command(line, agent).await;
    // The history first, its report after: a command that replaces the conversation (`/compact`,
    // `/new`, `/reprendre`) sends a reset, and a report sent before it would be wiped out with
    // the rest - the phone would show the conversation shrink, with no word about why.
    mirror.sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    mirror.push(po_wire::Event::CommandOutput { command: line.to_owned(), lines: control::wire_lines(&output.lines) });
    let text: Vec<String> = output.lines.iter().map(po_tui::CardLine::text).collect();
    let reply = (if text.is_empty() { po_i18n::t!("common.done").to_owned() } else { text.join("\n") }, output.lines.iter().any(po_tui::CardLine::is_warn));
    (output.quit, reply)
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
    // The subagents are followed from the phone as in the terminal's frame.
    let (monitor_tx, mut monitor) = mpsc::unbounded_channel();
    agent.set_subagent_monitor(monitor_tx);
    let mirror = control::mirror();
    mirror.sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    let mut waiting = Waiting::default();
    // The engines are looked for in the background: the default model can start at the first
    // message. The first time, or with no model at all, the phone opens the setup screen.
    crate::setup::start(None);
    crate::setup::offer(crate::setup::wanted_at_start(agent.router().models().is_empty()));
    publish(agent, &waiting, false);
    let commands = host.completions();
    let mut last_activity = Instant::now();
    let mut closing = false;
    let mut resume = false;
    while !closing {
        if std::mem::take(&mut resume) && agent.is_paused() {
            waiting.paused = false;
            closing = turn(agent, host, None, &mut requests, &mut approvals, &mut questions, &mut monitor, &mut waiting, &commands).await;
            continue;
        }
        // A paused turn keeps what waits: it goes in when the turn goes on.
        if !(agent.is_paused() && waiting.paused)
            && let Some(next) = waiting.queue.pop_front()
        {
            last_activity = Instant::now();
            if next.starts_with('/') {
                let (quit, _) = command(agent, host, &next).await;
                publish(agent, &waiting, false);
                if quit {
                    break;
                }
            } else {
                // Its model first: started if it is not serving (the message waits meanwhile).
                // The phone is told before it starts, not after: a model takes tens of seconds,
                // and `follow` keeps saying what it is doing while it loads.
                if !agent.is_paused() {
                    publish(agent, &waiting, true);
                    // The keyboard of the phone stays live while the model loads: Interrupt (or a
                    // cancel command) stops the load, a message sent meanwhile goes to the queue.
                    // The status published meanwhile is a snapshot: `before_turn` holds the agent.
                    let base = status(agent, &waiting, true);
                    let prepared = {
                        let work = host.before_turn(agent);
                        tokio::pin!(work);
                        loop {
                            tokio::select! {
                                output = &mut work => break output,
                                Some(request) = requests.recv() => {
                                    last_activity = Instant::now();
                                    let reply = match request.action {
                                        Action::Input(po_wire::Input::Interrupt) => {
                                            po_tui::request_load_cancel();
                                            (po_i18n::t!("setup.load_cancelled").to_owned(), false)
                                        }
                                        Action::Command(ref line) if po_tui::cancel_load_wanted(line) => {
                                            po_tui::request_load_cancel();
                                            (po_i18n::t!("setup.load_cancelled").to_owned(), false)
                                        }
                                        Action::Command(line) => {
                                            waiting.queue.push_back(line);
                                            (po_i18n::t!("tui.control.queued_command").to_owned(), false)
                                        }
                                        Action::Prompt(text) => {
                                            waiting.queue.push_back(text);
                                            (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                                        }
                                        other => control_read_headless(&commands, &other),
                                    };
                                    let _ = request.reply.send(reply);
                                    let mut shown = base.clone();
                                    shown.queued = waiting.queue.len();
                                    shown.queue = waiting.queue.iter().cloned().collect();
                                    control::mirror().status(shown);
                                }
                            }
                        }
                    };
                    if let Some(output) = prepared {
                        mirror.push(po_wire::Event::CommandOutput { command: po_i18n::t!("tui.model.preparing_title").to_owned(), lines: control::wire_lines(&output.lines) });
                        if output.quit {
                            // Cancelled (or no model to answer with): the message is put back,
                            // the phone can send it again once a model serves.
                            waiting.queue.push_front(next);
                            publish(agent, &waiting, false);
                            continue;
                        }
                    }
                    publish(agent, &waiting, false);
                }
                closing = turn(agent, host, Some(&next), &mut requests, &mut approvals, &mut questions, &mut monitor, &mut waiting, &commands).await;
            }
            continue;
        }
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { break };
                last_activity = Instant::now();
                let reply = match request.action {
                    Action::Prompt(text) => {
                        // A paused turn takes the message where it stopped, and goes on.
                        waiting.paused = false;
                        waiting.queue.push_back(text);
                        (po_i18n::t!("tui.control.prompt_sent").to_owned(), false)
                    }
                    // A command runs at once, and its report is the answer.
                    Action::Command(line) => {
                        let (quit, reply) = command(agent, host, &line).await;
                        closing = quit;
                        reply
                    }
                    Action::Input(po_wire::Input::Resume) if agent.is_paused() => {
                        resume = true;
                        (po_i18n::t!("tui.labs.resumed").to_owned(), false)
                    }
                    Action::Input(input) => answer(&input, &mut None, &mut None, &mut waiting),
                    Action::Menu(line) => {
                        let menu = host.command_menu(&line, agent).await;
                        (menu_json(menu.as_ref()), false)
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
                publish(agent, &waiting, false);
            }
            Some(update) = monitor.recv() => mirror.subagent(&update),
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

/// What the phone asked for, answered between turns while a model loads.
fn control_read_headless(commands: &[(String, String)], action: &Action) -> (String, bool) {
    match action {
        // The menus prepared at the turn's start are not available while a model loads.
        Action::Menu(_) => (menu_json(None), false),
        Action::Commands => (commands_json(commands), false),
        Action::Close => (po_i18n::t!("tui.control.closing").to_owned(), true),
        Action::Restart => (po_i18n::t!("headless.no_restart").to_owned(), true),
        Action::Status | Action::Screen | Action::Transcript { .. } => (control::mirror().info().to_string(), false),
        _ => (po_i18n::t!("tui.control.terminal_session").to_owned(), false),
    }
}

/// One turn (`prompt` None: a paused turn goes on), kept answerable from the phone: approvals,
/// questions, settings, messages, a pause, a stop. True if the phone asked to close the session
/// meanwhile.
#[allow(clippy::too_many_arguments)]
async fn turn(
    agent: &mut Agent,
    host: &mut impl Host,
    prompt: Option<&str>,
    requests: &mut mpsc::UnboundedReceiver<control::Request>,
    approvals: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: &mut mpsc::UnboundedReceiver<QuestionRequest>,
    monitor: &mut mpsc::UnboundedReceiver<po_agent::SubagentUpdate>,
    waiting: &mut Waiting,
    commands: &[(String, String)],
) -> bool {
    let mirror = control::mirror();
    let started = Instant::now();
    let checkpoint = agent.session.messages.len();
    let first_exchange = agent.title.is_none();
    publish(agent, waiting, true);
    // During the turn the agent is busy: the menus open on these.
    let menus = host.turn_menus(agent).await;
    let pause: Arc<AtomicBool> = agent.pause_handle();
    let mode = agent.work_mode();
    let mut approval: Option<ApprovalRequest> = None;
    let mut question: Option<QuestionRequest> = None;
    let mut interrupted = false;
    let mut closing = false;
    let mut finished = None;
    // A paused session (saved mid-turn, or paused by the user) takes the message where it stopped.
    let mut first = match prompt {
        Some(text) if agent.is_paused() => {
            agent.inject_user_message(text);
            mirror.push(po_wire::Event::User { text: text.to_owned() });
            mirror.turn_started(None);
            None
        }
        Some(text) => {
            mirror.turn_started(Some(text));
            Some(text.to_owned())
        }
        None => {
            mirror.turn_started(None);
            None
        }
    };
    loop {
        let (tx, mut events) = mpsc::unbounded_channel();
        let mut sink = move |event| {
            let _ = tx.send(event);
        };
        // The agent is busy with the turn: what changes meanwhile (the messages waiting, the
        // pause) is set on this.
        let mut shown = status(agent, waiting, true);
        {
            let prompt = first.take();
            let work = async {
                match &prompt {
                    Some(text) => agent.turn(text, &mut sink).await,
                    None => agent.resume(&mut sink).await,
                }
            };
            tokio::pin!(work);
            while finished.is_none() && !interrupted {
                tokio::select! {
                    result = &mut work => finished = Some(result),
                    Some(event) = events.recv() => mirror.agent(&event),
                    Some(update) = monitor.recv() => mirror.subagent(&update),
                    Some(request) = approvals.recv(), if approval.is_none() => approval = Some(request),
                    Some(request) = questions.recv(), if question.is_none() => question = Some(request),
                    Some(request) = requests.recv() => {
                        let reply = match request.action {
                            Action::Input(po_wire::Input::Interrupt) => {
                                interrupted = true;
                                (po_i18n::t!("tui.control.interrupted").to_owned(), false)
                            }
                            Action::Input(po_wire::Input::Pause) => {
                                pause.store(true, Ordering::Relaxed);
                                waiting.paused = true;
                                (po_i18n::t!("tui.labs.pausing").to_owned(), false)
                            }
                            Action::Input(po_wire::Input::Resume) => {
                                waiting.paused = false;
                                (po_i18n::t!("tui.labs.resumed").to_owned(), false)
                            }
                            Action::Input(input) => answer(&input, &mut approval, &mut question, waiting),
                            // As if typed: it reaches the model at the next safe point.
                            Action::Prompt(text) => {
                                if !text.starts_with('!') {
                                    pause.store(true, Ordering::Relaxed);
                                }
                                waiting.queue.push_back(text);
                                (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                            }
                            // A model to load, a session to replace: only between turns.
                            Action::Command(line) if po_tui::between_turns_only(&line) => {
                                waiting.queue.push_back(line);
                                (po_i18n::t!("tui.control.queued_command").to_owned(), false)
                            }
                            // Needing neither the agent nor the turn's end: at once.
                            Action::Command(line) if let Some(output) = host.command_now(&line, Some(&mode)).await => {
                                mirror.push(po_wire::Event::CommandOutput { command: line.clone(), lines: control::wire_lines(&output.lines) });
                                let text: Vec<String> = output.lines.iter().map(po_tui::CardLine::text).collect();
                                shown.work_mode = mode.get().label().to_owned();
                                (if text.is_empty() { po_i18n::t!("common.done").to_owned() } else { text.join("\n") }, false)
                            }
                            // A setting: applied at the next safe point, the last value of one
                            // setting winning over an earlier one.
                            Action::Command(line) => {
                                let words: Vec<&str> = line.split_whitespace().collect();
                                if let [name, _] = words[..] {
                                    waiting.deferred.retain(|c| {
                                        let earlier: Vec<&str> = c.split_whitespace().collect();
                                        !(earlier.len() == 2 && earlier[0] == name)
                                    });
                                }
                                waiting.deferred.push(line);
                                pause.store(true, Ordering::Relaxed);
                                (po_i18n::t!("tui.control.noted").to_owned(), false)
                            }
                            Action::Menu(line) => (menu_json(turn_menu(&menus, &line)), false),
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
                        shown.queued = waiting.queue.len();
                        shown.queue = waiting.queue.iter().cloned().collect();
                        shown.paused = waiting.paused;
                        mirror.status(shown.clone());
                    }
                }
            }
        }
        while let Ok(event) = events.try_recv() {
            mirror.agent(&event);
        }
        while let Ok(update) = monitor.try_recv() {
            mirror.subagent(&update);
        }
        if interrupted || !agent.is_paused() {
            break;
        }
        // Paused by the user: the agent keeps its turn, to go on later.
        if waiting.paused {
            break;
        }
        // Safe point: the tools have returned their results, the model has not resumed yet.
        for line in std::mem::take(&mut waiting.deferred) {
            let _ = command(agent, host, &line).await;
        }
        let mut rest = VecDeque::new();
        while let Some(text) = waiting.queue.pop_front() {
            if text.starts_with('/') || text.starts_with('!') {
                rest.push_back(text);
                continue;
            }
            agent.inject_user_message(&text);
            mirror.push(po_wire::Event::User { text });
        }
        waiting.queue = rest;
        finished = None;
        publish(agent, waiting, true);
    }
    pause.store(false, Ordering::Relaxed);
    // Unanswered, they are refused (and the phone is told) when dropped.
    drop(approval);
    drop(question);
    agent.end_turn_slot();
    if interrupted {
        agent.cancel_background();
        match agent.keep_after_interruption(checkpoint) {
            // Paused rather than lost: ▶ or a message takes it up again.
            po_agent::Interrupted::Resumable => {
                waiting.paused = true;
                mirror.push(po_wire::Event::Notice { text: po_i18n::t!("tui.note.interrupted_kept").to_owned() });
            }
            po_agent::Interrupted::Complete => waiting.paused = false,
            po_agent::Interrupted::Nothing => {
                waiting.paused = false;
                po_tui::restore_after_interruption(&mut agent.session.messages, checkpoint);
            }
        }
    }
    // What found no safe point is applied now, before the next turn.
    for line in std::mem::take(&mut waiting.deferred) {
        let _ = command(agent, host, &line).await;
    }
    let error = match &finished {
        Some(Err(err)) if !interrupted => Some(err.to_string()),
        _ => None,
    };
    mirror.turn_ended(interrupted, error, started.elapsed().as_secs_f64());
    if first_exchange
        && !interrupted
        && !agent.is_paused()
        && let Some(job) = agent.title_job()
        && let Some(title) = job.await
    {
        agent.title = Some(title);
    }
    host.after_turn(agent);
    mirror.sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    publish(agent, waiting, false);
    closing
}
