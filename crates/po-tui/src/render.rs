//! Conversation layout: from logical lines to screen rows.
//!
//! The conversation reads in **blocks**: the user's message, the reasoning, the tool calls, the
//! answer, the command reports. Each block has its mark on the left and its color, and an empty
//! line separates it from the next. The tools of a same step fold into one line; so does the
//! reasoning. Ctrl+O expands everything. The verbosity (`/verbosity`) sets the default: `quiet`
//! folds all the work of a turn into a single line, `verbose` expands everything.
//!
//! Wrapping of overlong lines is done here, not by ratatui: the left mark must repeat on every
//! screen row of a block, otherwise the rest of a paragraph restarts against the edge and the
//! eye loses the block it belongs to.

use po_term::UnicodeLevel;
use po_term::width::display_width;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TuiLine, Span};

use crate::state::{Line, LineKind};
use crate::view::{Palette, markdown_line};

/// How much of the work of a turn the conversation shows by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// The reasoning, tool calls and subagents of a turn fold into one line; the answer stays.
    Quiet,
    /// Each step folds on its own: one line per reasoning, one per group of tool calls.
    #[default]
    Normal,
    /// Everything expanded, as with Ctrl+O.
    Verbose,
}

/// The verbosity in force; `u8::MAX` until read from the preferences.
static VERBOSITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

impl Verbosity {
    pub const ALL: [Self; 3] = [Self::Quiet, Self::Normal, Self::Verbose];

    /// Accepts the English names and their French equivalents.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "quiet" | "minimal" | "discret" | "calme" | "0" => Some(Self::Quiet),
            "normal" | "1" => Some(Self::Normal),
            "verbose" | "detailed" | "full" | "detaille" | "détaillé" | "complet" | "2" => Some(Self::Verbose),
            _ => None,
        }
    }

    /// Stored name, the same in every language.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Normal => "normal",
            Self::Verbose => "verbose",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Quiet => po_i18n::t!("view.verbosity.quiet"),
            Self::Normal => po_i18n::t!("view.verbosity.normal"),
            Self::Verbose => po_i18n::t!("view.verbosity.verbose"),
        }
    }

    /// The level in force, read once from the preferences (`ui.json`).
    pub fn current() -> Self {
        use std::sync::atomic::Ordering;
        match VERBOSITY.load(Ordering::Relaxed) {
            0 => Self::Quiet,
            1 => Self::Normal,
            2 => Self::Verbose,
            _ => {
                let level =
                    po_i18n::prefs::get_str("verbosity").and_then(|v| Self::parse(&v)).unwrap_or_default();
                VERBOSITY.store(level as u8, Ordering::Relaxed);
                level
            }
        }
    }

    /// Applies the level right away and keeps it for the next sessions.
    pub fn set(self) -> std::io::Result<()> {
        VERBOSITY.store(self as u8, std::sync::atomic::Ordering::Relaxed);
        po_i18n::prefs::set_str("verbosity", self.as_str())
    }
}

/// What decides the appearance, apart from the lines themselves.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub width: u16,
    pub unicode: UnicodeLevel,
    /// Ctrl+O: reasoning and tool groups expanded.
    pub expanded: bool,
    /// A turn is in progress: the last block is live (streamed reasoning, running tool).
    pub live: bool,
    /// Animation frame, for running tools.
    pub frame: usize,
    pub verbosity: Verbosity,
}

impl Options {
    /// Expansion as set by Ctrl+O, inverted at the `verbose` level: there, everything starts
    /// expanded and Ctrl+O folds.
    pub fn new(width: u16, unicode: UnicodeLevel, expanded: bool, live: bool, frame: usize) -> Self {
        let verbosity = Verbosity::current();
        Self { width, unicode, expanded: expanded ^ (verbosity == Verbosity::Verbose), live, frame, verbosity }
    }
}

/// Family of a line: two consecutive lines of the same family form a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    User,
    Assistant,
    Thinking,
    Tools,
    /// Delegation to a subagent: its own block, so its goal stays readable.
    Agent,
    Meta,
    Card,
    TurnEnd,
}

fn family(kind: LineKind) -> Family {
    match kind {
        LineKind::User => Family::User,
        LineKind::Assistant => Family::Assistant,
        LineKind::Thinking | LineKind::ThinkingSummary => Family::Thinking,
        LineKind::Tool | LineKind::ToolRunning | LineKind::ToolError => Family::Tools,
        LineKind::Delegation => Family::Agent,
        LineKind::Notice | LineKind::Error => Family::Meta,
        LineKind::CardTitle
            | LineKind::Card
            | LineKind::CardWarn
            | LineKind::CardSection
            | LineKind::CardItem
            | LineKind::CardField
            | LineKind::CardDim => Family::Card,
        LineKind::Summary => Family::TurnEnd,
    }
}

/// The work of a turn, as opposed to what is said: reasoning, tool calls, subagents.
fn is_work(family: Family) -> bool {
    matches!(family, Family::Thinking | Family::Tools | Family::Agent)
}

/// Blocks for the given verbosity: at the `quiet` level, consecutive work blocks merge into
/// one, folded into a single line.
fn blocks_for(lines: &[Line], verbosity: Verbosity) -> Vec<(usize, usize)> {
    let all = blocks(lines);
    if verbosity != Verbosity::Quiet {
        return all;
    }
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(all.len());
    for (start, end) in all {
        if let Some(last) = out.last_mut()
            && is_work(family(lines[last.0].kind))
            && is_work(family(lines[start].kind))
        {
            last.1 = end;
        } else {
            out.push((start, end));
        }
    }
    out
}

/// Block bounds, in order: `(start, end exclusive)`.
fn blocks(lines: &[Line]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=lines.len() {
        let boundary = i == lines.len()
            || family(lines[i].kind) != family(lines[start].kind)
            // A title always opens a new card, a header a new reasoning block.
            || lines[i].kind == LineKind::CardTitle
            || lines[i].kind == LineKind::ThinkingSummary
            || lines[i].kind == LineKind::Summary
            || lines[i].kind == LineKind::Delegation;
        if boundary {
            if start < i {
                out.push((start, i));
            }
            start = i;
        }
    }
    out
}

/// Symbols, with their ASCII fallback.
struct Glyphs {
    gutter: &'static str,
    thought: &'static str,
    ok: &'static str,
    fail: &'static str,
    folded: &'static str,
    unfolded: &'static str,
    agent: &'static str,
    meta: &'static str,
    rule: &'static str,
    spinner: &'static [&'static str],
    boxed: bool,
}

impl Glyphs {
    fn for_level(unicode: UnicodeLevel) -> Self {
        if unicode >= UnicodeLevel::Bmp {
            Self {
                gutter: "▌ ",
                thought: "┊ ",
                ok: "✓",
                fail: "✗",
                folded: "▸",
                unfolded: "▾",
                agent: "◆",
                meta: "·",
                rule: "─",
                spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
                boxed: true,
            }
        } else {
            Self {
                gutter: "| ",
                thought: ": ",
                ok: "+",
                fail: "x",
                folded: ">",
                unfolded: "v",
                agent: "*",
                meta: "-",
                rule: "-",
                spinner: &["|", "/", "-", "\\"],
                boxed: false,
            }
        }
    }
}

/// A colored wave was drawn: the loop then redraws regularly to animate it.
static WAVE_DRAWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True if a wave was drawn since the last call.
pub fn take_wave() -> bool {
    WAVE_DRAWN.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Text in a scrolling wave of colors: reserved for what should stand out (Po Labs).
/// In true color, a continuous gradient; otherwise, the six basic colors.
pub fn wave(text: &str, colors: &Palette, bold: bool) -> Vec<Span<'static>> {
    WAVE_DRAWN.store(true, std::sync::atomic::Ordering::Relaxed);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f32();
    let modifier = if bold { Modifier::BOLD } else { Modifier::empty() };
    const BASIC: [Color; 6] = [Color::Red, Color::Yellow, Color::Green, Color::Cyan, Color::Blue, Color::Magenta];
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let phase = (i as f32 * 0.09 - now * 0.45).rem_euclid(1.0);
            let color = if colors.truecolor() {
                let (r, g, b) = hue(phase);
                Color::Rgb(r, g, b)
            } else {
                BASIC[(phase * 6.0) as usize % 6]
            };
            Span::styled(c.to_string(), colors.style(color, false).add_modifier(modifier))
        })
        .collect()
}

/// Hue (0 to 1) as a vivid color.
fn hue(h: f32) -> (u8, u8, u8) {
    let x = h * 6.0;
    let f = x - x.floor();
    let (r, g, b) = match x as u32 % 6 {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    };
    // A bit of white: readable on dark and light backgrounds alike.
    let lift = |v: f32| (80.0 + v * 175.0) as u8;
    (lift(r), lift(g), lift(b))
}

/// Rows of a reasoning block in progress shown live.
const LIVE_THINKING_ROWS: usize = 4;

/// Screen rows of the last blocks, enough to fill `wanted` rows, and whether the start of the
/// conversation was reached. Laying out a whole long session at every frame would be costly for
/// lines nobody sees.
/// Screen rows and, for each, the key of the block a touch expands or folds.
pub struct Rows {
    pub rows: Vec<TuiLine<'static>>,
    /// Key of the block (absolute position of its first line) when it expands.
    pub keys: Vec<Option<usize>>,
    pub reached_start: bool,
}

/// A block that has something folded: reasoning, tools, subagent.
fn foldable(kind: LineKind) -> bool {
    matches!(family(kind), Family::Thinking | Family::Tools | Family::Agent | Family::Card)
}

/// Like [`tail_rows`], with blocks toggled one by one (touch, click): `toggled(key)` says
/// whether a block is in the opposite state of the global setting (Ctrl+O). `offset` converts
/// indices into absolute positions, stable when the oldest lines drop out.
pub fn tail_rows_keyed(
    lines: &[Line],
    opts: &Options,
    colors: &Palette,
    wanted: usize,
    offset: usize,
    toggled: &dyn Fn(usize) -> bool,
) -> Rows {
    let all = blocks_for(lines, opts.verbosity);
    let glyphs = Glyphs::for_level(opts.unicode);
    let mut chunks: Vec<(Vec<TuiLine<'static>>, Option<usize>)> = Vec::new();
    let mut count = 0;
    let mut index = all.len();
    while index > 0 && count < wanted {
        index -= 1;
        let (start, end) = all[index];
        let block = &lines[start..end];
        let is_last = index + 1 == all.len();
        let po_header = family(block[0].kind) == Family::Assistant
            && all[..index]
                .iter()
                .rev()
                .map(|&(s, _)| family(lines[s].kind))
                .find(|f| matches!(f, Family::User | Family::Assistant))
                != Some(Family::Assistant);
        let key = foldable(block[0].kind).then_some(offset + start);
        let block_opts = Options { expanded: opts.expanded ^ key.is_some_and(toggled), ..*opts };
        let mut rows = render_block(block, &block_opts, colors, &glyphs, is_last, po_header);
        if index > 0 {
            rows.insert(0, TuiLine::from(""));
        }
        count += rows.len();
        chunks.push((rows, key));
    }
    let mut out = Rows { rows: Vec::with_capacity(count), keys: Vec::with_capacity(count), reached_start: index == 0 };
    for (chunk, key) in chunks.into_iter().rev() {
        for (i, row) in chunk.into_iter().enumerate() {
            // The empty separator line belongs to no block.
            out.keys.push(if i == 0 && row.spans.is_empty() { None } else { key });
            out.rows.push(row);
        }
    }
    out
}

pub fn tail_rows(lines: &[Line], opts: &Options, colors: &Palette, wanted: usize) -> (Vec<TuiLine<'static>>, bool) {
    let all = blocks_for(lines, opts.verbosity);
    let glyphs = Glyphs::for_level(opts.unicode);
    let mut chunks: Vec<Vec<TuiLine<'static>>> = Vec::new();
    let mut count = 0;
    let mut index = all.len();
    while index > 0 && count < wanted {
        index -= 1;
        let (start, end) = all[index];
        let block = &lines[start..end];
        let is_last = index + 1 == all.len();
        // "Po" is announced only once per turn: on the first answer following a user message.
        let po_header = family(block[0].kind) == Family::Assistant
            && all[..index]
                .iter()
                .rev()
                .map(|&(s, _)| family(lines[s].kind))
                .find(|f| matches!(f, Family::User | Family::Assistant))
                != Some(Family::Assistant);
        let mut rows = render_block(block, opts, colors, &glyphs, is_last, po_header);
        if index > 0 {
            rows.insert(0, TuiLine::from(""));
        }
        count += rows.len();
        chunks.push(rows);
    }
    let reached_start = index == 0;
    let mut out = Vec::with_capacity(count);
    for chunk in chunks.into_iter().rev() {
        out.extend(chunk);
    }
    (out, reached_start)
}

fn render_block(
    block: &[Line],
    opts: &Options,
    colors: &Palette,
    g: &Glyphs,
    is_last: bool,
    po_header: bool,
) -> Vec<TuiLine<'static>> {
    if opts.verbosity == Verbosity::Quiet && is_work(family(block[0].kind)) {
        return render_work(block, opts, colors, g, is_last);
    }
    let width = opts.width.max(8) as usize;
    let dim = colors.style(Color::DarkGray, true);
    let mut rows = Vec::new();
    match family(block[0].kind) {
        Family::User => {
            let mark = colors.style(Color::Cyan, false);
            rows.push(TuiLine::from(vec![
                Span::styled(g.gutter, mark),
                Span::styled(po_i18n::t!("view.user.name"), mark.add_modifier(Modifier::BOLD)),
            ]));
            for line in block {
                push_wrapped(&mut rows, vec![Span::raw(line.text.clone())], Span::styled(g.gutter, mark), width);
            }
        }
        Family::Assistant => {
            let mark = colors.style(Color::Green, false);
            if po_header {
                rows.push(TuiLine::from(vec![
                    Span::styled(g.gutter, mark),
                    Span::styled("Po", mark.add_modifier(Modifier::BOLD)),
                ]));
            }
            let mut in_code = false;
            for line in block {
                let spans = markdown_line(&line.text, &mut in_code, opts.unicode, colors).spans;
                push_wrapped(&mut rows, spans, Span::styled(g.gutter, mark), width);
            }
        }
        Family::Thinking => {
            let style = colors.style(Color::DarkGray, true).add_modifier(Modifier::ITALIC);
            let header = block
                .iter()
                .find(|l| l.kind == LineKind::ThinkingSummary)
                .map(|l| l.text.clone())
                .unwrap_or_else(|| po_i18n::t!("view.thinking.header").to_owned());
            // A reasoning block still being written: its header has no duration. The criterion does
            // not depend on the language: a closed header always carries "· duration · tokens".
            let thinking_now = opts.live && is_last && !header.contains(" · ");
            rows.push(TuiLine::from(vec![Span::styled(g.thought, dim), Span::styled(header, dim)]));
            if opts.expanded || thinking_now {
                // Streamed, the reasoning is made of lines; received in one piece, it is stored
                // folded in the header.
                let folded = block.iter().filter_map(|l| l.detail.as_deref()).flat_map(str::lines).map(str::to_owned);
                let streamed = block.iter().filter(|l| l.kind == LineKind::Thinking).map(|l| l.text.clone());
                let mut body = Vec::new();
                for text in streamed.chain(folded).collect::<Vec<_>>() {
                    push_wrapped(&mut body, vec![Span::styled(text, style)], Span::styled(g.thought, dim), width);
                }
                // Live, only its last lines: a long reasoning used to push everything else off the
                // screen, approval request included (observed). Once finished, it folds under its
                // header; Ctrl+O or a touch expands it entirely.
                if !opts.expanded && body.len() > LIVE_THINKING_ROWS {
                    body.drain(..body.len() - LIVE_THINKING_ROWS);
                }
                rows.extend(body);
            }
        }
        Family::Tools => render_tools(&mut rows, block, opts, colors, g, width),
        Family::Agent => {
            // "◆ explorer · model", then the goal: its first sentence, or the whole brief once
            // expanded.
            let accent = colors.style(Color::Magenta, false);
            for line in block {
                let (name, model) = line.text.split_once(" · ").unwrap_or((line.text.as_str(), ""));
                let mut head = vec![
                    Span::styled(format!("{} ", g.agent), accent),
                    Span::styled(name.to_owned(), accent.add_modifier(Modifier::BOLD)),
                ];
                if !model.is_empty() {
                    head.push(Span::styled(format!(" · {model}"), dim));
                }
                rows.push(TuiLine::from(head));
                let task = line.detail.as_deref().unwrap_or("");
                // Folded: what the subagent says it is doing, written by it live. The whole brief,
                // often long, is read expanded.
                let preview = match &line.progress {
                    _ if opts.expanded => None,
                    Some(progress) => Some(progress.clone()),
                    None if !task.is_empty() => Some(goal_summary(task)),
                    None => None,
                };
                if opts.expanded {
                    for part in task.lines().filter(|l| !l.trim().is_empty()) {
                        push_wrapped(&mut rows, vec![Span::styled(part.to_owned(), dim)], Span::raw("  "), width);
                    }
                } else if let Some(preview) = preview {
                    let mut wrapped = wrap_spans(
                        vec![Span::styled(preview, dim.add_modifier(Modifier::ITALIC))],
                        width.saturating_sub(2),
                    );
                    if wrapped.len() > 2 {
                        wrapped.truncate(2);
                        if let Some(last) = wrapped.last_mut() {
                            last.push(Span::styled(if g.boxed { "…" } else { "~" }, dim));
                        }
                    }
                    for mut row in wrapped {
                        row.insert(0, Span::raw("  "));
                        rows.push(TuiLine::from(row));
                    }
                }
            }
        }
        Family::Meta => {
            for line in block {
                let (mark, style) = match line.kind {
                    LineKind::Error => (g.fail, colors.style(Color::Yellow, false)),
                    _ => (g.meta, dim),
                };
                push_wrapped(
                    &mut rows,
                    vec![Span::styled(line.text.clone(), style)],
                    Span::styled(format!("  {mark} "), style),
                    width,
                );
            }
        }
        Family::Card => render_card(&mut rows, block, opts.expanded, colors, g, width),
        Family::TurnEnd => {
            let text = format!(" {} ", block[0].text);
            let room = width.saturating_sub(display_width(&text, UnicodeLevel::Full) + 2);
            if room < 4 {
                push_wrapped(&mut rows, vec![Span::styled(block[0].text.clone(), dim)], Span::styled("  ", dim), width);
            } else {
                rows.push(TuiLine::from(Span::styled(
                    format!("{}{text}{}", g.rule.repeat(room), g.rule.repeat(2)),
                    dim,
                )));
            }
        }
    }
    rows
}

/// The work of a turn at the `quiet` level: one line that counts it, then — while it runs — what
/// is happening now. Expanded, the steps are shown as at the `normal` level.
fn render_work(block: &[Line], opts: &Options, colors: &Palette, g: &Glyphs, is_last: bool) -> Vec<TuiLine<'static>> {
    let normal = Options { verbosity: Verbosity::Normal, ..*opts };
    if opts.expanded {
        let parts = blocks(block);
        let mut rows = Vec::new();
        for (i, &(start, end)) in parts.iter().enumerate() {
            if i > 0 {
                rows.push(TuiLine::from(""));
            }
            rows.extend(render_block(&block[start..end], &normal, colors, g, is_last && i + 1 == parts.len(), false));
        }
        return rows;
    }

    let dim = colors.style(Color::DarkGray, true);
    // A delegation is counted once, as a subagent: its `task` call is not another tool.
    let tools = block
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Tool | LineKind::ToolRunning | LineKind::ToolError))
        .filter(|l| !l.text.starts_with("task "))
        .count();
    let failures = block.iter().filter(|l| l.kind == LineKind::ToolError).count();
    let thoughts = block.iter().filter(|l| l.kind == LineKind::ThinkingSummary).count();
    let agents = block.iter().filter(|l| l.kind == LineKind::Delegation).count();
    let total: f64 = block.iter().filter_map(|l| l.secs).sum();
    let running = opts.live && is_last;

    let mut parts = Vec::new();
    if thoughts > 0 {
        parts.push(if thoughts > 1 {
            po_i18n::t!("view.work.thoughts_many", n = thoughts)
        } else {
            po_i18n::t!("view.work.thoughts_one", n = thoughts)
        });
    }
    if tools > 0 {
        parts.push(if tools > 1 {
            po_i18n::t!("view.tools.count_many", n = tools)
        } else {
            po_i18n::t!("view.tools.count_one", n = tools)
        });
    }
    if agents > 0 {
        parts.push(if agents > 1 {
            po_i18n::t!("view.work.agents_many", n = agents)
        } else {
            po_i18n::t!("view.work.agents_one", n = agents)
        });
    }
    let icon = if running { g.spinner[opts.frame % g.spinner.len()] } else { g.folded };
    let mut header = vec![
        Span::styled(format!("  {icon} "), if running { colors.style(Color::Cyan, false) } else { dim }),
        Span::styled(po_i18n::t!("view.work.label"), colors.style(Color::Gray, false).add_modifier(Modifier::BOLD)),
    ];
    if !parts.is_empty() {
        header.push(Span::styled(format!(" · {}", parts.join(" · ")), colors.style(Color::Gray, false)));
    }
    if failures > 0 {
        header.push(Span::styled(
            format!(
                " · {}",
                if failures > 1 { po_i18n::t!("view.tools.fail_many", n = failures) } else { po_i18n::t!("view.tools.fail_one", n = failures) }
            ),
            colors.style(Color::Red, false),
        ));
    }
    if total >= 0.05 {
        header.push(Span::styled(format!(" · {}", seconds(total)), dim));
    }
    let width = opts.width.max(8) as usize;
    let mut rows = vec![TuiLine::from(truncate_spans(header, width, g))];

    // While it runs, the step in progress, on one line: a turn that only showed a counter would
    // look stuck.
    if running && let Some(last) = block.last() {
        let now = match last.kind {
            LineKind::Thinking | LineKind::ThinkingSummary => block
                .iter()
                .rev()
                .find(|l| l.kind == LineKind::ThinkingSummary)
                .map(|l| l.text.clone())
                .unwrap_or_else(|| po_i18n::t!("view.thinking.header").to_owned()),
            LineKind::Delegation => last.progress.clone().unwrap_or_else(|| last.text.clone()),
            _ => last.text.clone(),
        };
        let row = vec![Span::raw("    "), Span::styled(now, dim.add_modifier(Modifier::ITALIC))];
        rows.push(TuiLine::from(truncate_spans(row, width, g)));
    }
    rows
}

/// A single call is displayed as is; several fold into one summary line, except while they run
/// or when the user has expanded everything.
fn render_tools(
    rows: &mut Vec<TuiLine<'static>>,
    block: &[Line],
    opts: &Options,
    colors: &Palette,
    g: &Glyphs,
    width: usize,
) {
    let dim = colors.style(Color::DarkGray, true);
    let running = block.iter().any(|l| l.kind == LineKind::ToolRunning);
    let calls = block.iter().filter(|l| l.kind != LineKind::Delegation).count();
    let failures = block.iter().filter(|l| l.kind == LineKind::ToolError).count();
    let total: f64 = block.iter().filter_map(|l| l.secs).sum();
    let unfold = opts.expanded || running || block.len() == 1;

    if block.len() > 1 {
        let mut header = vec![
            Span::styled(format!("  {} ", if unfold { g.unfolded } else { g.folded }), dim),
            Span::styled(
                if calls > 1 { po_i18n::t!("view.tools.count_many", n = calls) } else { po_i18n::t!("view.tools.count_one", n = calls) },
                colors.style(Color::Gray, false),
            ),
        ];
        if failures > 0 {
            header.push(Span::styled(
                format!(
                    " · {}",
                    if failures > 1 { po_i18n::t!("view.tools.fail_many", n = failures) } else { po_i18n::t!("view.tools.fail_one", n = failures) }
                ),
                colors.style(Color::Red, false),
            ));
        }
        if total >= 0.05 {
            header.push(Span::styled(format!(" · {}", seconds(total)), dim));
        }
        rows.push(TuiLine::from(header));
    }
    if !unfold {
        return;
    }
    let indent = if block.len() > 1 { "    " } else { "  " };
    for line in block {
        let (icon, icon_style) = match line.kind {
            LineKind::ToolRunning => (g.spinner[opts.frame % g.spinner.len()], colors.style(Color::Cyan, false)),
            LineKind::ToolError => (g.fail, colors.style(Color::Red, false)),
            LineKind::Delegation => (g.agent, colors.style(Color::Magenta, false)),
            _ => (g.ok, colors.style(Color::Green, false)),
        };
        let mut spans = vec![Span::styled(format!("{icon} "), icon_style)];
        if line.kind == LineKind::Delegation {
            spans.push(Span::styled(line.text.clone(), colors.style(Color::Magenta, false)));
        } else {
            let (name, target) = line.text.split_once(' ').unwrap_or((line.text.as_str(), ""));
            let target = target.strip_prefix("$ ").unwrap_or(target);
            spans.push(Span::styled(name.to_owned(), Style::default().add_modifier(Modifier::BOLD)));
            if !target.is_empty() {
                spans.push(Span::styled(format!("  {target}"), dim));
            }
            // The `bash` summary already gives its duration ("code 0 in 0.8 s"): do not repeat it.
            let has_duration = target.ends_with(" s") && (target.contains(" en ") || target.contains(" in "));
            if let Some(secs) = line.secs.filter(|s| *s >= 0.05 && !has_duration) {
                spans.push(Span::styled(format!(" · {}", seconds(secs)), dim));
            }
        }
        // A call fits on one line, cut with a "…"; Ctrl+O shows it in full.
        if opts.expanded {
            push_wrapped(rows, spans, Span::raw(indent), width);
        } else {
            let mut row = vec![Span::raw(indent)];
            row.extend(truncate_spans(spans, width.saturating_sub(indent.len()), g));
            rows.push(TuiLine::from(row));
        }
    }
}

/// Cuts a screen row to `width` columns (status bar, headers).
pub fn clip(spans: Vec<Span<'static>>, width: usize, unicode: UnicodeLevel) -> Vec<Span<'static>> {
    truncate_spans(spans, width, &Glyphs::for_level(unicode))
}

/// Cuts styled fragments to `width` columns, ending with "…" when text is missing.
fn truncate_spans(spans: Vec<Span<'static>>, width: usize, g: &Glyphs) -> Vec<Span<'static>> {
    let ellipsis = if g.boxed { "…" } else { "~" };
    let total: usize = spans.iter().map(|s| display_width(&s.content, UnicodeLevel::Full)).sum();
    if total <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut left = width.saturating_sub(1);
    for span in spans {
        if left == 0 {
            break;
        }
        let mut text = String::new();
        for c in span.content.chars() {
            let cw = display_width(c.encode_utf8(&mut [0; 4]), UnicodeLevel::Full);
            if cw > left {
                left = 0;
                break;
            }
            text.push(c);
            left -= cw;
        }
        out.push(Span::styled(text, span.style));
    }
    out.push(Span::raw(ellipsis));
    out
}

/// Items shown of a folded list; a touch (or Ctrl+O) shows the rest.
const FOLDED_ITEMS: usize = 8;
/// Rows of a folded description.
const FOLDED_DETAIL_ROWS: usize = 2;

/// Command report: boxed when space allows, simply indented otherwise.
///
/// Structured lines adapt to the width: an item (name and description) fits on two columns when
/// the screen is wide, and stacks — name, then indented description — on a phone. Folded, the
/// card shows the first items and cuts long descriptions; a touch expands it fully.
fn render_card(rows: &mut Vec<TuiLine<'static>>, block: &[Line], expanded: bool, colors: &Palette, g: &Glyphs, width: usize) {
    let border = colors.style(Color::DarkGray, true);
    let title_style = colors.style(Color::Cyan, false).add_modifier(Modifier::BOLD);
    let title = block.iter().find(|l| l.kind == LineKind::CardTitle).map(|l| l.text.clone()).unwrap_or_default();
    let body: Vec<&Line> = block.iter().filter(|l| l.kind != LineKind::CardTitle).collect();
    let boxed = g.boxed && width >= 30;
    let inner = if boxed { width - 4 } else { width.saturating_sub(2) };
    let content = card_body(&body, inner, expanded, colors, g);

    if !boxed {
        if !title.is_empty() {
            rows.push(TuiLine::from(vec![
                Span::styled(format!("{} ", g.folded), border),
                Span::styled(title, title_style),
            ]));
        }
        for mut row in content {
            row.insert(0, Span::raw("  "));
            rows.push(TuiLine::from(row));
        }
        return;
    }

    // A too long title (command with arguments) is cut: it must not break through the frame.
    let title = po_term::width::truncate(&title, width.saturating_sub(6), UnicodeLevel::Full).to_owned();
    let title_text = if title.is_empty() { String::new() } else { format!(" {title} ") };
    let used = display_width(&title_text, UnicodeLevel::Full);
    let mut top = vec![Span::styled("╭─", border)];
    if title == po_i18n::t!("labs.card_title") {
        top.extend(wave(&title_text, colors, true));
    } else {
        top.push(Span::styled(title_text, title_style));
    }
    top.push(Span::styled(format!("{}╮", "─".repeat((width - 3).saturating_sub(used))), border));
    rows.push(TuiLine::from(top));
    for mut row in content {
        let filled: usize = row.iter().map(|s| display_width(&s.content, UnicodeLevel::Full)).sum();
        row.insert(0, Span::styled("│ ", border));
        row.push(Span::raw(" ".repeat(inner.saturating_sub(filled))));
        row.push(Span::styled(" │", border));
        rows.push(TuiLine::from(row));
    }
    rows.push(TuiLine::from(Span::styled(format!("╰{}╯", "─".repeat(width - 2)), border)));
}

/// Screen rows of a card's content, without its frame.
fn card_body(body: &[&Line], inner: usize, expanded: bool, colors: &Palette, g: &Glyphs) -> Vec<Vec<Span<'static>>> {
    let dim = colors.style(Color::DarkGray, true);
    let name_style = colors.style(Color::Cyan, false).add_modifier(Modifier::BOLD);
    let ellipsis = if g.boxed { "…" } else { "~" };
    let width_of = |text: &str| display_width(text, UnicodeLevel::Full);
    // Two columns when the screen is wide and the name leaves real width to its description; a
    // name that is too long goes above, without making the others stack.
    let name_limit = (inner / 3).min(26);
    let name_column = body
        .iter()
        .filter(|l| l.kind == LineKind::CardItem)
        .map(|l| width_of(&l.text))
        .filter(|w| *w <= name_limit)
        .max()
        .unwrap_or(0);
    let key_column = body.iter().filter(|l| l.kind == LineKind::CardField).map(|l| width_of(&l.text)).max().unwrap_or(0).min(20);
    let fields_inline = key_column + 2 < inner / 2;

    let folded = |mut rows: Vec<Vec<Span<'static>>>| {
        if !expanded && rows.len() > FOLDED_DETAIL_ROWS {
            rows.truncate(FOLDED_DETAIL_ROWS);
            if let Some(last) = rows.last_mut() {
                last.push(Span::styled(ellipsis, dim));
            }
        }
        rows
    };

    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    // The cap applies per section: a long first section must not empty the others.
    let mut items = 0;
    let mut hidden = 0;
    let more = |out: &mut Vec<Vec<Span<'static>>>, hidden: &mut usize| {
        if *hidden > 0 {
            out.push(vec![Span::styled(
                format!("{ellipsis} {}", po_i18n::t!("view.card.more", n = hidden)),
                dim.add_modifier(Modifier::ITALIC),
            )]);
            *hidden = 0;
        }
    };
    for line in body {
        if line.kind != LineKind::CardItem {
            more(&mut out, &mut hidden);
        }
        match line.kind {
            LineKind::CardSection => {
                items = 0;
                if !out.is_empty() {
                    out.push(Vec::new());
                }
                out.push(vec![Span::styled(line.text.clone(), colors.style(Color::Magenta, false).add_modifier(Modifier::BOLD))]);
            }
            LineKind::CardItem => {
                items += 1;
                if !expanded && items > FOLDED_ITEMS {
                    hidden += 1;
                    continue;
                }
                let detail = line.detail.clone().unwrap_or_default();
                if inner >= 56 && width_of(&line.text) <= name_column {
                    let room = inner.saturating_sub(name_column + 2).max(8);
                    let desc = folded(if detail.is_empty() { vec![Vec::new()] } else { wrap_spans(vec![Span::styled(detail, dim)], room) });
                    for (i, mut row) in desc.into_iter().enumerate() {
                        let lead = if i == 0 {
                            let pad = name_column.saturating_sub(width_of(&line.text));
                            vec![Span::styled(line.text.clone(), name_style), Span::raw(" ".repeat(pad + 2))]
                        } else {
                            vec![Span::raw(" ".repeat(name_column + 2))]
                        };
                        let mut full = lead;
                        full.append(&mut row);
                        out.push(full);
                    }
                } else {
                    for row in wrap_spans(vec![Span::styled(line.text.clone(), name_style)], inner) {
                        out.push(row);
                    }
                    if !detail.is_empty() {
                        for mut row in folded(wrap_spans(vec![Span::styled(detail, dim)], inner.saturating_sub(2))) {
                            row.insert(0, Span::raw("  "));
                            out.push(row);
                        }
                    }
                }
            }
            LineKind::CardField => {
                let value = line.detail.clone().unwrap_or_default();
                if fields_inline {
                    let room = inner.saturating_sub(key_column + 2).max(8);
                    for (i, mut row) in wrap_spans(vec![Span::raw(value)], room).into_iter().enumerate() {
                        let mut full = if i == 0 {
                            let pad = key_column.saturating_sub(width_of(&line.text));
                            vec![Span::styled(line.text.clone(), dim), Span::raw(" ".repeat(pad + 2))]
                        } else {
                            vec![Span::raw(" ".repeat(key_column + 2))]
                        };
                        full.append(&mut row);
                        out.push(full);
                    }
                } else {
                    out.push(vec![Span::styled(line.text.clone(), dim)]);
                    for mut row in wrap_spans(vec![Span::raw(value)], inner.saturating_sub(2)) {
                        row.insert(0, Span::raw("  "));
                        out.push(row);
                    }
                }
            }
            kind => {
                let style = match kind {
                    LineKind::CardWarn => colors.style(Color::Yellow, false),
                    LineKind::CardDim => dim.add_modifier(Modifier::ITALIC),
                    _ => Style::default(),
                };
                out.extend(wrap_spans(vec![Span::styled(line.text.clone(), style)], inner));
            }
        }
    }
    more(&mut out, &mut hidden);
    out
}

/// Goal of a subagent in one sentence: the first of its brief, bounded.
pub fn goal_summary(task: &str) -> String {
    let flat = task.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .char_indices()
        .find(|&(i, c)| matches!(c, '.' | '!' | '?') && flat[i + c.len_utf8()..].starts_with(' '))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(flat.len());
    let sentence = flat[..end].trim_end_matches(['.', '!', '?']);
    // Details in parentheses or after a comma (paths, formatting instructions) lengthen the
    // text without saying anything about the goal: observed, "Analyze the Strata code, located
    // in E:\LLM\Strata (bash path: … " overflowed onto three lines.
    // Parentheses are removed, not cut at: "In the workspace (…), read a.txt" lost its verb when
    // cutting before them (observed).
    let mut stripped = String::new();
    let mut depth = 0usize;
    for c in sentence.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => stripped.push(c),
            _ => {}
        }
    }
    let stripped = stripped.split_whitespace().collect::<Vec<_>>().join(" ").replace(" ,", ",");
    let core = stripped.as_str();
    const MAX: usize = 80;
    if core.chars().count() <= MAX {
        return core.to_owned();
    }
    // Cut at the last whole word.
    let cut: String = core.chars().take(MAX).collect();
    let cut = cut.rsplit_once(' ').map(|(head, _)| head).unwrap_or(&cut);
    format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
}

fn seconds(secs: f64) -> String {
    if secs < 10.0 { format!("{secs:.1} s").replace('.', po_i18n::t!("view.meta.decimal")) } else { format!("{secs:.0} s") }
}

/// Adds the screen rows of a logical line, each preceded by the block's mark.
fn push_wrapped(rows: &mut Vec<TuiLine<'static>>, spans: Vec<Span<'static>>, prefix: Span<'static>, width: usize) {
    let room = width.saturating_sub(display_width(&prefix.content, UnicodeLevel::Full)).max(4);
    for mut row in wrap_spans(spans, room) {
        row.insert(0, prefix.clone());
        rows.push(TuiLine::from(row));
    }
}

/// Wraps styled fragments to `width` columns, at spaces only. A word can change style in its
/// middle (`` `LuaVault`, `` : code then a comma): the break is decided on the text, not on the
/// fragments — otherwise the comma went alone onto the next line (observed). A word longer than
/// the line is cut: better a cut path than an overflowing one. An empty line gives an empty
/// screen row, to keep paragraphs.
pub fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    // Indent of continuation lines: the continuation of a bullet or indented line aligns under
    // its text, not against the edge — otherwise a wrapped list reads like running text.
    let hang = hanging_indent(&spans).min(width / 2);

    // Words: leading spaces then non-blank characters, each keeping its style.
    type Word = Vec<(char, Style)>;
    let mut words: Vec<Word> = Vec::new();
    let mut current: Word = Vec::new();
    for span in &spans {
        for c in span.content.chars() {
            if c == ' ' && current.iter().any(|(ch, _)| *ch != ' ') {
                words.push(std::mem::take(&mut current));
            }
            current.push((c, span.style));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }

    let char_width = |c: char| display_width(c.encode_utf8(&mut [0; 4]), UnicodeLevel::Full);
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut used = 0usize;
    for word in words {
        let room = if rows.len() == 1 { width } else { (width - hang).max(1) };
        let w: usize = word.iter().map(|(c, _)| char_width(*c)).sum();
        if used + w <= room {
            used += w;
            rows.last_mut().expect("at least one row").extend(word);
            continue;
        }
        // The word does not fit: onto a new row, without its leading spaces.
        let word: Word = word.into_iter().skip_while(|(c, _)| *c == ' ').collect();
        if used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        for (c, style) in word {
            let room = if rows.len() == 1 { width } else { (width - hang).max(1) };
            let cw = char_width(c);
            if used + cw > room && used > 0 {
                rows.push(Vec::new());
                used = 0;
            }
            rows.last_mut().expect("at least one row").push((c, style));
            used += cw;
        }
    }

    // Back to fragments: consecutive characters of the same style are grouped.
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut out: Vec<Span<'static>> = Vec::new();
            if index > 0 && hang > 0 {
                out.push(Span::raw(" ".repeat(hang)));
            }
            let mut text = String::new();
            let mut style: Option<Style> = None;
            for (c, s) in row {
                if style.is_some_and(|current| current != s) {
                    out.push(Span::styled(std::mem::take(&mut text), style.expect("checked")));
                }
                style = Some(s);
                text.push(c);
            }
            if let Some(style) = style {
                out.push(Span::styled(text, style));
            }
            out
        })
        .collect()
}

/// Continuation indent of a line: its indentation, plus the width of its bullet if it has one
/// (`- `, `• `, `* `, `1. `).
fn hanging_indent(spans: &[Span<'static>]) -> usize {
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let rest = text.trim_start_matches(' ');
    let lead = text.len() - rest.len();
    let bullet = ["- ", "• ", "* ", "+ "].iter().find(|b| rest.starts_with(**b)).map(|b| b.chars().count());
    let numbered = {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && rest[digits..].starts_with(". ")).then_some(digits + 2)
    };
    lead + bullet.or(numbered).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_term::ColorDepth;

    fn text_of(row: &TuiLine) -> String {
        row.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(lines: &[Line], width: u16, expanded: bool) -> Vec<String> {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let opts = Options { width, unicode: UnicodeLevel::Full, expanded, live: false, frame: 0, verbosity: Verbosity::Normal };
        let (rows, start) = tail_rows(lines, &opts, &Palette::new(ColorDepth::None), usize::MAX);
        assert!(start);
        rows.iter().map(text_of).collect()
    }

    fn rendered_at(lines: &[Line], verbosity: Verbosity, expanded: bool, live: bool) -> Vec<String> {
        po_i18n::set_lang(po_i18n::Lang::En);
        let opts = Options { width: 70, unicode: UnicodeLevel::Full, expanded, live, frame: 0, verbosity };
        let (rows, _) = tail_rows(lines, &opts, &Palette::new(ColorDepth::None), usize::MAX);
        rows.iter().map(text_of).collect()
    }

    fn a_busy_turn() -> Vec<Line> {
        vec![
            Line::new(LineKind::User, "add a setting"),
            Line::new(LineKind::ThinkingSummary, "Thought · 4 s · 300 tokens"),
            Line::new(LineKind::Tool, "read a.rs").with_secs(0.2),
            Line::new(LineKind::Tool, "grep verbosity").with_secs(0.1),
            Line::new(LineKind::ThinkingSummary, "Thought · 2 s · 120 tokens"),
            Line::new(LineKind::ToolError, "bash cargo test").with_secs(3.0),
            Line::new(LineKind::Assistant, "Done: the setting is in."),
        ]
    }

    #[test]
    fn quiet_folds_the_work_of_a_turn_into_one_line() {
        let rows = rendered_at(&a_busy_turn(), Verbosity::Quiet, false, false);
        // Checked without the words: another test may switch the language meanwhile.
        let work: Vec<_> = rows.iter().filter(|r| r.contains("▸")).collect();
        assert_eq!(work.len(), 1, "{rows:#?}");
        assert!(work[0].contains("· 2 ") && work[0].contains("· 3 ") && work[0].contains("· 1 "), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("read") || r.contains("Thought")), "steps hidden: {rows:#?}");
        assert!(rows.iter().any(|r| r.contains("Done: the setting is in.")), "the answer stays");
    }

    #[test]
    fn quiet_expanded_shows_the_steps_and_live_shows_the_current_one() {
        let rows = rendered_at(&a_busy_turn(), Verbosity::Quiet, true, false);
        assert!(rows.iter().any(|r| r.contains("read")) && rows.iter().any(|r| r.contains("Thought")), "{rows:#?}");

        let mut running = a_busy_turn();
        running.pop();
        running.push(Line::new(LineKind::ToolRunning, "bash cargo build"));
        let rows = rendered_at(&running, Verbosity::Quiet, false, true);
        assert!(rows.iter().any(|r| r.contains("bash cargo build")), "{rows:#?}");
    }

    #[test]
    fn verbose_starts_expanded_and_ctrl_o_folds() {
        assert_eq!(Verbosity::parse("détaillé"), Some(Verbosity::Verbose));
        assert_eq!(Verbosity::parse("discret"), Some(Verbosity::Quiet));
        assert_eq!(Verbosity::parse("loud"), None);
        let normal = rendered_at(&a_busy_turn(), Verbosity::Normal, false, false);
        let verbose = rendered_at(&a_busy_turn(), Verbosity::Verbose, true, false);
        assert!(verbose.len() > normal.len(), "verbose shows more than normal");
    }

    #[test]
    fn wrapping_keeps_the_mark_on_every_row_and_never_overflows() {
        let lines = vec![Line::new(LineKind::User, "a message long enough to have to wrap on a narrow screen")];
        for row in rendered(&lines, 24, false) {
            assert!(row.starts_with("▌ "), "{row}");
            assert!(display_width(&row, UnicodeLevel::Full) <= 24, "{row}");
        }
    }

    #[test]
    fn blocks_are_separated_and_po_announces_itself_only_once() {
        let lines = vec![
            Line::new(LineKind::User, "hello"),
            Line::new(LineKind::Assistant, "let me look"),
            Line::new(LineKind::Tool, "read a.rs").with_secs(0.1),
            Line::new(LineKind::Assistant, "it is done"),
        ];
        let rows = rendered(&lines, 60, false);
        assert_eq!(rows.iter().filter(|r| r.trim_end() == "▌ Po").count(), 1, "{rows:#?}");
        assert!(rows.contains(&String::new()), "an empty line between two blocks");
        assert!(rows.iter().any(|r| r.contains("✓ read") && r.contains("a.rs")));
    }

    #[test]
    fn several_tools_fold_into_a_summary() {
        let lines = vec![
            Line::new(LineKind::Tool, "read a.rs").with_secs(0.2),
            Line::new(LineKind::ToolError, "bash cargo test").with_secs(3.0),
            Line::new(LineKind::Tool, "grep fn main").with_secs(0.1),
        ];
        let folded = rendered(&lines, 60, false);
        assert_eq!(folded.len(), 1, "{folded:#?}");
        assert!(
            folded[0].contains("3 outils") && folded[0].contains("1 échec") && folded[0].contains("3,3 s"),
            "{}",
            folded[0]
        );
        let expanded = rendered(&lines, 60, true);
        assert_eq!(expanded.len(), 4);
        assert!(expanded[2].contains("✗ bash"));
    }

    #[test]
    fn reasoning_stays_folded_unless_requested() {
        let lines = vec![
            Line::new(LineKind::ThinkingSummary, "Réflexion · 12 s · ~840 tokens"),
            Line::new(LineKind::Thinking, "weighing the options"),
        ];
        assert_eq!(rendered(&lines, 60, false), ["┊ Réflexion · 12 s · ~840 tokens"]);
        assert_eq!(rendered(&lines, 60, true).len(), 2);
    }

    #[test]
    fn a_card_is_boxed_at_the_exact_width() {
        let lines = vec![
            Line::new(LineKind::CardTitle, "/cache"),
            Line::new(LineKind::Card, "slot 0: free · 2436 tokens kept in cache"),
        ];
        let rows = rendered(&lines, 40, false);
        assert!(rows[0].starts_with("╭─ /cache "));
        for row in &rows {
            assert_eq!(display_width(row, UnicodeLevel::Full), 40, "{row}");
        }
        let long = vec![Line::new(LineKind::CardTitle, "/agents create a subagent that rereads every commit of the repository")];
        for row in rendered(&long, 40, false) {
            assert_eq!(display_width(&row, UnicodeLevel::Full), 40, "{row}");
        }
        let narrow = rendered(&lines, 20, false);
        assert!(narrow[0].contains("/cache") && !narrow[0].contains('╭'));
    }

    #[test]
    fn without_unicode_everything_stays_ascii() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let lines = vec![
            Line::new(LineKind::User, "hello"),
            Line::new(LineKind::Tool, "read a.rs"),
            Line::new(LineKind::Notice, "note"),
        ];
        let opts = Options { width: 40, unicode: UnicodeLevel::Ascii, expanded: false, live: false, frame: 0, verbosity: Verbosity::Normal };
        let (rows, _) = tail_rows(&lines, &opts, &Palette::new(ColorDepth::None), usize::MAX);
        for row in rows {
            assert!(text_of(&row).is_ascii(), "{}", text_of(&row));
        }
    }

    #[test]
    fn the_continuation_of_a_bullet_aligns_under_its_text() {
        let rows = wrap_spans(vec![Span::raw("  - read the code before editing it and respect its style")], 20);
        let texts: Vec<String> = rows.iter().map(|r| r.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(texts.len() >= 3, "{texts:?}");
        for row in &texts[1..] {
            assert!(row.starts_with("    ") && !row.starts_with("     "), "{row:?}");
            assert!(display_width(row, UnicodeLevel::Full) <= 20, "{row:?}");
        }
        let numbered = wrap_spans(vec![Span::raw("12. a numbered list item that is rather long")], 16);
        let rest: String = numbered[1].iter().map(|s| s.content.as_ref()).collect();
        assert!(rest.starts_with("    "), "{rest:?}");
    }

    #[test]
    fn a_subagents_goal_stays_short() {
        assert_eq!(
            goal_summary(r"Analyze the Strata project code, located in E:\LLM\Strata (bash path: /e/LLM/Strata). Then report."),
            r"Analyze the Strata project code, located in E:\LLM\Strata"
        );
        assert_eq!(
            goal_summary("In the workspace (test-project folder), read a.txt then b.txt. Return the content."),
            "In the workspace, read a.txt then b.txt"
        );
        let long = goal_summary(&"word ".repeat(60));
        assert!(long.ends_with('…') && long.chars().count() <= 81, "{long}");
        assert_eq!(goal_summary("Explore the project."), "Explore the project");
    }

    fn card(rows: Vec<Line>, width: u16, expanded: bool) -> Vec<String> {
        let mut all = vec![Line::new(LineKind::CardTitle, "/skills")];
        all.extend(rows);
        rendered(&all, width, expanded)
    }

    #[test]
    fn an_item_fits_on_two_columns_when_the_screen_allows() {
        let item = |n: &str, d: &str| Line::new(LineKind::CardItem, n).with_detail(d);
        let wide = card(vec![item("review", "rereads the code"), item("tests", "runs the tests")], 90, false);
        assert!(wide.iter().any(|l| l.contains("review") && l.contains("rereads the code")), "{wide:?}");
        // On a phone, the long name goes above its description.
        let long = "qwencloud-image-generation";
        let narrow = card(vec![item(long, "generates images")], 40, false);
        let i = narrow.iter().position(|l| l.contains(long)).unwrap();
        assert!(!narrow[i].contains("generates") && narrow[i + 1].contains("generates"), "{narrow:?}");
    }

    #[test]
    fn a_long_list_folds_section_by_section() {
        let mut lines = vec![Line::new(LineKind::CardSection, "Session")];
        lines.extend((0..12).map(|i| Line::new(LineKind::CardItem, format!("/c{i}")).with_detail("x")));
        lines.push(Line::new(LineKind::CardSection, "Models"));
        lines.push(Line::new(LineKind::CardItem, "/models").with_detail("list"));
        let folded = card(lines.clone(), 60, false).join("\n");
        assert!(folded.contains("4 de plus") && folded.contains("/models"), "{folded}");
        assert!(folded.find("4 de plus") < folded.find("Models"), "the mention follows its section");
        let expanded = card(lines, 60, true).join("\n");
        assert!(expanded.contains("/c11") && !expanded.contains("de plus"));
    }

    #[test]
    fn a_long_description_is_cut_then_read_in_full() {
        let desc = "word ".repeat(60);
        let lines = vec![Line::new(LineKind::CardItem, "name").with_detail(desc.trim())];
        let folded = card(lines.clone(), 40, false);
        assert!(folded.iter().filter(|l| l.contains("word")).count() <= FOLDED_DETAIL_ROWS);
        assert!(card(lines, 40, true).iter().filter(|l| l.contains("word")).count() > FOLDED_DETAIL_ROWS);
    }

    #[test]
    fn a_finished_reasoning_folds_even_mid_turn() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut lines = vec![Line::new(LineKind::ThinkingSummary, "Réflexion…")];
        lines.extend((0..20).map(|i| Line::new(LineKind::Thinking, format!("thought {i}"))));
        let opts = Options { width: 60, unicode: UnicodeLevel::Full, expanded: false, live: true, frame: 0, verbosity: Verbosity::Normal };
        let (rows, _) = tail_rows(&lines, &opts, &Palette::new(ColorDepth::None), usize::MAX);
        assert_eq!(rows.len(), 1 + LIVE_THINKING_ROWS, "live: only the last rows");
        assert!(text_of(rows.last().unwrap()).contains("thought 19"));
        // Once the header is closed (duration known), it folds, approval request or not.
        lines[0] = Line::new(LineKind::ThinkingSummary, "Réflexion · 11 s · ~434 tokens");
        let (rows, _) = tail_rows(&lines, &opts, &Palette::new(ColorDepth::None), usize::MAX);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_subagent_shows_its_goal_and_expands_on_touch() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let task = "Explore the project and list the points to improve. First read the README, then src/.";
        let lines = vec![Line::new(LineKind::Delegation, "explorer · qwen").with_detail(task)];
        let folded = rendered(&lines, 60, false);
        assert!(folded[0].contains("◆ explorer") && folded[0].contains("qwen"));
        let flat = folded.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("Explore the project and list the points to improve"), "{flat}");
        assert!(!flat.contains("README"));
        // As soon as the subagent says what it is doing, that is what is displayed, not the brief.
        let followed = vec![lines[0].clone().with_progress("I am reading the README")];
        let flat = rendered(&followed, 60, false).join(" ");
        assert!(flat.contains("I am reading the README") && !flat.contains("Explore"), "{flat}");
        let opts = Options { width: 60, unicode: UnicodeLevel::Full, expanded: false, live: false, frame: 0, verbosity: Verbosity::Normal };
        let rows = tail_rows_keyed(&lines, &opts, &Palette::new(ColorDepth::None), usize::MAX, 100, &|key| key == 100);
        assert!(rows.keys.iter().all(|k| *k == Some(100)));
        let texts: Vec<String> = rows.rows.iter().map(text_of).collect();
        assert!(texts.join(" ").contains("README"), "toggled by touch: the whole brief");
    }

    #[test]
    fn blocks_with_nothing_to_expand_have_no_key() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let lines = vec![Line::new(LineKind::User, "hello"), Line::new(LineKind::ThinkingSummary, "Réflexion · 1 s")];
        let opts = Options { width: 60, unicode: UnicodeLevel::Full, expanded: false, live: false, frame: 0, verbosity: Verbosity::Normal };
        let rows = tail_rows_keyed(&lines, &opts, &Palette::new(ColorDepth::None), usize::MAX, 0, &|_| false);
        assert_eq!(rows.keys, vec![None, None, None, Some(1)], "You (2 rows), separator, reasoning");
    }

    #[test]
    fn a_tool_call_fits_on_one_row_without_a_duplicate_duration() {
        let lines = vec![
            Line::new(LineKind::Tool, "bash $ ls -d /d/Documents/Programmation/Projets/*/ 2>/dev/null — code 0 in 0.8 s")
                .with_secs(0.8),
        ];
        let folded = rendered(&lines, 40, false);
        assert_eq!(folded.len(), 1, "{folded:?}");
        assert!(folded[0].ends_with('…') && display_width(&folded[0], UnicodeLevel::Full) <= 40, "{}", folded[0]);
        assert!(!folded[0].contains('$'));
        let expanded = rendered(&lines, 40, true).join(" ");
        assert!(expanded.contains("in 0.8 s") && !expanded.contains("0,8 s"), "{expanded}");
    }

    #[test]
    fn punctuation_stays_attached_to_its_word_even_if_the_style_changes() {
        let code = Style::default().add_modifier(Modifier::BOLD);
        let spans = vec![
            Span::raw("Others: Apollo, "),
            Span::styled("LuaVault", code),
            Span::raw(", RestoreImagerPro, Unity"),
        ];
        let rows = wrap_spans(spans, 24);
        for row in &rows {
            let text: String = row.iter().map(|s| s.content.as_ref()).collect();
            assert!(!text.trim_start().starts_with(','), "{rows:?}");
        }
        let joined: String = rows.iter().flatten().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("LuaVault,"));
    }

    #[test]
    fn a_word_longer_than_the_line_is_cut() {
        let rows = wrap_spans(vec![Span::raw("crates/po-agent/src/subagents.rs")], 10);
        assert!(rows.len() >= 4);
        for row in rows {
            let w: usize = row.iter().map(|s| display_width(&s.content, UnicodeLevel::Full)).sum();
            assert!(w <= 10);
        }
    }

    #[test]
    fn only_the_last_lines_are_laid_out() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let lines: Vec<Line> = (0..500).map(|i| Line::new(LineKind::Notice, format!("note {i}"))).collect();
        let lines: Vec<Line> = lines.into_iter().flat_map(|l| [l, Line::new(LineKind::User, "x")]).collect();
        let opts = Options { width: 40, unicode: UnicodeLevel::Full, expanded: false, live: false, frame: 0, verbosity: Verbosity::Normal };
        let (rows, start) = tail_rows(&lines, &opts, &Palette::new(ColorDepth::None), 30);
        assert!(!start && rows.len() < 60);
    }
}
