use super::*;

fn turn(c: &mut Conversation, events: &[Event]) {
    for e in events {
        c.apply(e);
    }
}

#[test]
fn deltas_build_one_answer_that_the_full_text_replaces() {
    let mut c = Conversation::default();
    turn(&mut c, &[
        Event::User { text: "hi".into() },
        Event::TurnStarted,
        Event::TextDelta { text: "Hel".into() },
        Event::TextDelta { text: "lo".into() },
    ]);
    assert_eq!(c.items[1], Item::Assistant { text: "Hello".into(), streaming: true });
    c.apply(&Event::Text { text: "Hello!".into() });
    assert_eq!(c.items.len(), 2);
    assert_eq!(c.items[1], Item::Assistant { text: "Hello!".into(), streaming: false });
    assert!(c.busy);
    c.apply(&Event::TurnEnded { interrupted: false, error: None, summary: Some("1 s".into()) });
    assert!(!c.busy);
    assert_eq!(c.items.last(), Some(&Item::Summary { text: "1 s".into() }));
}

#[test]
fn a_tool_runs_finishes_and_keeps_its_diff() {
    let mut c = Conversation::default();
    turn(&mut c, &[
        Event::User { text: "edit".into() },
        Event::ThinkingDelta { chars: 40 },
        Event::Thinking { text: "let me edit".into() },
        Event::ToolStarted { name: "edit".into(), summary: "a.rs".into() },
        Event::ToolStarted { name: "read".into(), summary: "b.rs".into() },
        Event::ToolFinished { name: "edit".into(), summary: "a.rs (+1)".into(), error: false },
        Event::ToolDiff { diff: "+x".into() },
        Event::ToolFinished { name: "read".into(), summary: String::new(), error: true },
    ]);
    assert!(matches!(&c.items[1], Item::Thinking { text, chars: 40, streaming: false } if text == "let me edit"));
    // The diff goes to the tool that just finished, as in the terminal.
    assert_eq!(c.items[2], Item::Tool { name: "edit".into(), summary: "a.rs (+1)".into(), state: ToolState::Done, diff: Some("+x".into()), output: String::new() });
    assert!(matches!(&c.items[3], Item::Tool { state: ToolState::Error, summary, .. } if summary == "b.rs"));
}

#[test]
fn an_interrupted_turn_leaves_nothing_spinning() {
    let mut c = Conversation::default();
    turn(&mut c, &[
        Event::User { text: "x".into() },
        Event::TurnStarted,
        Event::TextDelta { text: "a".into() },
        Event::ToolStarted { name: "bash".into(), summary: "sleep".into() },
        Event::Approval { id: 3, tool: "bash".into(), summary: "sleep".into() },
        Event::TurnEnded { interrupted: true, error: None, summary: None },
    ]);
    assert!(c.items.iter().all(|i| !i.streaming()));
    assert!(!c.items.iter().any(|i| matches!(i, Item::Tool { state: ToolState::Running, .. })));
    assert!(c.approval.is_none());
}

#[test]
fn approvals_and_questions_are_resolved_by_their_own_id_only() {
    let mut c = Conversation::default();
    c.apply(&Event::Approval { id: 1, tool: "write".into(), summary: "a".into() });
    c.apply(&Event::ApprovalDone { id: 2, granted: true });
    assert!(c.approval.is_some(), "another id does not close it");
    c.apply(&Event::ApprovalDone { id: 1, granted: false });
    assert!(c.approval.is_none());
    c.apply(&Event::Question { id: 5, question: "?".into(), options: vec![] });
    c.apply(&Event::QuestionDone { id: 5 });
    assert!(c.question.is_none());
}

#[test]
fn changes_say_where_to_redraw_from() {
    let mut c = Conversation::new(vec![Item::User { text: "a".into() }]);
    assert_eq!(c.take_changes(), Some(0));
    assert_eq!(c.take_changes(), None);
    c.apply(&Event::TextDelta { text: "b".into() });
    assert_eq!(c.take_changes(), Some(1));
    c.apply(&Event::TextDelta { text: "c".into() });
    c.apply(&Event::Notice { text: "n".into() });
    assert_eq!(c.take_changes(), Some(1));
}

#[test]
fn a_conversation_keeps_its_last_items_only() {
    let mut c = Conversation::default();
    for i in 0..MAX_ITEMS + 10 {
        c.apply(&Event::Notice { text: i.to_string() });
    }
    assert_eq!(c.items.len(), MAX_ITEMS);
    assert_eq!(c.items[0], Item::Notice { text: "10".into(), warn: false });
}

#[test]
fn the_journal_replays_what_was_missed_or_sends_a_snapshot() {
    let mut j = Journal::new("run1", 5);
    for i in 0..4 {
        j.push(Event::Notice { text: i.to_string() });
    }
    assert_eq!(j.last(), 4);
    match j.since(Some("run1"), 2) {
        Catchup::Events(events) => assert_eq!(events.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![3, 4]),
        other => panic!("{other:?}"),
    }
    assert_eq!(j.since(Some("run1"), 4), Catchup::Events(Vec::new()));
    // Another run, a number from the future, or too far behind: the whole conversation.
    assert!(matches!(j.since(Some("run0"), 2), Catchup::Snapshot { seq: 4, .. }));
    assert!(matches!(j.since(Some("run1"), 99), Catchup::Snapshot { .. }));
    assert!(matches!(j.since(None, 0), Catchup::Snapshot { .. }));
    for i in 0..10 {
        j.push(Event::Notice { text: i.to_string() });
    }
    assert!(matches!(j.since(Some("run1"), 3), Catchup::Snapshot { seq: 14, .. }));
    match j.since(Some("run1"), 9) {
        Catchup::Events(events) => assert_eq!(events.len(), 5),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_snapshot_then_the_events_after_it_give_the_same_conversation() {
    let mut j = Journal::new("e", Journal::CAPACITY);
    let events = vec![
        Event::User { text: "go".into() },
        Event::TurnStarted,
        Event::ThinkingDelta { chars: 3 },
        Event::TextDelta { text: "one ".into() },
        Event::ToolStarted { name: "read".into(), summary: "f".into() },
        Event::ToolFinished { name: "read".into(), summary: "f".into(), error: false },
        Event::TextDelta { text: "two".into() },
        Event::Text { text: "two.".into() },
        Event::TurnEnded { interrupted: false, error: None, summary: None },
    ];
    for (i, e) in events.iter().enumerate() {
        j.push(e.clone());
        // A phone joining after any event and then following ends up where the PC is.
        let Catchup::Snapshot { seq, conversation } = j.since(None, 0) else { panic!() };
        let mut phone = *conversation;
        for e in &events[i + 1..] {
            phone.apply(e);
        }
        let mut pc = Conversation::default();
        turn(&mut pc, &events);
        phone.dirty_from = None;
        pc.dirty_from = None;
        assert_eq!(phone, pc, "joined after event {seq}");
    }
}

#[test]
fn a_reset_replaces_everything_and_clears_the_ring() {
    let mut j = Journal::new("e", 100);
    j.push(Event::User { text: "old".into() });
    let fresh = Conversation::new(vec![Item::User { text: "new".into() }]);
    j.push(Event::Reset { conversation: Box::new(fresh) });
    assert_eq!(j.conversation().items, vec![Item::User { text: "new".into() }]);
    match j.since(Some("e"), 1) {
        Catchup::Events(events) => assert!(matches!(events[0].1, Event::Reset { .. })),
        other => panic!("{other:?}"),
    }
    assert!(matches!(j.since(Some("e"), 0), Catchup::Snapshot { .. }), "what came before the reset is gone");
}

#[test]
fn frames_come_out_whatever_the_cuts() {
    let messages = vec![
        Envelope { rid: 1, body: Request::Projects },
        Envelope { rid: 2, body: Request::Input { session: "s".into(), id: 7, input: Input::Prompt { text: "é€😀".repeat(100) } } },
    ];
    let bytes: Vec<u8> = messages.iter().flat_map(encode).collect();
    for cut in [1, 3, 4, 5, 17, bytes.len()] {
        let mut d = Decoder::default();
        let mut out: Vec<Envelope<Request>> = Vec::new();
        for chunk in bytes.chunks(cut) {
            d.feed(chunk);
            while let Some(m) = d.next_message().unwrap() {
                out.push(m);
            }
        }
        assert_eq!(out, messages, "cut {cut}");
    }
}

#[test]
fn a_hostile_frame_is_refused_without_allocating() {
    let mut d = Decoder::default();
    d.feed(&u32::MAX.to_be_bytes());
    assert!(matches!(d.next_frame(), Err(FrameError::TooLarge(_))));
    let mut d = Decoder::default();
    d.feed(&encode(&"not a request"));
    assert!(matches!(d.next_message::<Envelope<Request>>(), Err(FrameError::Invalid(_))));
    let mut d = Decoder::default();
    let garbage = b"\x00\x00\x00\x05\xff\xfe\x00{]";
    d.feed(garbage);
    assert!(d.next_message::<Feed>().is_err());
}

#[test]
fn the_wire_format_is_stable_json() {
    let e = Event::ToolFinished { name: "edit".into(), summary: "a".into(), error: false };
    assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"type":"tool_finished","name":"edit","summary":"a","error":false}"#);
    let r: Envelope<Request> = serde_json::from_str(r#"{"rid":4,"op":"sessions"}"#).unwrap();
    assert_eq!(r.body, Request::Sessions { project: None });
    let i: Input = serde_json::from_str(r#"{"kind":"answer","id":2,"choice":1}"#).unwrap();
    assert_eq!(i, Input::Answer { id: 2, choice: Some(1), text: None });
    // A field a newer peer adds does not break an older one.
    let s: Status = serde_json::from_str(r#"{"model":"m","future":1}"#).unwrap();
    assert_eq!(s.model, "m");
}

#[test]
fn clipping_respects_characters() {
    assert_eq!(clip("ééé", 3), "é…");
    assert_eq!(clip("abc", 3), "abc");
}

#[test]
fn a_subagent_builds_its_own_conversation_and_resumes_in_it() {
    let sub = |update: SubagentEvent| Event::Subagent { update: Box::new(update) };
    let id = || "t1".to_owned();
    let mut c = Conversation::default();
    turn(
        &mut c,
        &[
            sub(SubagentEvent::Started { id: id(), agent: "explorer".into(), model: "qwen".into(), task: "look".into(), background: false }),
            sub(SubagentEvent::Thinking { id: id(), text: "hmm".into() }),
            sub(SubagentEvent::Text { id: id(), text: "Two modules.\n".into() }),
            sub(SubagentEvent::Progress { id: id(), text: "reading".into() }),
            sub(SubagentEvent::Tool { id: id(), name: "read".into(), summary: "a.rs".into() }),
            sub(SubagentEvent::ToolDone { id: id(), name: "read".into(), ok: true }),
            sub(SubagentEvent::Paused { id: id() }),
        ],
    );
    assert!(c.take_subagents_changed());
    assert!(!c.take_subagents_changed(), "said once");
    let s = &c.subagents[0];
    assert_eq!(s.state, SubagentState::Paused);
    assert_eq!(s.progress.as_deref(), Some("reading"));
    assert!(matches!(&s.items[0], Item::Delegation { task, .. } if task == "look"), "the brief opens it");
    assert!(matches!(&s.items[1], Item::Thinking { text, streaming: false, .. } if text == "hmm"));
    assert!(matches!(&s.items[2], Item::Assistant { text, streaming: false } if text == "Two modules.\n"));
    assert!(matches!(&s.items[3], Item::Tool { state: ToolState::Done, .. }));
    // Started again: the same card goes on.
    c.apply(&sub(SubagentEvent::Started { id: id(), agent: "explorer".into(), model: "qwen".into(), task: "look".into(), background: false }));
    c.apply(&sub(SubagentEvent::Finished { id: id(), ok: true }));
    assert_eq!(c.subagents.len(), 1);
    assert_eq!(c.subagents[0].items.len(), 4);
    assert_eq!(c.subagents[0].state, SubagentState::Done);
    // An unknown subagent's step changes nothing.
    c.apply(&sub(SubagentEvent::Text { id: "other".into(), text: "x".into() }));
    assert_eq!(c.subagents.len(), 1);
}

#[test]
fn the_new_inputs_and_requests_keep_their_shape_on_the_wire() {
    let pause: Input = serde_json::from_str(r#"{"kind":"pause"}"#).unwrap();
    assert_eq!(pause, Input::Pause);
    let requeue: Input = serde_json::from_str(r#"{"kind":"requeue","index":1,"text":"hi"}"#).unwrap();
    assert_eq!(requeue, Input::Requeue { index: 1, text: "hi".into() });
    // A menu from an older PC, without the new fields, still reads.
    let menu: Menu = serde_json::from_str(r#"{"title":"t","items":[{"label":"a","command":"/a"}]}"#).unwrap();
    assert!(!menu.live && menu.back.is_none() && menu.items[0].then.is_none());
    let status: Status = serde_json::from_str(r#"{"model":"m","queued":2}"#).unwrap();
    assert!(status.queue.is_empty() && !status.paused);
}

#[test]
fn a_loading_says_what_is_being_loaded_and_for_how_long() {
    let mut c = Conversation::default();
    c.apply(&Event::Status { status: Box::new(Status { model: "qwen".into(), busy: true, loading: Some(Loading { model: "qwen".into(), stage: "loading the weights".into(), since_ms: 1000 }), ..Default::default() }) });
    let loading = c.status.loading.clone().unwrap();
    assert_eq!(loading.model, "qwen");
    assert_eq!(loading.stage, "loading the weights");
    // The seconds are counted where they are read, not sent.
    assert!(loading.elapsed() >= 1, "started in the past: {loading:?}");
    // The turn ends, the model is loaded: nothing is being waited for any more.
    c.apply(&Event::TurnStarted);
    assert!(c.busy);
    c.apply(&Event::Status { status: Box::new(Status { model: "qwen".into(), ..Default::default() }) });
    assert_eq!(c.status.loading, None);
}

#[test]
fn a_loading_keeps_its_shape_on_the_wire() {
    let status: Status = serde_json::from_str(r#"{"model":"m","loading":{"model":"m","stage":"w","since_ms":1234}}"#).unwrap();
    assert_eq!(status.loading, Some(Loading { model: "m".into(), stage: "w".into(), since_ms: 1234 }));
    let text = serde_json::to_string(&status.loading.unwrap()).unwrap();
    assert_eq!(text, r#"{"model":"m","stage":"w","since_ms":1234}"#);
    // A PC from before this field exists: a status without it still reads, and says nothing.
    let old: Status = serde_json::from_str(r#"{"model":"m","work_mode":"build"}"#).unwrap();
    assert_eq!(old.loading, None);
    // A session list from before it exists too.
    let session: SessionInfo = serde_json::from_str(r#"{"key":"live:1","live":true,"busy":true}"#).unwrap();
    assert!(!session.loading);
}

#[test]
fn an_image_event_adds_its_row() {
    let mut c = Conversation::new(vec![Item::User { text: "look @shot.png".into() }]);
    c.apply(&Event::Image { path: "C:/w/shot.png".into() });
    assert_eq!(c.items.last(), Some(&Item::Image { path: "C:/w/shot.png".into() }));
    let json = serde_json::to_string(&Request::Image { session: "s".into(), path: "p.png".into() }).unwrap();
    assert!(json.contains("\"op\":\"image\""), "{json}");
}
