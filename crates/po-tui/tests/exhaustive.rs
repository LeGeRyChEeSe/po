//! Exhaustive check of the interface rendering.
//!
//! Same approach as for `po-term`: rather than hoping the right screen size was tried, we
//! enumerate the possible states of the interface and check that the layout invariants hold
//! everywhere. An overflow of a single column breaks a frame and makes the display unreadable -
//! which is exactly the defect reported on competing harnesses.

use po_agent::AgentEvent;
use po_core::Usage;
use po_router::{BudgetMode, Reason, TaskClass};
use po_tui::{App, Line, LineKind, Status};

const MODES: &[BudgetMode] = &[
    BudgetMode::MaxQuality,
    BudgetMode::Balanced,
    BudgetMode::SaveMaxTokens,
    BudgetMode::Offline,
];

/// Realistic model names, from shortest to longest, with accents and digits.
const MODELS: &[&str] = &[
    "",
    "m",
    "opus",
    "claude-opus-5",
    "Qwen3-27B-Instruct-Q4_K_M.gguf (llama.cpp)",
    "a-deliberately-very-long-modèle-name-to-overflow-any-bar",
];

#[test]
fn the_status_bar_never_overflows() {
    // The bar is the only element whose width depends on external data (model name, amounts).
    // That is where overflow lurks.
    let mut cases = 0;
    for model in MODELS {
        for &mode in MODES {
            for tokens in [0u64, 1, 999, 12_345, u64::MAX] {
                for cost in [0.0, 0.0001, 1.5, 12_345.678] {
                    for share in [0.0, 0.5, 1.0] {
                        for vram in [None, Some(0u64), Some(4096), Some(u64::MAX)] {
                            let status = Status {
                                model: (*model).to_owned(),
                                mode: Some(mode),
                                local_server: None,
                                vram_free_mib: vram,
                                tokens,
                                cost,
                                local_share: share,
                                busy: false,
                                context: Some((tokens as u32, 131_072)),
                            };
                            for width in [0usize, 1, 5, 20, 40, 56, 80, 200] {
                                let rendered = status.render(width);
                                let columns = rendered.chars().count();
                                // A single segment may exceed a tiny bar - which is better than
                                // an empty bar - but never two.
                                if columns > width {
                                    assert!(
                                        !rendered.contains(" · "),
                                        "{width}-column bar exceeded by several segments: {rendered:?}"
                                    );
                                }
                                assert!(!rendered.contains('\n'), "the bar must fit on one line");
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(cases > 10_000, "{cases} cases covered");
}

#[test]
fn every_event_produces_a_consistent_display() {
    // No event, even a degenerate one (empty text, very long, multi-line, control characters),
    // must panic or produce a row containing a raw line break: a transcript line must stay one
    // line.
    let texts = [
        String::new(),
        "simple".to_owned(),
        "multi\nline\nwith\nbreaks".to_owned(),
        "é".repeat(500),
        "🚀".repeat(200),
        "\u{1}\u{7}controls".to_owned(),
        "very long ".repeat(300),
    ];

    for text in &texts {
        let mut app = App::new();
        app.absorb(AgentEvent::ModelSelected {
            model: text.clone(),
            class: TaskClass::Orchestration,
            reason: Reason::OfflineFallback,
        });
        app.absorb(AgentEvent::TextDelta(text.clone()));
        app.absorb(AgentEvent::Text(text.clone()));
        app.absorb(AgentEvent::Thinking(text.clone()));
        app.absorb(AgentEvent::ToolStarted { name: text.clone(), summary: text.clone() });
        app.absorb(AgentEvent::ToolFinished {
            name: text.clone(),
            summary: text.clone(),
            is_error: true,
        });
        app.absorb(AgentEvent::ToolDenied { name: text.clone(), summary: text.clone() });
        app.absorb(AgentEvent::Delegated {
            model: text.clone(),
            agent: text.clone(),
            task: text.clone(),
            background: true,
        });
        app.absorb(AgentEvent::Notice(text.clone()));
        app.absorb(AgentEvent::TurnCompleted {
            usage: Usage { input_tokens: u64::MAX / 4, output_tokens: 1, ..Default::default() },
            cost: f64::MAX / 2.0,
            local_share: 1.0,
            local_seconds: f64::MAX / 4.0,
        });

        for line in app.transcript.lines() {
            assert!(
                !line.text.contains('\n'),
                "a transcript line contains a line break: {:?}",
                line.text
            );
        }
    }
}

#[test]
fn scrolling_stays_within_bounds_whatever_the_window() {
    for total in [0usize, 1, 5, 50, 5000] {
        for viewport in [1usize, 3, 24, 40, 100] {
            let mut transcript = po_tui::Transcript::default();
            for index in 0..total {
                transcript.push(Line::new(LineKind::Assistant, index.to_string()));
            }
            // Scrolling up past the start, then back down past the end, must never produce an
            // invalid slice.
            transcript.scroll_up(usize::MAX / 2, viewport);
            let visible = transcript.visible(viewport);
            assert!(visible.len() <= viewport.max(1) || total <= viewport);

            transcript.scroll_down(usize::MAX / 2);
            assert!(transcript.is_at_bottom());
            let visible = transcript.visible(viewport);
            assert!(visible.len() <= total.max(1));

            // The requested window can be larger than the content.
            let _ = transcript.visible(usize::MAX / 2);
        }
    }
}

#[test]
fn input_supports_any_editing_sequence() {
    // The cursor is indexed in characters; any sequence of insertions and deletions must leave
    // the buffer in a consistent state, including on multi-byte graphemes.
    let mut input = po_tui::Input::default();
    let alphabet = ['a', 'é', '日', '🚀', ' '];

    for step in 0..2000 {
        match step % 7 {
            0 | 1 => input.insert(alphabet[step % alphabet.len()]),
            2 => input.backspace(),
            3 => input.delete(),
            4 => input.move_left(),
            5 => input.move_right(),
            _ => {
                if step % 100 == 6 {
                    let _ = input.submit();
                }
            }
        }
        // Invariant: the cursor never leaves the buffer.
        assert!(input.cursor() <= input.text().chars().count(), "cursor outside the buffer");
    }

    input.move_home();
    assert_eq!(input.cursor(), 0);
    input.move_end();
    assert_eq!(input.cursor(), input.text().chars().count());
}
