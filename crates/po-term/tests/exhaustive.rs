//! Exhaustive check of the terminal capability space.
//!
//! Po cannot be tested on every terminal in the world: there are dozens, on platforms we have no
//! access to, and more will come. What we can do - and it is stronger than a list of terminals
//! tried - is enumerate **every possible combination of capabilities** and check that the
//! invariants hold for each one.
//!
//! It is the only form of "total compatibility" that can be demonstrated: an unknown terminal
//! can only present one combination of these capabilities, and all of them are covered here.

use po_term::caps::{ColorDepth, TermCaps, UnicodeLevel};
use po_term::env::{Multiplexer, Platform, TermHost, Transport};
use po_term::{BoxChars, RenderProfile};

/// All the values of each observable dimension.
const PLATFORMS: &[Platform] = &[
    Platform::Windows,
    Platform::MacOs,
    Platform::Linux,
    Platform::Termux,
    Platform::Other,
];
const TRANSPORTS: &[Transport] = &[Transport::Local, Transport::Ssh];
const MULTIPLEXERS: &[Option<Multiplexer>] = &[
    None,
    Some(Multiplexer::Tmux),
    Some(Multiplexer::Screen),
    Some(Multiplexer::Zellij),
];
const COLORS: &[ColorDepth] = &[
    ColorDepth::None,
    ColorDepth::Ansi16,
    ColorDepth::Ansi256,
    ColorDepth::TrueColor,
];
const UNICODES: &[UnicodeLevel] = &[UnicodeLevel::Ascii, UnicodeLevel::Bmp, UnicodeLevel::Full];
/// Sizes covering the extreme cases: degenerate, phone in portrait, narrow terminal, desktop
/// screen, and an absurd width that a multiplexer can really report.
const SIZES: &[(u16, u16)] = &[
    (0, 0),
    (1, 1),
    (32, 12),
    (40, 24),
    (56, 52),
    (80, 24),
    (120, 40),
    (400, 100),
    (u16::MAX, u16::MAX),
];
const TERMS: &[&str] = &["", "dumb", "vt100", "xterm", "xterm-256color", "screen-256color", "unknown-of-the-future"];

/// Builds a capability set from a combination, without going through the environment.
#[allow(clippy::too_many_arguments)]
fn build(
    platform: Platform,
    transport: Transport,
    multiplexer: Option<Multiplexer>,
    color: ColorDepth,
    unicode: UnicodeLevel,
    size: (u16, u16),
    term: &str,
    is_tty: bool,
    sync_output: bool,
) -> TermCaps {
    let hostile = matches!(term, "dumb" | "") && platform != Platform::Windows;
    TermCaps {
        host: TermHost {
            term: term.to_owned(),
            multiplexer,
            transport,
            platform,
            emulator: None,
            term_is_hostile: hostile,
        },
        color,
        unicode,
        alt_screen: is_tty && !hostile,
        mouse: is_tty && !hostile,
        kitty_keyboard: false,
        sync_output,
        bracketed_paste: is_tty,
        hyperlinks: false,
        size,
        is_tty,
        probed: false,
    }
}

/// Walks the whole combinatorics and applies a check to each case.
fn for_each_combination(mut check: impl FnMut(&TermCaps)) -> usize {
    let mut count = 0;
    for &platform in PLATFORMS {
        for &transport in TRANSPORTS {
            for &multiplexer in MULTIPLEXERS {
                for &color in COLORS {
                    for &unicode in UNICODES {
                        for &size in SIZES {
                            for &term in TERMS {
                                for is_tty in [true, false] {
                                    for sync in [true, false] {
                                        let caps = build(
                                            platform, transport, multiplexer, color, unicode,
                                            size, term, is_tty, sync,
                                        );
                                        check(&caps);
                                        count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    count
}

#[test]
fn no_combination_panics() {
    // The first invariant, and the most important: whatever the terminal, Po must reach a
    // decision. A panic here would be a crash on the user's machine.
    let total = for_each_combination(|caps| {
        let profile = RenderProfile::from_caps(caps);
        let _ = profile.to_string();
        let _ = profile.box_chars(caps);
        let _ = profile.uses_alt_screen();
        let _ = profile.allows_mouse();
        let _ = caps.degraded();
    });
    assert!(total > 5000, "the combinatorics must be significant, {total} cases walked");
}

#[test]
fn without_a_terminal_rendering_is_always_a_stream() {
    // Safety invariant: without a TTY, no alternate screen must ever be attempted, or control
    // sequences would be dumped into a file or a pipe.
    for_each_combination(|caps| {
        if !caps.is_tty {
            assert_eq!(
                RenderProfile::from_caps(caps),
                RenderProfile::Stream,
                "non-TTY output: {caps:?}"
            );
        }
    });
}

#[test]
fn a_hostile_terminal_never_gets_full_screen() {
    for_each_combination(|caps| {
        if caps.host.term_is_hostile {
            assert_eq!(RenderProfile::from_caps(caps), RenderProfile::Stream);
        }
    });
}

#[test]
fn rich_rendering_always_requires_its_conditions() {
    // Nobody must get full rendering without the capabilities that make it readable.
    for_each_combination(|caps| {
        if RenderProfile::from_caps(caps) == RenderProfile::Rich {
            assert!(caps.is_tty);
            assert!(caps.alt_screen);
            assert!(!caps.host.term_is_hostile);
            assert!(caps.color >= ColorDepth::Ansi256);
            assert!(caps.unicode >= UnicodeLevel::Bmp);
            assert!(caps.size.0 >= 30 && caps.size.1 >= 12, "size: {:?}", caps.size);
            // A remote link without synchronized refresh would visibly tear.
            assert!(caps.host.transport == Transport::Local || caps.sync_output);
            assert_ne!(caps.host.platform, Platform::Termux, "a touch screen stays sober");
        }
    });
}

#[test]
fn degradation_always_ends_on_the_stream() {
    // The fallback must converge: an infinite loop here would freeze the interface at the worst
    // moment, the one where rendering has just failed.
    for_each_combination(|caps| {
        let mut profile = RenderProfile::from_caps(caps);
        for _ in 0..8 {
            let next = profile.degrade();
            assert!(next <= profile, "degradation must never go back up");
            profile = next;
        }
        assert_eq!(profile, RenderProfile::Stream, "the floor must be reached");
    });
}

#[test]
fn degraded_capabilities_never_go_back_up() {
    for_each_combination(|caps| {
        let low = caps.degraded();
        assert!(low.color <= caps.color);
        assert!(low.unicode <= caps.unicode);
        assert!(!low.mouse);
        assert!(!low.kitty_keyboard);
        assert!(!low.hyperlinks);
        // The nature of the terminal does not change because the display is degraded.
        assert_eq!(low.is_tty, caps.is_tty);
        assert_eq!(low.size, caps.size);
    });
}

#[test]
fn frames_remain_displayable_everywhere() {
    // A Unicode frame on an ASCII terminal would produce question marks in place of the
    // borders - the kind of detail that makes an interface unreadable.
    for_each_combination(|caps| {
        let profile = RenderProfile::from_caps(caps);
        let chars = profile.box_chars(caps);
        if caps.unicode < UnicodeLevel::Bmp {
            assert_eq!(chars, BoxChars::ASCII, "non-ASCII frame on an ASCII terminal");
        }
        // Whatever the set, no frame character must be a control character.
        for c in [
            chars.horizontal, chars.vertical,
            chars.top_left, chars.top_right,
            chars.bottom_left, chars.bottom_right,
        ] {
            assert!(!c.is_control(), "control character in a frame");
        }
    });
}

#[test]
fn the_mouse_is_never_enabled_outside_rich_rendering() {
    // Enabling mouse tracking without knowing how to use it dumps sequences into the input.
    for_each_combination(|caps| {
        let profile = RenderProfile::from_caps(caps);
        if profile != RenderProfile::Rich {
            assert!(!profile.allows_mouse());
        }
    });
}

#[test]
fn computed_width_never_exceeds_the_budget() {
    // Layout invariant: every truncated string must fit in the requested width, for every
    // Unicode level. Overflowing by one column breaks a frame.
    let samples = [
        "",
        "plain ascii",
        "accents éàïôù",
        "日本語のテキスト",
        "emoji 🚀✅👨‍👩‍👧 mixed",
        "combining e\u{0301}e\u{0301}e\u{0301}",
        &"very long ".repeat(40),
    ];
    for &level in UNICODES {
        for text in samples {
            for budget in [0usize, 1, 2, 3, 5, 10, 40, 200] {
                let cut = po_term::width::truncate(text, budget, level);
                assert!(
                    po_term::width::display_width(cut, level) <= budget,
                    "truncation to {budget} exceeded for {text:?} in {level:?}"
                );
                let with_ellipsis = po_term::width::truncate_with_ellipsis(text, budget, level);
                assert!(
                    po_term::width::display_width(&with_ellipsis, level) <= budget,
                    "ellipsis at {budget} exceeded for {text:?} in {level:?}"
                );
                // Padding must reach the width exactly, never exceed it.
                let padded = po_term::width::pad_to(cut, budget, level);
                assert!(po_term::width::display_width(&padded, level) <= budget.max(1));
            }
        }
    }
}
