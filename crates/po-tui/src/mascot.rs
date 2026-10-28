//! Po's mascot: a chunky kawaii panda in pixel art, drawn with half blocks.
//!
//! The head is one grid of 19 × 15 pixels (the left half is drawn, the right half mirrors it);
//! the expressions change a few pixels. Two pixels stack in each terminal cell (`▀` with a
//! foreground and a background color), so the panda takes 19 columns and 8 rows. The same grid
//! makes the SVG of the README.
//!
//! On an empty session it greets the user in the middle of the screen: it pops up with a small
//! bounce, smiles open-mouthed, then blinks now and then.

use std::time::Duration;

use po_term::ColorDepth;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TuiLine, Span};

/// Left half; the last column is the axis.
const HALF: [&str; 15] = [
    "..KKK.....",
    ".KKKKK....",
    ".KKKKK....",
    "..KKKKKKKK",
    ".KWWWWWWWW",
    "KWWWWWWWWW",
    "KWWKKKKWWW",
    "KWKKKKKKWW",
    "KWKKKKKKWW",
    "KWWKKKKWKK",
    "KWPPPWWWWW",
    "KWPPPWWWWW",
    ".KWWWWWWWW",
    "..KKWWWWWW",
    "....KKKKKK",
];

pub const WIDTH: usize = 19;
/// Terminal rows taken by the head.
pub const ROWS: usize = HALF.len().div_ceil(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eyes {
    /// Round, with a shine.
    Open,
    /// Closed and content: a white `^`.
    Happy,
    /// Closed for a blink.
    Blink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mouth {
    Smile,
    /// Open, tongue showing: saying hello.
    Open,
}

fn color(code: u8) -> Option<(u8, u8, u8)> {
    Some(match code {
        b'K' => (0x3a, 0x3f, 0x52),
        b'W' => (0xfb, 0xfa, 0xf5),
        b'P' => (0xff, 0x9f, 0xb8),
        b'w' => (0xff, 0xff, 0xff),
        b'R' => (0xe8, 0x5a, 0x71),
        _ => return None,
    })
}

/// The pixel grid of an expression.
pub fn grid(eyes: Eyes, mouth: Mouth) -> Vec<Vec<u8>> {
    let mut grid: Vec<Vec<u8>> = HALF
        .iter()
        .map(|row| {
            let left = row.as_bytes();
            left.iter().chain(left[..left.len() - 1].iter().rev()).copied().collect()
        })
        .collect();
    let mut set = |x: usize, y: usize, c: u8| grid[y][x] = c;
    let both = |x: usize, y: usize, c: u8, set: &mut dyn FnMut(usize, usize, u8)| {
        set(x, y, c);
        set(WIDTH - 1 - x, y, c);
    };
    match eyes {
        Eyes::Open => {
            set(4, 7, b'w');
            set(12, 7, b'w');
        }
        Eyes::Happy => {
            for (x, y) in [(4, 7), (5, 7), (3, 8), (6, 8)] {
                both(x, y, b'w', &mut set);
            }
        }
        Eyes::Blink => {
            for x in 3..=6 {
                both(x, 8, b'w', &mut set);
            }
        }
    }
    let mouth_pixels: &[(usize, usize, u8)] = match mouth {
        Mouth::Smile => &[(9, 10, b'K'), (8, 11, b'K'), (10, 11, b'K')],
        Mouth::Open => &[(9, 10, b'K'), (8, 11, b'K'), (9, 11, b'R'), (10, 11, b'K'), (9, 12, b'K')],
    };
    for &(x, y, c) in mouth_pixels {
        set(x, y, c);
    }
    grid
}

/// Nearest color the terminal can show.
fn paint((r, g, b): (u8, u8, u8), depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::TrueColor => Color::Rgb(r, g, b),
        _ => {
            // The 6×6×6 cube of 256-color terminals, or the gray ramp for near grays.
            let level = |v: u8| if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 };
            if r.abs_diff(g) < 12 && g.abs_diff(b) < 20 {
                let gray = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
                if gray > 238 {
                    return Color::Indexed(231);
                }
                return Color::Indexed(232 + ((gray.saturating_sub(8)) / 10).min(23) as u8);
            }
            Color::Indexed(16 + 36 * level(r) + 6 * level(g) + level(b))
        }
    }
}

/// The head as terminal rows. Without colors, nothing: a panda in plain characters would only
/// be a smudge.
pub fn rows(eyes: Eyes, mouth: Mouth, depth: ColorDepth) -> Vec<TuiLine<'static>> {
    if matches!(depth, ColorDepth::None | ColorDepth::Ansi16) {
        return Vec::new();
    }
    let grid = grid(eyes, mouth);
    let mut out = Vec::with_capacity(ROWS);
    for y in (0..grid.len()).step_by(2) {
        let mut spans = Vec::with_capacity(WIDTH);
        for x in 0..WIDTH {
            let top = color(grid[y][x]);
            let bottom = grid.get(y + 1).and_then(|row| color(row[x]));
            spans.push(match (top, bottom) {
                (None, None) => Span::raw(" "),
                (Some(t), None) => Span::styled("▀", Style::default().fg(paint(t, depth))),
                (None, Some(b)) => Span::styled("▄", Style::default().fg(paint(b, depth))),
                (Some(t), Some(b)) => Span::styled("▀", Style::default().fg(paint(t, depth)).bg(paint(b, depth))),
            });
        }
        out.push(TuiLine::from(spans));
    }
    out
}

/// Where the greeting animation is, `elapsed` after the screen opened: the vertical offset of
/// the head (in rows, for the bounce), and its expression.
pub fn pose(elapsed: Duration) -> (usize, Eyes, Mouth) {
    let ms = elapsed.as_millis() as u64;
    // Pops up from below, overshoots a little, settles.
    let offset = match ms {
        0..=80 => 3,
        81..=160 => 2,
        161..=240 => 1,
        241..=320 => 0,
        321..=400 => 1,
        _ => 0,
    };
    if ms < 1600 {
        return (offset, Eyes::Happy, Mouth::Open);
    }
    // Then a blink every four seconds, and a content smile now and then.
    let cycle = (ms - 1600) % 4000;
    let eyes = match cycle {
        0..=140 => Eyes::Blink,
        2000..=2700 if (ms / 4000) % 3 == 1 => Eyes::Happy,
        _ => Eyes::Open,
    };
    (offset, eyes, Mouth::Smile)
}

/// Whether the animation still moves `elapsed` after the start (for the redraw rhythm).
pub fn moving(elapsed: Duration) -> bool {
    elapsed < Duration::from_millis(1700)
}

/// Name to greet: the account's, first letter capitalized.
pub fn user_name() -> String {
    let raw = std::env::var("PO_USER")
        .or_else(|_| std::env::var("USERNAME"))
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One of the greetings, chosen by `seed`.
pub fn greeting(seed: u64, user: &str, project: &str) -> String {
    let all = po_i18n::t!("view.splash.greetings", user = user, project = project);
    let lines: Vec<&str> = all.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines.get(seed as usize % lines.len().max(1)).map(|l| (*l).to_owned()).unwrap_or_default()
}

/// The whole welcome block, centered by the caller: the head, then the project and its branch,
/// then the greeting.
pub fn welcome(
    elapsed: Duration,
    seed: u64,
    depth: ColorDepth,
    project: &str,
    branch: Option<&str>,
    unicode: bool,
) -> Vec<TuiLine<'static>> {
    let (offset, eyes, mouth) = pose(elapsed);
    let mut lines = rows(eyes, mouth, depth);
    // The bounce: the head is drawn lower, the space above it taking the difference.
    if !lines.is_empty() {
        for _ in 0..offset {
            lines.pop();
            lines.insert(0, TuiLine::from(""));
        }
        lines.push(TuiLine::from(""));
    }
    let green = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let mut title = vec![Span::styled(project.to_owned(), green)];
    if let Some(branch) = branch {
        title.push(Span::styled(format!("  {}{branch}", if unicode { "⎇ " } else { "" }), dim));
    }
    lines.push(TuiLine::from(title));
    let user = user_name();
    lines.push(TuiLine::from(Span::styled(greeting(seed, &user, project), Style::default())));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_head_is_symmetric_and_the_expressions_only_touch_the_face() {
        let base = grid(Eyes::Open, Mouth::Smile);
        assert_eq!(base.len(), 15);
        for row in &base {
            assert_eq!(row.len(), WIDTH);
        }
        let blink = grid(Eyes::Blink, Mouth::Smile);
        for (y, (a, b)) in base.iter().zip(&blink).enumerate() {
            for x in 0..WIDTH {
                if a[x] != b[x] {
                    assert!((7..=8).contains(&y), "a blink changes the eyes only ({x},{y})");
                }
            }
        }
        let shape = |g: &Vec<Vec<u8>>| g.iter().map(|r| r.iter().map(|c| *c != b'.').collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(shape(&base), shape(&grid(Eyes::Happy, Mouth::Open)), "the outline never moves");
    }

    #[test]
    fn eight_rows_of_half_blocks_and_nothing_without_colors() {
        let rows = rows(Eyes::Open, Mouth::Smile, ColorDepth::TrueColor);
        assert_eq!(rows.len(), ROWS);
        assert!(rows.iter().all(|r| r.width() == WIDTH));
        assert!(super::rows(Eyes::Open, Mouth::Smile, ColorDepth::None).is_empty());
        assert!(!super::rows(Eyes::Open, Mouth::Smile, ColorDepth::Ansi256).is_empty());
    }

    #[test]
    fn it_pops_up_says_hello_then_blinks() {
        assert_eq!(pose(Duration::ZERO).0, 3, "starts lower");
        assert_eq!(pose(Duration::from_millis(500)), (0, Eyes::Happy, Mouth::Open));
        assert_eq!(pose(Duration::from_millis(1650)).1, Eyes::Blink);
        assert_eq!(pose(Duration::from_millis(3000)), (0, Eyes::Open, Mouth::Smile));
    }

    #[test]
    fn every_greeting_names_the_user_in_both_languages() {
        for lang in [po_i18n::Lang::Fr, po_i18n::Lang::En] {
            let all = po_i18n::text_in("view.splash.greetings", lang).unwrap();
            let lines: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
            assert!(lines.len() >= 8, "enough variety");
            assert!(lines.iter().all(|l| l.contains("{user}")), "{lang:?}");
        }
    }
}
