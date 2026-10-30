//! ratatui rendering.
//!
//! Rendering adapts to what `po-term` detected: rounded frames and Unicode symbols are only
//! emitted when the terminal renders them correctly, and the refresh is wrapped in DECSET 2026
//! when the terminal supports it — which is what prevents the tearing observed under tmux.

use std::io::{self, BufWriter, Write};

use po_term::width::{display_width, truncate};
use po_term::{ColorDepth, RenderProfile, Terminal, UnicodeLevel};
use ratatui::Terminal as RatatuiTerminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TuiLine, Span};
use ratatui::buffer::Buffer;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::state::Picker;
use crate::{App, shortcuts};

/// Enough for a full frame of a large terminal: Rust's standard output only buffers 1 KiB and
/// flushes at every line break, so a frame went out in dozens of pieces. Without synchronized
/// output (conhost, many SSH clients), the terminal displayed those pieces as they came: that
/// is the tearing.
const FRAME_BUFFER: usize = 256 * 1024;

pub struct Backend {
    terminal: RatatuiTerminal<CrosstermBackend<BufWriter<io::Stdout>>>,
    transcript_height: usize,
    /// Maximum scroll measured at the last draw, in screen rows.
    max_scroll: usize,
    /// Can the terminal wrap a refresh?
    sync_output: bool,
    /// Conversation area at the last draw, and the block of each of its rows: enough to know
    /// what a touch or a click designates.
    transcript_area: Rect,
    row_keys: Vec<Option<usize>>,
    /// Rows of each subagent in the small frame, and big card, at the last draw.
    panel_entries: Vec<(Rect, usize)>,
    card: Option<CardLayout>,
    /// Last drawn frame, for remote control (`po_ecran`).
    screen: Option<ratatui::buffer::Buffer>,
    /// "Dictate" button at the last draw.
    voice_area: Option<Rect>,
    /// Po Labs indicator, which a touch opens.
    labs_area: Option<Rect>,
    /// "‹ prev · next ›" row of the deck.
    deck_nav: Option<Rect>,
    /// Po Labs: its card, the proposal's two buttons, and the cross back to the user's session.
    labs_card: Option<Rect>,
    labs_yes: Option<Rect>,
    labs_later: Option<Rect>,
    labs_close: Option<Rect>,
    /// The verdict buttons of the change on trial, in the notebook.
    labs_verdicts: Vec<(Rect, crate::labs::Verdict)>,
    /// The buttons of the contribution on review, and its page arrows.
    labs_contribs: Vec<(Rect, crate::labs::ContribAction)>,
    labs_review_nav: Option<(Rect, Rect)>,
}

impl Backend {
    pub fn new(terminal: &Terminal) -> io::Result<Self> {
        let backend = CrosstermBackend::new(BufWriter::with_capacity(FRAME_BUFFER, io::stdout()));
        let mut ratatui = RatatuiTerminal::new(backend)?;
        // Differential rendering assumes a blank screen and does not redraw empty cells. A
        // terminal without an alternate screen, or whose alternate screen keeps its content,
        // used to leave leftovers of the startup banner in the middle of the conversation
        // (observed under ConPTY). Clearing once at the start costs nothing and lifts the
        // assumption.
        ratatui.clear()?;
        Ok(Self {
            terminal: ratatui,
            transcript_height: terminal.caps().size.1.saturating_sub(6) as usize,
            max_scroll: 0,
            sync_output: terminal.caps().sync_output,
            transcript_area: Rect::default(),
            row_keys: Vec::new(),
            panel_entries: Vec::new(),
            card: None,
            screen: None,
            voice_area: None,
            labs_area: None,
            deck_nav: None,
            labs_card: None,
            labs_yes: None,
            labs_later: None,
            labs_close: None,
            labs_verdicts: Vec::new(),
            labs_contribs: Vec::new(),
            labs_review_nav: None,
        })
    }

    /// What a click hits outside the conversation: cross, big card, small frame.
    pub fn hit(&self, column: u16, row: u16) -> crate::Hit {
        let inside = |r: Option<Rect>| {
            r.is_some_and(|r| column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height)
        };
        // A navigation row: left half, the previous card; right half, the next one.
        let side = |nav: Rect| if column < nav.x + nav.width / 2 { crate::Hit::CardPrev } else { crate::Hit::CardNext };
        if let Some(card) = &self.card {
            return if inside(Some(card.close)) {
                crate::Hit::MonitorClose
            } else if let Some(nav) = card.nav.filter(|n| inside(Some(*n))) {
                side(nav)
            } else if inside(Some(card.card)) {
                crate::Hit::MonitorCard
            } else {
                crate::Hit::None
            };
        }
        if let Some((_, action)) = self.labs_contribs.iter().find(|(r, _)| inside(Some(*r))) {
            return crate::Hit::LabsContrib(*action);
        }
        if let Some((prev, next)) = self.labs_review_nav {
            if inside(Some(prev)) {
                return crate::Hit::LabsReviewPrev;
            }
            if inside(Some(next)) {
                return crate::Hit::LabsReviewNext;
            }
        }
        if let Some((_, verdict)) = self.labs_verdicts.iter().find(|(r, _)| inside(Some(*r))) {
            return crate::Hit::LabsVerdict(*verdict);
        }
        if inside(self.labs_close) {
            return crate::Hit::LabsBack;
        }
        if inside(self.labs_yes) {
            return crate::Hit::LabsYes;
        }
        if inside(self.labs_later) {
            return crate::Hit::LabsLater;
        }
        if inside(self.labs_card) {
            return crate::Hit::LabsCard;
        }
        if inside(self.voice_area) {
            return crate::Hit::Voice;
        }
        if inside(self.labs_area) {
            return crate::Hit::Labs;
        }
        if let Some(nav) = self.deck_nav.filter(|n| inside(Some(*n))) {
            return side(nav);
        }
        match self.panel_entries.iter().find(|(r, _)| inside(Some(*r))) {
            Some((_, index)) => crate::Hit::MonitorPanel(*index),
            None => crate::Hit::None,
        }
    }

    /// Maximum scroll of the big card, or of the conversation when it is closed.
    pub fn card_max_scroll(&self) -> Option<usize> {
        self.card.as_ref().map(|c| c.max_scroll)
    }

    /// Expandable block displayed at this screen position, and the conversation row where it was
    /// touched.
    pub fn block_at(&self, column: u16, row: u16) -> Option<(usize, u16)> {
        // Big card open: the blocks are the subagent's.
        if let Some(card) = &self.card {
            let area = card.body;
            if column < area.x || column >= area.x + area.width || row < area.y || row >= area.y + area.height {
                return None;
            }
            let offset = row - area.y;
            return card.keys.get(offset as usize).copied().flatten().map(|key| (key, offset));
        }
        let area = self.transcript_area;
        if column < area.x || column >= area.x + area.width || row < area.y || row >= area.y + area.height {
            return None;
        }
        let offset = row - area.y;
        self.row_keys.get(offset as usize).copied().flatten().map(|key| (key, offset))
    }

    /// The screen as it has just been drawn, as text.
    pub fn screen_text(&self) -> String {
        let Some(buffer) = &self.screen else { return String::new() };
        let area = buffer.area;
        let mut out = Vec::with_capacity(area.height as usize);
        for y in area.y..area.y + area.height {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            out.push(row.trim_end().to_owned());
        }
        out.join("\n")
    }

    pub fn transcript_height(&self) -> usize {
        self.transcript_height.max(1)
    }

    pub fn max_scroll(&self) -> usize {
        self.max_scroll
    }

    /// Forgets what ratatui believes is displayed: the next frame is drawn in full. Used when
    /// someone else wrote on the screen (watchdog banner).
    pub fn invalidate(&mut self) -> io::Result<()> {
        self.terminal.clear()
    }

    pub fn draw(&mut self, app: &App, terminal: &Terminal) -> io::Result<()> {
        // Every frame is a sign of life for the watchdog; if it wrote in the meantime, the
        // screen is redrawn in full.
        crate::watchdog::beat();
        if crate::watchdog::take_scribbled() {
            self.terminal.clear()?;
        }
        // Wrapping the refresh asks the terminal to display it as one block rather than line by
        // line: without it, a full redraw under tmux visibly tears. The markers go through the
        // same buffer as the frame: written elsewhere, they could arrive out of order.
        if self.sync_output {
            let _ = self.terminal.backend_mut().write_all(b"\x1b[?2026h");
        }

        let caps = terminal.caps();
        let unicode = caps.unicode;
        let colors = Palette::new(caps.color);
        let rounded = terminal.profile() == RenderProfile::Rich && unicode >= UnicodeLevel::Bmp;

        // The useful height drives page scrolling; it is read before drawing, because the
        // borrow of the drawing buffer is exclusive.
        self.transcript_height = self.terminal.size().map(|s| s.height.saturating_sub(7)).unwrap_or(20) as usize;

        let mut transcript_rows = self.transcript_height;
        let mut max_scroll = self.max_scroll;
        let mut transcript_area = self.transcript_area;
        let mut row_keys: Vec<Option<usize>> = Vec::new();
        let mut panel_entries = Vec::new();
        let mut card = None;
        let mut screen = None;
        let mut voice_area = None;
        let mut labs_area = None;
        let mut labs_card = None;
        let mut labs_yes = None;
        let mut labs_later = None;
        let mut labs_close = None;
        let mut labs_verdicts = Vec::new();
        let mut labs_contribs = Vec::new();
        let mut labs_review_nav = None;
        let mut deck_nav = None;
        let result: io::Result<()> = self
            .terminal
            .draw(|frame| {
                let area = frame.area();
                // The input grows with its content, never taking more than a third of the screen.
                let inner = area.width.saturating_sub(2).max(1) as usize;
                let wanted = input_rows(app.input.text(), inner, unicode) as u16;
                let input_height = wanted.clamp(1, (area.height / 3).max(1)) + 2;
                // Messages waiting for the next safe point stay in sight, right above the input.
                let waiting = (app.queued.len().min(3) as u16).min(area.height / 4);
                // The Po Labs proposal: one row, two when the screen is narrow.
                let proposal = app.labs_proposal().map(|ask| labs_proposal_lines(app, ask, area.width as usize, &colors));
                let proposal_rows = proposal.as_ref().map_or(0, |(lines, _)| lines.len() as u16);
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),            // session title
                        Constraint::Min(1),               // conversation
                        Constraint::Length(waiting),      // waiting messages
                        Constraint::Length(proposal_rows), // Po Labs proposal
                        Constraint::Length(1),            // status
                        Constraint::Length(input_height), // input
                        Constraint::Length(1),            // information
                    ])
                    .split(area);
                if waiting > 0 {
                    frame.render_widget(queued_widget(app, chunks[2].width as usize, &colors, unicode), chunks[2]);
                }
                if let Some((lines, buttons)) = proposal {
                    let zone = chunks[3];
                    let (yes, later) = buttons;
                    labs_yes = Some(Rect { x: zone.x + yes.0, y: zone.y + yes.1, width: yes.2, height: 1 });
                    labs_later = Some(Rect { x: zone.x + later.0, y: zone.y + later.1, width: later.2, height: 1 });
                    frame.render_widget(Paragraph::new(lines), zone);
                }
                let chunks = [chunks[1], chunks[4], chunks[5], chunks[0], chunks[6]];

                if app.labs_front {
                    // The Po Labs session in front: its name in colors, and a cross back to the
                    // user's session, which waits behind.
                    let mut spans = crate::render::wave("Po Labs", &colors, true);
                    if let Some(title) = app.title.as_ref().filter(|t| t.as_str() != "Po Labs") {
                        spans.push(Span::styled(format!(" · {title}"), colors.style(Color::DarkGray, true)));
                    }
                    let row = chunks[3];
                    frame.render_widget(Paragraph::new(TuiLine::from(spans)).alignment(ratatui::layout::Alignment::Center), row);
                    let label = format!(" {} ✕ ", po_i18n::t!("tui.labs.back"));
                    let width = (label.chars().count() as u16).min(row.width);
                    let close = Rect { x: row.x + row.width - width, width, ..row };
                    frame.render_widget(Paragraph::new(Span::styled(label, colors.style(Color::Yellow, false))), close);
                    labs_close = Some(close);
                } else {
                    frame.render_widget(title_widget(app, chunks[3].width as usize, &colors), chunks[3]);
                }
                // As long as a subagent is visible, it has its column on the right: the conversation
                // narrows instead of being covered (observed: the frame hid the text).
                let (conversation, column) = split_for_monitor(app, chunks[0]);
                let (widget, max, keys) = transcript_widget(app, unicode, &colors, conversation);
                max_scroll = max;
                row_keys = keys;
                transcript_area = conversation;
                if app.welcome_shown() {
                    // An empty session: the mascot in the middle, the start notices under it.
                    let notices = app.transcript.len() as u16;
                    let below = if notices == 0 { 0 } else { (notices * 2 + 1).min(conversation.height / 3) };
                    let top = Rect { height: conversation.height - below, ..conversation };
                    let block = crate::mascot::welcome(
                        app.welcome.0.elapsed(),
                        app.welcome.1,
                        terminal.caps().color,
                        &crate::statusline::project(),
                        crate::statusline::git().and_then(|g| g.branch).as_deref(),
                        unicode >= UnicodeLevel::Bmp,
                    );
                    let height = (block.len() as u16).min(top.height);
                    let area = Rect { y: top.y + (top.height - height) / 2, height, ..top };
                    frame.render_widget(Paragraph::new(block).alignment(ratatui::layout::Alignment::Center), area);
                    if below > 0 {
                        let rest = Rect { y: top.y + top.height, height: below, ..conversation };
                        let (widget, max, keys) = transcript_widget(app, unicode, &colors, rest);
                        max_scroll = max;
                        row_keys = keys;
                        transcript_area = rest;
                        frame.render_widget(widget, rest);
                    }
                } else {
                    frame.render_widget(widget, conversation);
                }
                if let Some(column) = column
                    && let Some((deck, nav)) = monitor_panel(frame, app, column, rounded, &colors, unicode)
                {
                    panel_entries = vec![(deck, app.front_card())];
                    deck_nav = nav;
                }
                if let Some(column) = column.filter(|_| app.labs_card_shown()) {
                    labs_card = Some(labs_small_card(frame, app, column, rounded, &colors));
                }
                if app.monitor_open {
                    let zone = Rect { y: chunks[3].y + 1, height: chunks[1].y.saturating_sub(chunks[3].y + 1), ..area };
                    card = monitor_card(frame, app, zone, rounded, &colors, unicode);
                } else if let Some(review) = &app.labs_review {
                    let zone = Rect { y: chunks[3].y + 1, height: chunks[1].y.saturating_sub(chunks[3].y + 1), ..area };
                    let (close, buttons, nav) = labs_review_card(frame, app, review, zone, rounded, &colors);
                    labs_close = Some(close);
                    labs_contribs = buttons;
                    labs_review_nav = nav;
                } else if app.labs_notebook_open {
                    let zone = Rect { y: chunks[3].y + 1, height: chunks[1].y.saturating_sub(chunks[3].y + 1), ..area };
                    let (close, verdicts) = labs_notebook(frame, app, zone, rounded, &colors);
                    labs_close = Some(close);
                    labs_verdicts = verdicts;
                }
                frame.render_widget(
                    status_widget(app, chunks[1].width as usize, &colors, unicode >= UnicodeLevel::Bmp),
                    chunks[1],
                );
                transcript_rows = chunks[0].height as usize;
                frame.render_widget(
                    input_widget(app, rounded, &colors, inner, (input_height - 2) as usize, unicode),
                    chunks[2],
                );
                // The "dictate" button takes the right of the information row, wide enough for a
                // finger; the information keeps the rest.
                let button = voice_button(app, unicode >= UnicodeLevel::Bmp, &colors);
                let button_width = button.as_ref().map(|b| b.width() as u16).unwrap_or(0).min(chunks[4].width / 2);
                // The Po Labs indicator, discreet but always there: when off, it invites to try.
                let labs = labs_indicator(app.labs, unicode >= UnicodeLevel::Bmp, &colors);
                let labs_width = (labs.width() as u16).min(chunks[4].width.saturating_sub(button_width) / 3);
                let labs_rect = Rect {
                    x: chunks[4].x + chunks[4].width - button_width - labs_width,
                    width: labs_width,
                    ..chunks[4]
                };
                frame.render_widget(Paragraph::new(labs), labs_rect);
                labs_area = Some(labs_rect);
                let info = Rect { width: chunks[4].width - button_width - labs_width, ..chunks[4] };
                frame.render_widget(meta_widget(app, info.width as usize, &colors, unicode >= UnicodeLevel::Bmp), info);
                if let Some(button) = button {
                    let area = Rect { x: info.x + info.width, width: button_width, ..chunks[4] };
                    frame.render_widget(Paragraph::new(button), area);
                    voice_area = Some(area);
                }

                // Completion suggestions, right above the input.
                let suggestions = app.suggestions();
                if !suggestions.is_empty()
                    && app.pending_approval.is_none()
                    && app.picker.is_none()
                    && app.panel.is_none()
                {
                    let shown = suggestions.len().min(crate::complete::MAX_SUGGESTIONS);
                    let height = shown as u16 + 2;
                    let width = area.width.min(72);
                    if chunks[2].y >= height {
                        let popup = Rect { x: area.x, y: chunks[2].y - height, width, height };
                        frame.render_widget(Clear, popup);
                        frame.render_widget(
                            suggestions_widget(&suggestions, app.completion_selected(), rounded, &colors, width),
                            popup,
                        );
                    }
                }

                if let Some(pending) = &app.pending_question {
                // Anchored above the input, across the full width: the question is read where one
                // answers, and the conversation stays visible above.
                let width = area.width;
                let inner = width.saturating_sub(4) as usize;
                let preview = pending
                    .request
                    .question
                    .options
                    .get(pending.selected)
                    .and_then(|o| o.preview.clone());
                let side = preview.is_some() && inner >= 96;
                let left_width = if side { inner / 2 } else { inner };
                let mut lines = question_lines(pending, left_width, &colors, unicode);
                let preview_lines = preview.map(|p| preview_lines(&p, if side { inner - left_width - 3 } else { inner }, &colors, unicode));
                if let (Some(extra), false) = (&preview_lines, side) {
                    lines.push(TuiLine::from(""));
                    lines.extend(extra.iter().cloned());
                }
                // The footer takes the full width, below the two columns.
                let footer = if pending.typing.is_some() {
                    po_i18n::t!("view.question.footer_typing").to_owned()
                } else if inner >= 70 {
                    po_i18n::t!("view.question.footer_wide", n = pending.choices())
                } else {
                    po_i18n::t!("view.question.footer_narrow", n = pending.choices())
                };
                let wanted = lines.len().max(preview_lines.as_ref().map(|p| p.len()).unwrap_or(0)) as u16 + 4;
                let height = wanted.min(chunks[2].y.saturating_sub(area.y)).max(3);
                let popup = Rect { x: area.x, y: chunks[2].y.saturating_sub(height), width, height };
                frame.render_widget(Clear, popup);
                let block = frame_block(format!(" {} ", po_i18n::t!("view.question.title")), rounded, &colors).border_style(colors.style(Color::Magenta, false));
                let inner_area = block.inner(popup);
                frame.render_widget(block, popup);
                let inner_area = Rect { x: inner_area.x + 1, width: inner_area.width.saturating_sub(2), ..inner_area };
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(1)])
                    .split(inner_area);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        truncate(&footer, rows[1].width as usize, UnicodeLevel::Full).to_owned(),
                        colors.style(Color::Cyan, false),
                    )),
                    rows[1],
                );
                let inner_area = rows[0];
                if let (Some(extra), true) = (preview_lines, side) {
                    let halves = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Length(left_width as u16), Constraint::Length(1), Constraint::Min(1)])
                        .split(inner_area);
                    frame.render_widget(Paragraph::new(lines), halves[0]);
                    frame.render_widget(Paragraph::new(extra), halves[2]);
                } else {
                    frame.render_widget(Paragraph::new(lines), inner_area);
                }
            }

            if let Some(panel) = &app.panel {
                    // On a narrow screen the panel takes the whole screen: a centered frame would leave
                    // columns too short to read a value.
                    let popup = if area.width < 60 {
                        area
                    } else {
                        let height = (panel.rows().len() as u16 + 7).max(12).min(area.height.saturating_sub(2));
                        centered(area, 72, height)
                    };
                    frame.render_widget(Clear, popup);
                    frame.render_widget(
                        panel_widget(
                            panel,
                            rounded,
                            &colors,
                            unicode,
                            popup.width.saturating_sub(2) as usize,
                            popup.height,
                        ),
                        popup,
                    );
                }

                if let Some(finder) = &app.finder {
                    let height = (finder.matches().len().min(crate::settings::FINDER_ROWS) as u16 + 4).min(area.height);
                    let width = area.width.min(72);
                    let popup =
                        Rect { x: area.x + (area.width - width) / 2, y: area.y + 1.min(area.height), width, height };
                    frame.render_widget(Clear, popup);
                    frame.render_widget(
                        finder_widget(finder, rounded, &colors, width.saturating_sub(2) as usize),
                        popup,
                    );
                }

                if app.show_help {
                    let popup = centered(area, 56, (shortcuts().len() + 2) as u16);
                    frame.render_widget(Clear, popup);
                    frame.render_widget(help_widget(rounded, &colors), popup);
                }

                if let Some(picker) = &app.picker {
                    // The picker adapts to the screen: on a phone, it must neither overflow nor
                    // entirely hide the conversation.
                    let picker_height = (picker.entries.len() as u16 + 5).min(area.height.saturating_sub(2));
                    let popup = centered(area, 74, picker_height);
                    frame.render_widget(Clear, popup);
                    // Two columns go to the borders: the useful width is the content's, not the
                    // frame's.
                    let usable = popup.width.saturating_sub(2) as usize;
                    frame.render_widget(picker_widget(picker, rounded, &colors, unicode, usable), popup);
                }

                if let Some(request) = &app.pending_approval {
                    // Like a question: anchored above the input, across the full width, and as tall as
                    // the command requires — a cut command or invisible keys make the decision
                    // impossible (observed on a phone).
                    let inner = area.width.saturating_sub(4) as usize;
                    let lines = approval_lines(&request.summary, inner, &colors);
                    let height = (lines.len() as u16 + 2).min(chunks[2].y.saturating_sub(area.y)).max(3);
                    let popup = Rect { x: area.x, y: chunks[2].y.saturating_sub(height), width: area.width, height };
                    frame.render_widget(Clear, popup);
                    let title = format!(" {} ", po_i18n::t!("view.approval.title", tool = request.tool));
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_type(if rounded {
                            ratatui::widgets::BorderType::Rounded
                        } else {
                            ratatui::widgets::BorderType::Plain
                        })
                        .border_style(colors.style(Color::Yellow, false))
                        .title(Span::styled(title, colors.style(Color::Yellow, false).add_modifier(Modifier::BOLD)));
                    let inner_area = block.inner(popup);
                    frame.render_widget(block, popup);
                    let inner_area = Rect { x: inner_area.x + 1, width: inner_area.width.saturating_sub(2), ..inner_area };
                    // The footer stays visible even when the command is too long for the screen.
                    let rows = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(1), Constraint::Length(1)])
                        .split(inner_area);
                    let body: Vec<TuiLine> = lines[..lines.len().saturating_sub(1)].to_vec();
                    frame.render_widget(Paragraph::new(body), rows[0]);
                    if let Some(footer) = lines.last() {
                        frame.render_widget(Paragraph::new(footer.clone()), rows[1]);
                    }
                }
            })
            .map(|completed| {
                screen = Some(completed.buffer.clone());
            });

        if self.sync_output {
            let _ = self.terminal.backend_mut().write_all(b"\x1b[?2026l");
        }
        let _ = self.terminal.backend_mut().flush();
        self.transcript_height = transcript_rows;
        self.max_scroll = max_scroll;
        self.transcript_area = transcript_area;
        self.row_keys = row_keys;
        self.panel_entries = panel_entries;
        self.card = card;
        if screen.is_some() {
            self.screen = screen;
        }
        self.voice_area = voice_area;
        self.labs_area = labs_area;
        self.labs_card = labs_card;
        self.labs_yes = labs_yes;
        self.labs_later = labs_later;
        self.labs_close = labs_close;
        self.labs_verdicts = labs_verdicts;
        self.labs_contribs = labs_contribs;
        self.labs_review_nav = labs_review_nav;
        self.deck_nav = deck_nav;
        result
    }
}

/// Screen rows the input occupies, cursor included, wrapping of long lines included.
fn input_rows(text: &str, width: usize, level: UnicodeLevel) -> usize {
    text.split('\n').map(|line| (display_width(line, level) + 1).div_ceil(width).max(1)).sum()
}

/// Colors, reduced to what the terminal can display.
pub struct Palette {
    depth: ColorDepth,
}

impl Palette {
    pub fn new(depth: ColorDepth) -> Self {
        Self { depth }
    }

    pub fn truecolor(&self) -> bool {
        self.depth == ColorDepth::TrueColor
    }

    pub fn style(&self, color: Color, dim: bool) -> Style {
        if self.depth == ColorDepth::None {
            // Without color, we distinguish by intensity rather than flattening everything.
            return if dim { Style::default().add_modifier(Modifier::DIM) } else { Style::default() };
        }
        let style = Style::default().fg(color);
        if dim { style.add_modifier(Modifier::DIM) } else { style }
    }
}

/// Conversation laid out by blocks, maximum possible scroll, and the block of each visible row
/// (for touch).
fn transcript_widget<'a>(
    app: &'a App,
    unicode: UnicodeLevel,
    colors: &Palette,
    area: Rect,
) -> (Paragraph<'a>, usize, Vec<Option<usize>>) {
    let opts = crate::render::Options::new(area.width, unicode, app.expanded, app.progress.is_some(), app.frame);
    conversation_widget(&app.transcript, &app.toggled, &app.anchor, &opts, colors, area.height as usize)
}

/// A conversation laid out by blocks — the main agent's, or a subagent's in the big card — with
/// its scroll and its blocks toggled by touch.
fn conversation_widget<'a>(
    transcript: &crate::state::Transcript,
    toggled: &std::collections::HashSet<usize>,
    anchor: &std::cell::Cell<Option<(usize, u16)>>,
    opts: &crate::render::Options,
    colors: &Palette,
    height: usize,
) -> (Paragraph<'a>, usize, Vec<Option<usize>>) {
    let scroll = transcript.scroll();
    // One page more than needed: the next scroll finds it already laid out.
    let laid = crate::render::tail_rows_keyed(
        transcript.lines(),
        opts,
        colors,
        height * 2 + scroll,
        transcript.offset(),
        &|key| toggled.contains(&key),
    );
    let total = laid.rows.len();
    // A block has just been touched: we shift so its first row stays where the finger found it,
    // instead of letting the expansion push it off the screen.
    if let Some((key, offset)) = anchor.take()
        && let Some(index) = laid.keys.iter().position(|k| *k == Some(key))
    {
        let top = index.saturating_sub(offset as usize);
        transcript.set_scroll(total.saturating_sub(height).saturating_sub(top));
    }
    let scroll = transcript.scroll();
    let max =
        if laid.reached_start { total.saturating_sub(height) } else { total.saturating_sub(height).max(scroll + 1) };
    let scroll = scroll.min(total.saturating_sub(height));
    let end = total - scroll;
    let start = end.saturating_sub(height);
    (Paragraph::new(laid.rows[start..end].to_vec()), max, laid.keys[start..end].to_vec())
}

/// Session title, centered on the top row.
fn title_widget<'a>(app: &App, width: usize, colors: &Palette) -> Paragraph<'a> {
    let (text, style) = match &app.title {
        Some(title) => (title.clone(), colors.style(Color::Reset, false).add_modifier(Modifier::BOLD)),
        None if app.title_pending_display() => ("…".to_owned(), colors.style(Color::DarkGray, true)),
        None => ("Po".to_owned(), colors.style(Color::DarkGray, true)),
    };
    let text = truncate(&text, width.saturating_sub(4), UnicodeLevel::Full).to_owned();
    Paragraph::new(Span::styled(text, style)).alignment(ratatui::layout::Alignment::Center)
}

/// Information row under the input: time, context, speeds — small, permanent.
fn meta_widget<'a>(app: &App, width: usize, colors: &Palette, unicode: bool) -> Paragraph<'a> {
    let dim = colors.style(Color::DarkGray, true);
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for segment in crate::statusline::current() {
        let value = segment_spans(app, &segment, width < 70, colors, unicode);
        if value.is_empty() {
            continue;
        }
        if spans.len() > 1 {
            spans.push(Span::styled(" · ", dim));
        }
        if let Some(label) = segment.text.as_deref().filter(|_| !matches!(segment.id.as_str(), "text" | "command")) {
            spans.push(Span::styled(format!("{label} "), dim));
        }
        let color = segment.color.as_deref().and_then(color_named);
        spans.extend(value.into_iter().map(|span| match color {
            Some(color) => Span::styled(span.content, colors.style(color, false)),
            None => span,
        }));
    }
    Paragraph::new(TuiLine::from(crate::render::clip(spans, width, UnicodeLevel::Full)))
}

fn color_named(name: &str) -> Option<Color> {
    Some(match name {
        "green" => Color::Green,
        "red" => Color::Red,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "white" => Color::White,
        _ => return None,
    })
}

/// The value of one status line segment, empty when there is nothing to show.
fn segment_spans(
    app: &App,
    segment: &crate::statusline::Segment,
    compact: bool,
    colors: &Palette,
    unicode: bool,
) -> Vec<Span<'static>> {
    let dim = colors.style(Color::DarkGray, true);
    let text = |value: String| if value.is_empty() { Vec::new() } else { vec![Span::styled(value, dim)] };
    match segment.id.as_str() {
        "time" => text(chrono::Local::now().format("%H:%M").to_string()),
        "project" => text(crate::statusline::project()),
        "branch" => {
            let branch = crate::statusline::git().and_then(|g| g.branch).unwrap_or_default();
            if branch.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(format!("{}{branch}", if unicode { "⎇ " } else { "" }), colors.style(Color::Magenta, false))]
            }
        }
        "diff" => {
            let Some(git) = crate::statusline::git() else { return Vec::new() };
            let (up, down) = if unicode { ("▲", "▼") } else { ("+", "-") };
            let mut out = Vec::new();
            if git.added > 0 {
                out.push(Span::styled(format!("{up}{}", git.added), colors.style(Color::Green, false)));
            }
            if git.removed > 0 {
                if !out.is_empty() {
                    out.push(Span::raw(" "));
                }
                out.push(Span::styled(format!("{down}{}", git.removed), colors.style(Color::Red, false)));
            }
            out
        }
        "context" => {
            let Some((used, window)) = app.status.context.filter(|(_, w)| *w > 0) else { return Vec::new() };
            let k = |n: u32| {
                if n >= 10_000 { format!("{}k", n / 1000) } else { format!("{:.1}k", n as f64 / 1000.0).replace('.', po_i18n::t!("view.meta.decimal")) }
            };
            let percent = po_i18n::t!("view.meta.percent", value = format!("{:.0}", (used as f64 * 100.0 / window as f64).round()));
            text(if compact {
                format!("{}/{} ({percent})", k(used), k(window))
            } else {
                format!("{} {}/{} ({percent})", po_i18n::t!("view.meta.context"), k(used), k(window))
            })
        }
        "prefill" => match app.speeds.filter(|s| s.prefill_tok_s > 0.0) {
            Some(stats) if compact => text(format!("↑{:.0}", stats.prefill_tok_s)),
            Some(stats) => text(po_i18n::t!("view.meta.prefill", value = format!("{:.0}", stats.prefill_tok_s))),
            None => Vec::new(),
        },
        "generation" => {
            // During generation, the live rate; otherwise, that of the last call.
            let live = app.progress.as_ref().and_then(|p| p.live_generation());
            match live.or(app.speeds.map(|s| s.generation_tok_s)).filter(|g| *g > 0.0) {
                Some(g) if compact => text(format!("↓{g:.0} t/s")),
                Some(g) => text(po_i18n::t!("view.meta.generation", value = format!("{g:.0}"))),
                None => Vec::new(),
            }
        }
        "tokens" if app.status.tokens > 0 && !compact => text(po_i18n::t!("view.meta.tokens", n = app.status.tokens)),
        "cost" if app.status.cost > 0.0 => text(format!("${:.2}", app.status.cost)),
        "model" => text(app.status.model.clone()),
        "local_share" if app.status.tokens > 0 => text(format!("{:.0} % local", app.status.local_share * 100.0)),
        "text" => text(segment.text.clone().unwrap_or_default()),
        "command" => text(segment.command.as_deref().and_then(crate::statusline::command_output).unwrap_or_default()),
        _ => Vec::new(),
    }
}

/// The proposal above the input, and where its two buttons are: `(x, row, width)` each.
type ButtonSpot = (u16, u16, u16);

fn labs_proposal_lines(app: &App, ask: crate::LabsAsk, width: usize, colors: &Palette) -> (Vec<TuiLine<'static>>, (ButtonSpot, ButtonSpot)) {
    let dim = colors.style(Color::DarkGray, true);
    let mut head = vec![Span::raw("  ")];
    head.extend(crate::render::wave("Po Labs", colors, true));
    let summary = app.labs_book.trial.as_ref().map(|t| truncate(&t.summary, 60, UnicodeLevel::Full).to_owned()).unwrap_or_default();
    let (text, answer) = match ask {
        crate::LabsAsk::Requests(n) => (po_i18n::t!("tui.labs.proposal", n = n), po_i18n::t!("tui.labs.yes")),
        crate::LabsAsk::Adjust => (po_i18n::t!("tui.labs.proposal_adjust").to_owned(), po_i18n::t!("tui.labs.yes")),
        crate::LabsAsk::Verdict => (po_i18n::t!("tui.labs.proposal_verdict", summary = summary), po_i18n::t!("tui.labs.see")),
        crate::LabsAsk::Draft => (po_i18n::t!("tui.labs.proposal_draft").to_owned(), po_i18n::t!("tui.labs.yes")),
        crate::LabsAsk::Share(n) => (po_i18n::t!("tui.labs.proposal_share", n = n), po_i18n::t!("tui.labs.see")),
    };
    let message = format!(" · {text}");
    head.push(Span::styled(message.clone(), dim));
    let yes = format!("[{} {}]", crate::labs::shortcut("labs").label(), answer);
    let later = format!("[{}]", po_i18n::t!("tui.labs.later"));
    let head_width = 2 + 7 + message.chars().count();
    let buttons_width = yes.chars().count() + 1 + later.chars().count();
    let green = colors.style(Color::Green, false).add_modifier(Modifier::BOLD);
    if head_width + 2 + buttons_width <= width {
        let x = (head_width + 2) as u16;
        head.push(Span::raw("  "));
        head.push(Span::styled(yes.clone(), green));
        head.push(Span::raw(" "));
        head.push(Span::styled(later.clone(), dim));
        let yes_spot = (x, 0, yes.chars().count() as u16);
        let later_spot = (x + yes.chars().count() as u16 + 1, 0, later.chars().count() as u16);
        return (vec![TuiLine::from(crate::render::clip(head, width, UnicodeLevel::Full))], (yes_spot, later_spot));
    }
    let second = TuiLine::from(vec![Span::raw("    "), Span::styled(yes.clone(), green), Span::raw(" "), Span::styled(later.clone(), dim)]);
    let yes_spot = (4, 1, yes.chars().count() as u16);
    let later_spot = (5 + yes.chars().count() as u16, 1, later.chars().count() as u16);
    (vec![TuiLine::from(crate::render::clip(head, width, UnicodeLevel::Full)), second], (yes_spot, later_spot))
}

/// The Po Labs card, at the bottom of the right column (the subagents' cards take the top):
/// its state and the first request, in a frame of its own colors. Returns where it is.
fn labs_small_card(frame: &mut ratatui::Frame, app: &App, column: Rect, rounded: bool, colors: &Palette) -> Rect {
    let inner = column.width.saturating_sub(4) as usize;
    let dim = colors.style(Color::DarkGray, true);
    let pending = app.labs_book.pending();
    let paused = app.other_agent.as_ref().is_some_and(|a| a.is_paused()) && !app.labs_front;
    let elsewhere = app.labs_elsewhere.as_ref();
    let state = if let Some(other) = elsewhere {
        // Held by another session: shown greyed, without intruding.
        if other.activity.is_empty() {
            po_i18n::t!("tui.labs.state_elsewhere", project = other.project)
        } else {
            po_i18n::t!("tui.labs.state_elsewhere_doing", project = other.project, activity = other.activity)
        }
    } else if app.labs_front {
        po_i18n::t!("tui.labs.state_back").to_owned()
    } else if let Some(activity) = &app.labs_activity {
        po_i18n::t!("tui.labs.state_parallel", activity = activity)
    } else if paused {
        po_i18n::t!("tui.labs.state_paused", key = crate::labs::shortcut("labs").label())
    } else if app.labs_drafting.is_some() {
        po_i18n::t!("tui.labs.state_drafting").to_owned()
    } else if let Some(n) = app.labs_book.trial.as_ref().filter(|t| t.untouched()).and_then(|t| t.contribution) {
        po_i18n::t!("tui.labs.state_contribution", n = n)
    } else if app.labs_book.awaiting_verdict() {
        po_i18n::t!("tui.labs.state_trial").to_owned()
    } else if app.labs_book.trial.is_some() {
        po_i18n::t!("tui.labs.state_adjust").to_owned()
    } else if app.other_agent.is_some() {
        po_i18n::t!("tui.labs.state_open").to_owned()
    } else if pending.is_empty() && app.labs_book.undrafted().is_some() {
        po_i18n::t!("tui.labs.state_to_draft").to_owned()
    } else if pending.is_empty() && !app.labs_book.to_review().is_empty() {
        po_i18n::t!("tui.labs.state_share", n = app.labs_book.to_review().len())
    } else {
        po_i18n::t!("tui.labs.state_pending", n = pending.len())
    };
    let text_style = if elsewhere.is_some() { dim } else { colors.style(Color::Reset, false) };
    let mut lines = vec![TuiLine::from(Span::styled(truncate(&state, inner, UnicodeLevel::Full).to_owned(), text_style))];
    if !app.labs_front {
        if let Some(trial) = &app.labs_book.trial {
            lines.push(TuiLine::from(Span::styled(
                truncate(&trial.summary, inner, UnicodeLevel::Full).to_owned(),
                if elsewhere.is_some() { dim } else { colors.style(Color::Yellow, false) },
            )));
        }
        for item in pending.iter().take(if app.labs_book.trial.is_some() { 1 } else { 2 }) {
            lines.push(TuiLine::from(Span::styled(
                truncate(&format!("#{} {}", item.id, item.summary), inner, UnicodeLevel::Full).to_owned(),
                dim,
            )));
        }
    }
    let height = (lines.len() as u16 + 2).min(column.height);
    let rect = Rect { y: column.y + column.height - height, height, ..column };
    let buf = frame.buffer_mut();
    Clear.render(rect, buf);
    let mut title = vec![Span::raw(" ")];
    if elsewhere.is_some() {
        title.push(Span::styled("Po Labs", dim));
    } else {
        title.extend(crate::render::wave("Po Labs", colors, true));
    }
    title.push(Span::raw(" "));
    let border = if elsewhere.is_some() { dim } else { colors.style(Color::Yellow, false) };
    let block = card_block(TuiLine::from(title), border, rounded);
    let body = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(lines).render(Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body }, buf);
    rect
}

/// The notebook in the big card: the change on trial with its verdict buttons, then the requests
/// waiting for Po Labs, as the model wrote them. Returns where its cross and its buttons are.
fn labs_notebook(
    frame: &mut ratatui::Frame,
    app: &App,
    zone: Rect,
    rounded: bool,
    colors: &Palette,
) -> (Rect, Vec<(Rect, crate::labs::Verdict)>) {
    let buf = frame.buffer_mut();
    Clear.render(zone, buf);
    let mut title = vec![Span::raw(" ")];
    title.extend(crate::render::wave("Po Labs", colors, true));
    title.push(Span::styled(format!(" · {} ", po_i18n::t!("tui.labs.notebook")), colors.style(Color::Reset, false)));
    let block = card_block(TuiLine::from(title), colors.style(Color::Yellow, false), rounded);
    let body = block.inner(zone);
    block.render(zone, buf);
    let width = body.width.saturating_sub(2) as usize;
    let dim = colors.style(Color::DarkGray, true);
    let mut lines = Vec::new();
    let mut buttons = Vec::new();
    if let Some(trial) = &app.labs_book.trial {
        let ids: Vec<String> = trial.ids.iter().map(|id| format!("#{id}")).collect();
        let heading = match trial.contribution {
            Some(n) => po_i18n::t!("tui.labs.trial_contribution", n = n),
            None => po_i18n::t!("tui.labs.trial").to_owned(),
        };
        if trial.untouched() {
            lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.contribution_talk"), colors.style(Color::Cyan, false))));
        }
        lines.push(TuiLine::from(vec![
            Span::styled(format!("{heading} "), colors.style(Color::Yellow, false).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{} · {}", ids.join(" "), trial.date), dim),
        ]));
        for row in crate::render::wrap_spans(vec![Span::raw(trial.summary.clone())], width.saturating_sub(2)) {
            let mut row = row;
            row.insert(0, Span::raw("  "));
            lines.push(TuiLine::from(row));
        }
        if app.labs_book.awaiting_verdict() && app.labs_elsewhere.is_none() {
            // One button per verdict, on one row (wrapped when narrow), each with its key.
            let mut row: Vec<Span> = vec![Span::raw("  ")];
            let mut x = 2usize;
            for verdict in crate::labs::Verdict::ALL {
                let label = format!("[{} {}]", verdict.key(), verdict.label());
                let w = label.chars().count();
                if x + w > width && x > 2 {
                    lines.push(TuiLine::from(std::mem::take(&mut row)));
                    row.push(Span::raw("  "));
                    x = 2;
                }
                let style = match verdict {
                    crate::labs::Verdict::Keep => colors.style(Color::Green, false).add_modifier(Modifier::BOLD),
                    crate::labs::Verdict::Undo => colors.style(Color::Red, false),
                    crate::labs::Verdict::Revise => colors.style(Color::Cyan, false),
                };
                buttons.push((lines.len(), x, w, verdict));
                row.push(Span::styled(label, style));
                row.push(Span::raw("  "));
                x += w + 2;
            }
            lines.push(TuiLine::from(row));
        } else {
            for remark in &trial.revisions {
                lines.push(TuiLine::from(Span::styled(format!("  → {remark}"), colors.style(Color::Cyan, false))));
            }
            lines.push(TuiLine::from(Span::styled(format!("  {}", po_i18n::t!("tui.labs.adjusting")), dim)));
        }
        lines.push(TuiLine::from(""));
        lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.waiting_title"), dim.add_modifier(Modifier::BOLD))));
    }
    let pending = app.labs_book.pending();
    if pending.is_empty() {
        lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.empty"), dim)));
    }
    for item in pending {
        let mut tag = if item.kind.is_empty() { "idea".to_owned() } else { item.kind.clone() };
        if item.core {
            tag.push_str(" · core");
        }
        lines.push(TuiLine::from(vec![
            Span::styled(format!("#{} ", item.id), colors.style(Color::Yellow, false).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{tag} · {}", item.date), dim),
        ]));
        for row in crate::render::wrap_spans(vec![Span::raw(item.summary.clone())], width.saturating_sub(2)) {
            let mut row = row;
            row.insert(0, Span::raw("  "));
            lines.push(TuiLine::from(row));
        }
        lines.push(TuiLine::from(""));
    }
    let hint = if app.labs_book.awaiting_verdict() {
        po_i18n::t!("tui.labs.verdict_hint").to_owned()
    } else {
        po_i18n::t!("tui.labs.notebook_hint", key = crate::labs::shortcut("labs").label())
    };
    lines.push(TuiLine::from(Span::styled(hint, dim)));
    let text = Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body };
    Paragraph::new(lines).render(text, buf);
    let buttons = buttons
        .into_iter()
        .filter(|(row, ..)| (*row as u16) < text.height)
        .map(|(row, x, w, verdict)| (Rect { x: text.x + x as u16, y: text.y + row as u16, width: w as u16, height: 1 }, verdict))
        .collect();
    let close = Rect { x: zone.x + zone.width.saturating_sub(5), y: zone.y, width: 3, height: 1 };
    Paragraph::new(Span::styled(" ✕ ", colors.style(Color::Yellow, false))).render(close, buf);
    (close, buttons)
}

/// A contribution's category, in the interface language.
fn category_label(category: &str) -> String {
    match category {
        "appearance" => po_i18n::t!("tui.labs.cat.appearance").to_owned(),
        "commands" => po_i18n::t!("tui.labs.cat.commands").to_owned(),
        "tools" => po_i18n::t!("tui.labs.cat.tools").to_owned(),
        "models" => po_i18n::t!("tui.labs.cat.models").to_owned(),
        "sessions" => po_i18n::t!("tui.labs.cat.sessions").to_owned(),
        "performance" => po_i18n::t!("tui.labs.cat.performance").to_owned(),
        "accessibility" => po_i18n::t!("tui.labs.cat.accessibility").to_owned(),
        "other" | "" => po_i18n::t!("tui.labs.cat.other").to_owned(),
        other => other.to_owned(),
    }
}

/// Buttons and arrows of the review card.
type ReviewSpots = (Rect, Vec<(Rect, crate::labs::ContribAction)>, Option<(Rect, Rect)>);

/// The review of contributions in the big card: one page per contribution — its title, whether it
/// changes Po's core or extends it, what it changes in plain words, the details for the
/// developers — with its buttons; then the summary of what became of them. Returns where its
/// cross, its buttons and its arrows are.
fn labs_review_card(
    frame: &mut ratatui::Frame,
    app: &App,
    review: &crate::labs::Review,
    zone: Rect,
    rounded: bool,
    colors: &Palette,
) -> ReviewSpots {
    use crate::labs::ContribAction;
    let buf = frame.buffer_mut();
    Clear.render(zone, buf);
    let n = review.ids.len();
    let mut title = vec![Span::raw(" ")];
    title.extend(crate::render::wave("Po Labs", colors, true));
    let position = if review.finished() { String::new() } else { format!(" · {}/{n}", review.page + 1) };
    title.push(Span::styled(format!(" · {}{position} ", po_i18n::t!("tui.labs.contrib_title")), colors.style(Color::Reset, false)));
    let block = card_block(TuiLine::from(title), colors.style(Color::Yellow, false), rounded);
    let body = block.inner(zone);
    block.render(zone, buf);
    let text = Rect { x: body.x + 1, width: body.width.saturating_sub(2), ..body };
    let width = text.width as usize;
    let dim = colors.style(Color::DarkGray, true);
    let bold = colors.style(Color::Reset, false).add_modifier(Modifier::BOLD);
    let wrap = |text: &str, style: Style, lines: &mut Vec<TuiLine<'static>>| {
        for paragraph in text.lines() {
            for row in crate::render::wrap_spans(vec![Span::styled(paragraph.to_owned(), style)], width.max(1)) {
                lines.push(TuiLine::from(row));
            }
        }
    };
    let mut lines: Vec<TuiLine<'static>> = Vec::new();
    let mut buttons = Vec::new();
    let mut nav = None;
    let book = &app.labs_book;
    match review.current().and_then(|id| book.kept.iter().find(|k| k.id == id)) {
        None => {
            // The summary: what was sent, kept for later, deleted.
            lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.recap_title"), bold)));
            lines.push(TuiLine::from(""));
            for (id, action) in &review.done {
                let name = book.kept.iter().find(|k| k.id == *id).and_then(|k| k.draft.as_ref()).map(|d| d.title.clone()).unwrap_or_default();
                let (mark, style) = match action {
                    ContribAction::Share => (po_i18n::t!("tui.labs.done_shared"), colors.style(Color::Green, false)),
                    ContribAction::Later => (po_i18n::t!("tui.labs.done_aside"), colors.style(Color::Yellow, false)),
                    _ => (po_i18n::t!("tui.labs.done_deleted"), dim),
                };
                wrap(&format!("{mark} · {name}"), style, &mut lines);
            }
            lines.push(TuiLine::from(""));
            wrap(po_i18n::t!("tui.labs.recap_hint"), dim, &mut lines);
        }
        Some(kept) => {
            let draft = kept.draft.clone().unwrap_or_default();
            wrap(&draft.title, bold, &mut lines);
            let (badge, badge_style) = if kept.core {
                (po_i18n::t!("tui.labs.core_badge"), colors.style(Color::Yellow, false).add_modifier(Modifier::BOLD))
            } else {
                (po_i18n::t!("tui.labs.extension_badge"), colors.style(Color::Cyan, false).add_modifier(Modifier::BOLD))
            };
            let ids: Vec<String> = kept.ids.iter().map(|i| format!("#{i}")).collect();
            lines.push(TuiLine::from(vec![
                Span::styled(badge.to_owned(), badge_style),
                Span::styled(format!(" · {} · {}", po_i18n::t!("tui.labs.category", category = category_label(&draft.category)), ids.join(" ")), dim),
            ]));
            lines.push(TuiLine::from(""));
            wrap(&draft.user_summary, colors.style(Color::Reset, false), &mut lines);
            lines.push(TuiLine::from(""));
            if let Some(action) = review.outcome(kept.id) {
                let mark = match action {
                    ContribAction::Share => po_i18n::t!("tui.labs.done_shared"),
                    ContribAction::Later => po_i18n::t!("tui.labs.done_aside"),
                    _ => po_i18n::t!("tui.labs.done_deleted"),
                };
                lines.push(TuiLine::from(Span::styled(format!("→ {mark}"), colors.style(Color::Green, false))));
                if action == ContribAction::Share {
                    // What becomes of it: in a future version for the core, in the catalog for an extension.
                    let next = if kept.core { po_i18n::t!("tui.labs.shared_core") } else { po_i18n::t!("tui.labs.shared_extension") };
                    wrap(next, colors.style(Color::Green, false), &mut lines);
                }
                lines.push(TuiLine::from(""));
            }
            if review.rewriting == Some(kept.id) {
                lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.rewriting"), colors.style(Color::Cyan, false))));
            }
            lines.push(TuiLine::from(Span::styled(po_i18n::t!("tui.labs.for_devs"), dim.add_modifier(Modifier::BOLD))));
            wrap(&draft.description, dim, &mut lines);
            if !kept.files.is_empty() {
                wrap(&po_i18n::t!("tui.labs.files", files = kept.files.join(", ")), dim, &mut lines);
            }
        }
    }
    // The buttons and the arrows keep the last rows, whatever the length of the text.
    let reserved: u16 = if review.finished() { 0 } else { 2 };
    let room = text.height.saturating_sub(reserved) as usize;
    if lines.len() > room {
        lines.truncate(room.saturating_sub(1));
        lines.push(TuiLine::from(Span::styled("…", dim)));
    }
    Paragraph::new(lines).render(Rect { height: text.height.saturating_sub(reserved), ..text }, buf);
    if !review.finished() && text.height >= 2 {
        let row = text.y + text.height - 2;
        let mut x = text.x;
        let mut spans = Vec::new();
        // Edit chosen: its text, or its code.
        let choices: &[ContribAction] = if review.choosing { &ContribAction::EDIT } else { &ContribAction::ALL };
        if review.choosing {
            let ask = format!("{} ", po_i18n::t!("tui.labs.edit_which"));
            x += ask.chars().count() as u16;
            spans.push(Span::styled(ask, colors.style(Color::Reset, false)));
        }
        for &action in choices {
            let label = format!("[{} {}]", action.key(), action.label());
            let w = label.chars().count() as u16;
            if x + w > text.x + text.width {
                break;
            }
            let style = match action {
                ContribAction::Share => colors.style(Color::Green, false).add_modifier(Modifier::BOLD),
                ContribAction::Later => colors.style(Color::Yellow, false),
                ContribAction::Delete => colors.style(Color::Red, false),
                ContribAction::Edit | ContribAction::EditText | ContribAction::EditCode => colors.style(Color::Cyan, false),
            };
            buttons.push((Rect { x, y: row, width: w, height: 1 }, action));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        Paragraph::new(TuiLine::from(spans)).render(Rect { y: row, height: 1, ..text }, buf);
        if n > 1 {
            let prev = format!("‹ {}", po_i18n::t!("tui.labs.prev"));
            let next = format!("{} ›", po_i18n::t!("tui.labs.next"));
            let nav_row = row + 1;
            let half = text.width / 2;
            Paragraph::new(Span::styled(prev, colors.style(Color::Yellow, false))).render(Rect { y: nav_row, height: 1, width: half, ..text }, buf);
            Paragraph::new(Span::styled(next, colors.style(Color::Yellow, false)))
                .alignment(ratatui::layout::Alignment::Right)
                .render(Rect { x: text.x + half, y: nav_row, width: text.width - half, height: 1 }, buf);
            nav = Some((Rect { y: nav_row, height: 1, width: half, ..text }, Rect { x: text.x + half, y: nav_row, width: text.width - half, height: 1 }));
        } else {
            Paragraph::new(Span::styled(po_i18n::t!("tui.labs.review_hint"), dim)).render(Rect { y: row + 1, height: 1, ..text }, buf);
        }
    }
    let close = Rect { x: zone.x + zone.width.saturating_sub(5), y: zone.y, width: 3, height: 1 };
    Paragraph::new(Span::styled(" ✕ ", colors.style(Color::Yellow, false))).render(close, buf);
    (close, buttons, nav)
}

/// Po Labs indicator: a dimmed "○ Labs" when off, a colorful "● Labs" when on.
fn labs_indicator(on: bool, fancy: bool, colors: &Palette) -> TuiLine<'static> {
    let label = po_i18n::t!("labs.indicator");
    if on {
        let mut spans = vec![Span::styled(if fancy { " ● " } else { " * " }, colors.style(Color::Green, false))];
        spans.extend(crate::render::wave(&format!("{label} "), colors, true));
        TuiLine::from(spans)
    } else {
        TuiLine::from(Span::styled(format!(" {} {label} ", if fancy { "○" } else { "o" }), colors.style(Color::DarkGray, true)))
    }
}

/// Voice dictation button, when enabled: "dictate", then the recording duration (touch to
/// finish), then "transcribing…".
fn voice_button(app: &App, fancy: bool, colors: &Palette) -> Option<TuiLine<'static>> {
    if !app.voice_enabled && app.voice_started.is_none() && !app.voice_busy {
        return None;
    }
    let dot = if fancy { "●" } else { "*" };
    let line = if let Some(started) = app.voice_started {
        TuiLine::from(Span::styled(
            format!(" {dot} {} s · {} ", started.elapsed().as_secs(), po_i18n::t!("view.voice.stop")),
            colors.style(Color::Red, false).add_modifier(Modifier::BOLD),
        ))
    } else if app.voice_busy {
        TuiLine::from(Span::styled(format!(" {} ", po_i18n::t!("view.voice.transcribing")), colors.style(Color::Yellow, false)))
    } else {
        TuiLine::from(Span::styled(format!(" {dot} {} ", po_i18n::t!("view.voice.dictate")), colors.style(Color::Cyan, false)))
    };
    Some(line)
}

/// Colors of the subagent cards, in order of arrival: two neighboring cards of the deck are
/// never confused.
const CARD_COLORS: [Color; 6] = [Color::Magenta, Color::Cyan, Color::Yellow, Color::Green, Color::Blue, Color::LightRed];

pub fn card_color(index: usize) -> Color {
    CARD_COLORS[index % CARD_COLORS.len()]
}

/// Splits the conversation area when subagents are to be displayed: their column on the right
/// (a bit less than half the width), the conversation on the left. On a screen too narrow for
/// two readable columns, the deck takes a band at the top, covering nothing.
fn split_for_monitor(app: &App, area: Rect) -> (Rect, Option<Rect>) {
    // The Po Labs card has its place under the subagents' even when none runs.
    if (app.visible_deck().is_empty() && !app.labs_card_shown()) || area.height < 6 {
        return (area, None);
    }
    if area.width >= 50 {
        let width = (area.width * 9 / 20).clamp(28, 56);
        let conversation = Rect { width: area.width - width - 1, ..area };
        let column = Rect { x: area.x + area.width - width, width, ..area };
        return (conversation, Some(column));
    }
    let height = (area.height / 3).max(7);
    let band = Rect { height, ..area };
    let conversation = Rect { y: area.y + height, height: area.height - height, ..area };
    (conversation, Some(band))
}

/// Content of a small card: the subagent, for how long, and what it says it is doing, in full —
/// the card grows with its summary instead of cutting it.
fn small_card_lines(
    app: &App,
    view: &crate::monitor::SubagentView,
    inner: usize,
    colors: &Palette,
    unicode: UnicodeLevel,
) -> Vec<TuiLine<'static>> {
    let fancy = unicode >= UnicodeLevel::Bmp;
    let dim = colors.style(Color::DarkGray, true);
    let accent = colors.style(card_color(view.color), false);
    let spinner: &[&str] = if fancy { &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] } else { &["|", "/", "-", "\\"] };
    let (state, state_style) = match view.finished {
        None if view.paused.is_some() => ((if fancy { "‖" } else { "=" }).to_owned(), colors.style(Color::Yellow, false)),
        None => (spinner[app.frame % spinner.len()].to_owned(), colors.style(Color::Cyan, false)),
        Some((_, true)) => ((if fancy { "✓" } else { "+" }).to_owned(), colors.style(Color::Green, false)),
        Some((_, false)) => ((if fancy { "✗" } else { "x" }).to_owned(), colors.style(Color::Red, false)),
    };
    let elapsed = view.elapsed();
    let mut head = vec![
        Span::styled(format!("{state} "), state_style),
        Span::styled(view.agent.clone(), accent.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {} s", elapsed.as_secs()), dim),
    ];
    if view.background {
        head.push(Span::styled(format!(" · {}", po_i18n::t!("view.card.in_background")), dim));
    }
    let mut lines = vec![TuiLine::from(crate::render::clip(head, inner, unicode))];
    let style = if view.progress.is_some() || view.finished.is_some() {
        Style::default()
    } else {
        dim.add_modifier(Modifier::ITALIC)
    };
    for mut row in crate::render::wrap_spans(vec![Span::styled(view.status_line(), style)], inner.saturating_sub(2)) {
        row.insert(0, Span::raw("  "));
        lines.push(TuiLine::from(row));
    }
    lines
}

fn card_block(title: TuiLine<'static>, color: Style, rounded: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if rounded { ratatui::widgets::BorderType::Rounded } else { ratatui::widgets::BorderType::Plain })
        .border_style(color)
        .title(title)
}

/// Draws a small card into a buffer (the screen, or an animation buffer).
#[allow(clippy::too_many_arguments)]
fn draw_small_card(
    buf: &mut Buffer,
    rect: Rect,
    app: &App,
    view: &crate::monitor::SubagentView,
    lines: Vec<TuiLine<'static>>,
    rank: Option<(usize, usize)>,
    rounded: bool,
    colors: &Palette,
) -> Option<Rect> {
    let accent = colors.style(card_color(view.color), false);
    Clear.render(rect, buf);
    let block = card_block(TuiLine::from(Span::styled(format!(" {} ", po_i18n::t!("view.card.title")), accent.add_modifier(Modifier::BOLD))), accent, rounded);
    let inner = block.inner(rect);
    block.render(rect, buf);
    let mut inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    // Several subagents: a "‹ prev · 2/4 · next ›" row at the bottom of the card, to touch.
    // The horizontal swipe does not always reach Po (Termius does not forward it, observed);
    // wide zones work everywhere.
    let nav = match rank {
        Some((k, n)) if inner.height >= 2 => {
            let row = Rect { y: inner.y + inner.height - 1, height: 1, ..inner };
            inner.height -= 1;
            let (prev, next) = (format!("‹ {}", po_i18n::t!("view.card.prev_short")), format!("{} ›", po_i18n::t!("view.card.next_short")));
            nav_row(buf, row, &prev, &format!("{k}/{n}"), &next, accent, colors);
            Some(Rect { x: rect.x, width: rect.width, ..row })
        }
        _ => None,
    };
    // Too tall for the column: the last rows, the most recent.
    let skip = lines.len().saturating_sub(inner.height as usize);
    Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()).render(inner, buf);
    let _ = app;
    nav
}

/// Navigation row: left, middle, right.
fn nav_row(buf: &mut Buffer, row: Rect, left: &str, middle: &str, right: &str, accent: Style, colors: &Palette) {
    let width = row.width as usize;
    let dim = colors.style(Color::DarkGray, true);
    let (lw, mw, rw) = (display_width(left, UnicodeLevel::Full), display_width(middle, UnicodeLevel::Full), display_width(right, UnicodeLevel::Full));
    if lw + mw + rw + 2 > width {
        let short = format!("‹ {middle} ›");
        let pad = width.saturating_sub(display_width(&short, UnicodeLevel::Full)) / 2;
        Paragraph::new(Span::styled(format!("{}{short}", " ".repeat(pad)), accent)).render(row, buf);
        return;
    }
    let gap = width - lw - mw - rw;
    let spans = vec![
        Span::styled(left.to_owned(), accent.add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap / 2)),
        Span::styled(middle.to_owned(), dim),
        Span::raw(" ".repeat(gap - gap / 2)),
        Span::styled(right.to_owned(), accent.add_modifier(Modifier::BOLD)),
    ];
    Paragraph::new(TuiLine::from(spans)).render(row, buf);
}

/// Deck of the running subagents, at the top of its column: the one in front shows its summary
/// in full; the following ones stick out behind it, each in its color, like a deck of cards. A
/// swipe changes the front card (page turning). Returns the deck's area, which a touch or a
/// swipe designates.
fn monitor_panel(
    frame: &mut ratatui::Frame,
    app: &App,
    area: Rect,
    rounded: bool,
    colors: &Palette,
    unicode: UnicodeLevel,
) -> Option<(Rect, Option<Rect>)> {
    let deck = app.visible_deck();
    if deck.is_empty() {
        return None;
    }
    let front = app.front_card();
    let position = deck.iter().position(|i| *i == front).unwrap_or(0);
    let n = deck.len();
    // One column and one row of offset per card behind, three at most: enough to see the deck,
    // without eating the summary's room.
    let depth = (n - 1).min(3) as u16;
    let card_width = area.width.saturating_sub(depth);
    let inner = card_width.saturating_sub(4) as usize;
    let view = &app.monitor.views[front];
    let lines = small_card_lines(app, view, inner, colors, unicode);
    let nav_rows = u16::from(n > 1);
    let height = (lines.len() as u16 + 2 + nav_rows).min(area.height.saturating_sub(depth)).max(3);
    let buf = frame.buffer_mut();
    for j in (1..=depth).rev() {
        let behind = &app.monitor.views[deck[(position + j as usize) % n]];
        let rect = Rect { x: area.x + j, y: area.y + depth - j, width: card_width, height };
        let color = colors.style(card_color(behind.color), false);
        Clear.render(rect, buf);
        card_block(TuiLine::from(""), color, rounded).render(rect, buf);
    }
    let rect = Rect { x: area.x, y: area.y + depth, width: card_width, height };
    let rank = (n > 1).then_some((position + 1, n));
    let nav = match app.card_turn() {
        Some((progress, from, dir)) if from != front && from < app.monitor.views.len() => {
            let old_view = &app.monitor.views[from];
            let old_lines = small_card_lines(app, old_view, inner, colors, unicode);
            let old_rank = deck.iter().position(|i| *i == from).map(|p| (p + 1, n)).filter(|_| n > 1);
            let mut old = Buffer::empty(rect);
            draw_small_card(&mut old, rect, app, old_view, old_lines, old_rank, rounded, colors);
            let mut new = Buffer::empty(rect);
            let nav = draw_small_card(&mut new, rect, app, view, lines, rank, rounded, colors);
            turn_page(buf, rect, &old, &new, progress, dir, colors.style(card_color(view.color), false));
            nav
        }
        _ => draw_small_card(buf, rect, app, view, lines, rank, rounded, colors),
    };
    Some((Rect { x: area.x, y: area.y, width: area.width, height: height + depth }, nav))
}

/// Page-turning effect: the new card covers the old one from the edge it comes from (right for
/// the next, left for the previous), with a fold of its color.
fn turn_page(target: &mut Buffer, rect: Rect, old: &Buffer, new: &Buffer, progress: f32, dir: i8, fold: Style) {
    let width = rect.width;
    // Fast at the start, soft at the end: the gesture "settles".
    let eased = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(3);
    let shown = ((eased * f32::from(width)).round() as u16).min(width);
    for y in rect.y..rect.y + rect.height {
        for dx in 0..width {
            let from_new = if dir > 0 { dx >= width - shown } else { dx < shown };
            let source = if from_new { new } else { old };
            let x = rect.x + dx;
            target[(x, y)] = source[(x, y)].clone();
        }
    }
    if shown > 0 && shown < width {
        let edge = if dir > 0 { rect.x + width - shown } else { rect.x + shown - 1 };
        let color = fold.fg.unwrap_or(Color::Magenta);
        for y in rect.y..rect.y + rect.height {
            let cell = &mut target[(edge, y)];
            cell.set_style(Style::default().bg(color));
        }
    }
}

/// What the big card drew: its area, its cross, the area of the subagent's conversation, the
/// block of each of its rows and its maximum scroll.
pub struct CardLayout {
    pub card: Rect,
    pub close: Rect,
    pub body: Rect,
    pub keys: Vec<Option<usize>>,
    pub max_scroll: usize,
    /// "‹ previous · … · next ›" row, when there are several subagents.
    pub nav: Option<Rect>,
}

/// Big card of a subagent: the whole conversation area, its own conversation laid out like the
/// main agent's (reasoning, tools, text, foldable by touch), a cross at the top right. Switching
/// to another subagent turns the page.
fn monitor_card(
    frame: &mut ratatui::Frame,
    app: &App,
    area: Rect,
    rounded: bool,
    colors: &Palette,
    unicode: UnicodeLevel,
) -> Option<CardLayout> {
    let count = app.monitor.views.len();
    let selected = app.monitor_selected.min(count.checked_sub(1)?);
    let buf = frame.buffer_mut();
    match app.card_turn() {
        Some((progress, from, dir)) if from != selected && from < count => {
            let mut old = Buffer::empty(area);
            draw_big_card(&mut old, app, from, area, rounded, colors, unicode);
            let mut new = Buffer::empty(area);
            let layout = draw_big_card(&mut new, app, selected, area, rounded, colors, unicode);
            let fold = colors.style(card_color(app.monitor.views[selected].color), false);
            turn_page(buf, area, &old, &new, progress, dir, fold);
            Some(layout)
        }
        _ => Some(draw_big_card(buf, app, selected, area, rounded, colors, unicode)),
    }
}

fn draw_big_card(
    buf: &mut Buffer,
    app: &App,
    index: usize,
    card: Rect,
    rounded: bool,
    colors: &Palette,
    unicode: UnicodeLevel,
) -> CardLayout {
    let count = app.monitor.views.len();
    let view = &app.monitor.views[index];
    let accent = colors.style(card_color(view.color), false);
    let dim = colors.style(Color::DarkGray, true);
    let state = match view.finished {
        None if view.paused.is_some() => po_i18n::t!("view.card.state_paused"),
        None => po_i18n::t!("view.card.state_running"),
        Some((_, true)) => po_i18n::t!("view.card.state_done"),
        Some((_, false)) => po_i18n::t!("view.card.state_stopped"),
    };
    let elapsed = view.elapsed();
    let mut title = vec![
        Span::styled(format!(" {} ", view.agent), accent.add_modifier(Modifier::BOLD)),
        Span::styled(format!("· {state} · {} s ", elapsed.as_secs()), dim),
    ];
    if count > 1 {
        let (left, right) = if unicode >= UnicodeLevel::Bmp { ("‹", "›") } else { ("<", ">") };
        title.push(Span::styled(format!("{left} {}/{count} {right} ", index + 1), accent));
    }
    Clear.render(card, buf);
    let block = card_block(TuiLine::from(crate::render::clip(title, card.width.saturating_sub(7) as usize, unicode)), accent, rounded);
    let inner_area = block.inner(card);
    block.render(card, buf);
    let inner_area = Rect { x: inner_area.x + 1, width: inner_area.width.saturating_sub(2), ..inner_area };
    // The footer says how to leave and how to switch from one subagent to another.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner_area);
    let nav = if count > 1 {
        // The longest middle text that leaves both arrows readable.
        let (prev, next) = (format!("‹ {}", po_i18n::t!("view.card.prev")), format!("{} ›", po_i18n::t!("view.card.next")));
        let room = (rows[1].width as usize).saturating_sub(prev.chars().count() + next.chars().count() + 2);
        let middle = [po_i18n::t!("view.card.close_hint_long"), po_i18n::t!("view.card.close_hint_mid"), po_i18n::t!("view.card.esc"), ""]
            .into_iter()
            .find(|m| m.chars().count() <= room)
            .unwrap_or("");
        nav_row(buf, rows[1], &prev, middle, &next, accent, colors);
        Some(rows[1])
    } else {
        let footer = if inner_area.width >= 50 {
            po_i18n::t!("view.card.footer_wide")
        } else {
            po_i18n::t!("view.card.footer_narrow")
        };
        Paragraph::new(Span::styled(truncate(footer, rows[1].width as usize, UnicodeLevel::Full).to_owned(), dim))
            .render(rows[1], buf);
        None
    };
    let opts = crate::render::Options::new(rows[0].width, unicode, app.expanded, view.running(), app.frame);
    let (widget, max_scroll, keys) =
        conversation_widget(&view.transcript, &view.toggled, &view.anchor, &opts, colors, rows[0].height as usize);
    widget.render(rows[0], buf);
    // The cross, wide enough for a finger.
    let close = Rect { x: card.x + card.width.saturating_sub(5), y: card.y, width: 4, height: 1 };
    Paragraph::new(Span::styled(" ✕ ", colors.style(Color::Red, false).add_modifier(Modifier::BOLD))).render(close, buf);
    CardLayout { card, close, body: rows[0], keys, max_scroll, nav }
}

/// Last lines needed to fill `height` screen rows plus `scroll` rows of scrolling, and the
/// offset from the top of that window.
pub fn bottom_window(lines: Vec<TuiLine<'_>>, width: u16, height: usize, scroll: usize) -> (Vec<TuiLine<'_>>, usize) {
    let wanted = height + scroll;
    let mut rows = 0;
    let mut start = lines.len();
    while start > 0 && rows < wanted {
        start -= 1;
        rows += Paragraph::new(lines[start].clone()).wrap(Wrap { trim: false }).line_count(width.max(1)).max(1);
    }
    let top = rows.saturating_sub(height).saturating_sub(scroll);
    let mut lines = lines;
    (lines.split_off(start), top)
}

/// Renders an answer line in light markdown: headings, lists, bold, inline code and code
/// blocks. No tables or links: their width is unpredictable during streaming, and a rendering
/// that jumps at every fragment is worse than plain text. An unclosed marker stays as is: in
/// the middle of streaming, the rest has not arrived yet.
pub fn markdown_line<'a>(text: &str, in_code: &mut bool, unicode: UnicodeLevel, colors: &Palette) -> TuiLine<'a> {
    let code_style = colors.style(Color::Cyan, false);
    let trimmed = text.trim_start();
    if trimmed.starts_with("```") {
        *in_code = !*in_code;
        let rule = if unicode >= UnicodeLevel::Bmp { "──" } else { "--" };
        let lang = trimmed.trim_start_matches('`').trim();
        let label = if lang.is_empty() { rule.to_owned() } else { format!("{rule} {lang}") };
        return TuiLine::from(Span::styled(label, colors.style(Color::DarkGray, true)));
    }
    if *in_code {
        return TuiLine::from(Span::styled(format!("  {text}"), code_style));
    }
    // Outside code blocks, no emoji: the user does not want any.
    let stripped = po_term::width::strip_emoji(text);
    let text = stripped.as_str();
    // Recomputed on the cleaned text: the indentation is measured by length difference.
    let trimmed = text.trim_start();
    let bold = Style::default().add_modifier(Modifier::BOLD);
    if let Some(title) = heading(trimmed) {
        // A heading keeps its inner formatting: "### `D:\Projects` — …" used to show its
        // backticks (observed).
        let spans: Vec<Span> = inline_markdown(title)
            .into_iter()
            .map(|(piece, emphasis)| match emphasis {
                Emphasis::Code => Span::styled(piece, code_style.add_modifier(Modifier::BOLD)),
                _ => Span::styled(piece, bold),
            })
            .collect();
        return TuiLine::from(spans);
    }
    let mut spans = Vec::new();
    let mut body = text;
    let indent = text.len() - trimmed.len();
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            let bullet_mark = if unicode >= UnicodeLevel::Bmp { "• " } else { "- " };
            spans.push(Span::raw(format!("{}{bullet_mark}", &text[..indent])));
            body = rest;
            break;
        }
    }
    for (piece, emphasis) in inline_markdown(body) {
        spans.push(match emphasis {
            Emphasis::Plain => Span::raw(piece),
            Emphasis::Bold => Span::styled(piece, bold),
            Emphasis::Code => Span::styled(piece, code_style),
        });
    }
    TuiLine::from(spans)
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') { Some(line[hashes..].trim()) } else { None }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Plain,
    Bold,
    Code,
}

/// Splits `**bold**` and `` `code` ``. A marker without a partner stays literal.
pub fn inline_markdown(text: &str) -> Vec<(String, Emphasis)> {
    let mut out: Vec<(String, Emphasis)> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            if !plain.is_empty() {
                out.push((std::mem::take(&mut plain), Emphasis::Plain));
            }
            out.push((after[..end].to_owned(), Emphasis::Code));
            rest = &after[end + 1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
            && end > 0
        {
            if !plain.is_empty() {
                out.push((std::mem::take(&mut plain), Emphasis::Plain));
            }
            // Bold often contains code (`**`path`**`): without this analysis, the backticks stayed
            // displayed (observed).
            for (piece, emphasis) in inline_markdown(&after[..end]) {
                out.push((piece, if emphasis == Emphasis::Code { Emphasis::Code } else { Emphasis::Bold }));
            }
            rest = &after[end + 2..];
            continue;
        }
        let c = rest.chars().next().expect("non-empty");
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !plain.is_empty() {
        out.push((plain, Emphasis::Plain));
    }
    out
}

fn suggestions_widget<'a>(
    suggestions: &[crate::complete::Suggestion],
    selected: usize,
    rounded: bool,
    colors: &Palette,
    width: u16,
) -> Paragraph<'a> {
    let inner = width.saturating_sub(2) as usize;
    // The window follows the selection when the list exceeds what the frame shows.
    let shown = crate::complete::MAX_SUGGESTIONS;
    let first = selected.saturating_sub(shown - 1).min(suggestions.len().saturating_sub(shown));
    let lines: Vec<TuiLine> = suggestions
        .iter()
        .enumerate()
        .skip(first)
        .take(shown)
        .map(|(i, s)| {
            let name = s.replacement.clone();
            let room = inner.saturating_sub(display_width(&name, UnicodeLevel::Full) + 3);
            let detail = truncate(&s.detail, room, UnicodeLevel::Full).to_owned();
            let style = if i == selected {
                colors.style(Color::Cyan, false).add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default()
            };
            if name == "/labs" {
                let mut spans = crate::render::wave(&name, colors, true);
                spans.push(Span::raw("  "));
                spans.extend(crate::render::wave(&detail, colors, false));
                return TuiLine::from(spans);
            }
            TuiLine::from(vec![
                Span::styled(name, style),
                Span::raw("  "),
                Span::styled(detail, colors.style(Color::DarkGray, true)),
            ])
        })
        .collect();
    let title = if suggestions.len() > shown {
        format!(" {} · {}/{} ", po_i18n::t!("view.suggest.keys"), selected + 1, suggestions.len())
    } else {
        format!(" {} ", po_i18n::t!("view.suggest.keys"))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if rounded { ratatui::widgets::BorderType::Rounded } else { ratatui::widgets::BorderType::Plain })
        .border_style(colors.style(Color::DarkGray, true))
        .title(title);
    Paragraph::new(lines).block(block)
}

fn status_widget<'a>(app: &'a App, width: usize, colors: &Palette, unicode: bool) -> Paragraph<'a> {
    // During a turn, progress comes first: it is what the user looks for when nothing moves. A
    // brief message comes next, then the session state.
    let sep = if unicode { " · " } else { " | " };
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let mut used = 1;
    let mut separated = false;
    // The work mode comes first: it tells what the model is allowed to do.
    if let Some(mode) = app.work_mode.as_ref().map(|h| h.get()) {
        let (label, color) = match mode {
            po_agent::work_mode::WorkMode::Plan => ("plan", Color::Cyan),
            po_agent::work_mode::WorkMode::Build => ("builder", Color::Green),
            po_agent::work_mode::WorkMode::Bypass => ("bypass", Color::Red),
        };
        spans.push(Span::styled(format!(" {label} "), colors.style(color, false).add_modifier(Modifier::REVERSED | Modifier::BOLD)));
        spans.push(Span::raw(" "));
        used += label.len() + 3;
    }
    // The brief message goes first: it is often a request ("Esc again to stop…") that would
    // otherwise be hidden behind the progress.
    if let Some(toast) = app.current_toast() {
        let room = width.saturating_sub(used + 1);
        let text = truncate(toast, room, UnicodeLevel::Full).to_owned();
        used += display_width(&text, UnicodeLevel::Full) + 3;
        spans.push(Span::styled(text, colors.style(Color::Yellow, false).add_modifier(Modifier::BOLD)));
        separated = true;
    }
    // A separator between two items only: the mode badge stands apart on its own.
    let dim_sep = || Span::styled(sep, colors.style(Color::DarkGray, true));
    let activity = if let Some(progress) = &app.progress {
        Some(progress.line(app.frame, unicode))
    } else if app.status.busy {
        let glyphs: &[&str] = if unicode {
            &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        } else {
            &["|", "/", "-", "\\"]
        };
        Some(format!("{} {}", glyphs[app.frame % glyphs.len()], po_i18n::t!("view.status.running")))
    } else {
        None
    };
    if let Some(line) = activity {
        if separated {
            spans.push(dim_sep());
        }
        used += line.chars().count();
        spans.push(Span::styled(line, colors.style(Color::Cyan, false)));
        separated = true;
    }
    let rest = app.status.render(width.saturating_sub(used + 4));
    if !rest.is_empty() && width > used + 8 {
        if separated {
            spans.push(dim_sep());
        }
        spans.push(Span::styled(rest, colors.style(Color::DarkGray, true)));
    }
    // Nothing must overflow: on a phone, an abruptly cut end ("(st") read poorly.
    let level = if unicode { UnicodeLevel::Bmp } else { UnicodeLevel::Ascii };
    let owned: Vec<Span<'static>> = spans.into_iter().map(|s| Span::styled(s.content.into_owned(), s.style)).collect();
    Paragraph::new(TuiLine::from(crate::render::clip(owned, width, level)))
}

/// The messages typed during a turn, until the agent takes them: the last one first in line for
/// ↑ (edit) and Esc (cancel).
fn queued_widget(app: &App, width: usize, colors: &Palette, unicode: UnicodeLevel) -> Paragraph<'static> {
    let dim = colors.style(Color::DarkGray, true);
    let accent = colors.style(Color::Yellow, false);
    let mark = if unicode >= UnicodeLevel::Bmp { "⏸" } else { "|" };
    let count = app.queued.len();
    let lines: Vec<ratatui::text::Line<'static>> = app
        .queued
        .iter()
        .enumerate()
        .skip(count.saturating_sub(3))
        .map(|(i, text)| {
            let first = text.lines().next().unwrap_or("").to_owned();
            let mut spans = vec![
                Span::styled(format!("  {mark} "), accent),
                Span::styled(format!("{} · ", po_i18n::t!("tui.queued.label")), accent),
                Span::raw(first),
            ];
            if i + 1 == count {
                spans.push(Span::styled(format!("  {}", po_i18n::t!("tui.queued.hint")), dim));
            }
            ratatui::text::Line::from(crate::render::clip(spans, width, unicode))
        })
        .collect();
    Paragraph::new(lines)
}

fn input_widget<'a>(
    app: &'a App,
    rounded: bool,
    colors: &Palette,
    width: usize,
    visible: usize,
    level: UnicodeLevel,
) -> Paragraph<'a> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if rounded { ratatui::widgets::BorderType::Rounded } else { ratatui::widgets::BorderType::Plain })
        .border_style(match app.work_mode.as_ref().map(|h| h.get()) {
            Some(po_agent::work_mode::WorkMode::Plan) => colors.style(Color::Cyan, false),
            Some(po_agent::work_mode::WorkMode::Bypass) => colors.style(Color::Red, false),
            _ => colors.style(Color::DarkGray, true),
        });

    // A cursor drawn in the text stays visible even when the terminal hides its own, which
    // happens behind some multiplexers.
    let text = app.input.text();
    let cursor = app.input.cursor();
    let (before, after) = split_at_char(text, cursor);
    let mut lines: Vec<TuiLine> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    for (index, piece) in before.split('\n').enumerate() {
        if index > 0 {
            lines.push(TuiLine::from(std::mem::take(&mut current)));
        }
        current.push(Span::raw(piece.to_owned()));
    }
    current.push(Span::styled("▏", colors.style(Color::Cyan, false)));
    for (index, piece) in after.split('\n').enumerate() {
        if index > 0 {
            lines.push(TuiLine::from(std::mem::take(&mut current)));
        }
        current.push(Span::raw(piece.to_owned()));
    }
    if text.is_empty() {
        // Empty input: what can be done, in place of the text. Disappears as soon as typing starts.
        let hint = if app.labs_rewriting.is_some() {
            po_i18n::t!("view.input.hint_contrib_edit")
        } else if app.labs_revising {
            po_i18n::t!("view.input.hint_revise")
        } else if width >= 70 {
            po_i18n::t!("view.input.hint_wide")
        } else if width >= 36 {
            po_i18n::t!("view.input.hint_mid")
        } else {
            po_i18n::t!("view.input.hint_short")
        };
        current.push(Span::styled(hint, colors.style(Color::DarkGray, true)));
    }
    lines.push(TuiLine::from(current));

    // When the input exceeds the visible height, we scroll to keep the cursor in view.
    let rows_to_cursor = input_rows(before, width, level);
    let scroll = rows_to_cursor.saturating_sub(visible) as u16;
    Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((scroll, 0))
}

/// Question and proposed answers, wrapped to `width` columns.
fn question_lines(pending: &crate::question::Pending, width: usize, colors: &Palette, unicode: UnicodeLevel) -> Vec<TuiLine<'static>> {
    let fancy = unicode >= UnicodeLevel::Bmp;
    let accent = colors.style(Color::Cyan, false);
    let dim = colors.style(Color::DarkGray, true);
    let question = &pending.request.question;
    let mut lines: Vec<TuiLine<'static>> = crate::render::wrap_spans(
        vec![Span::styled(question.question.clone(), Style::default().add_modifier(Modifier::BOLD))],
        width,
    )
    .into_iter()
    .map(TuiLine::from)
    .collect();
    lines.push(TuiLine::from(""));
    let pointer = if fancy { "› " } else { "> " };
    let mut entries: Vec<(String, String)> =
        question.options.iter().map(|o| (o.label.clone(), o.description.clone())).collect();
    entries.push((po_i18n::t!("view.question.other_label").to_owned(), po_i18n::t!("view.question.other_desc").to_owned()));
    for (i, (label, description)) in entries.into_iter().enumerate() {
        let selected = i == pending.selected;
        let head = format!("{}{}. ", if selected { pointer } else { "  " }, i + 1);
        let label_style = if selected { accent.add_modifier(Modifier::BOLD) } else { Style::default() };
        let mut row = vec![Span::styled(head, accent), Span::styled(label, label_style)];
        if selected && i + 1 == pending.choices() && let Some(text) = &pending.typing {
            row.push(Span::raw(" : "));
            row.push(Span::raw(text.clone()));
            row.push(Span::styled("▏", accent));
        }
        for wrapped in crate::render::wrap_spans(row, width) {
            lines.push(TuiLine::from(wrapped));
        }
        if !description.is_empty() {
            for wrapped in crate::render::wrap_spans(vec![Span::styled(format!("     {description}"), dim)], width) {
                lines.push(TuiLine::from(wrapped));
            }
        }
    }
    lines
}

/// Preview of the highlighted answer, under a titled rule.
fn preview_lines(preview: &str, width: usize, colors: &Palette, unicode: UnicodeLevel) -> Vec<TuiLine<'static>> {
    let rule = if unicode >= UnicodeLevel::Bmp { "─" } else { "-" };
    let mut lines = vec![TuiLine::from(Span::styled(
        {
            let label = po_i18n::t!("view.question.preview");
            format!("{} {label} {}", rule.repeat(2), rule.repeat(width.saturating_sub(display_width(label, UnicodeLevel::Full) + 4)))
        },
        colors.style(Color::DarkGray, true),
    ))];
    for line in preview.lines() {
        let clean = po_term::width::sanitize_line(line);
        for wrapped in crate::render::wrap_spans(vec![Span::styled(clean, colors.style(Color::Cyan, false))], width) {
            lines.push(TuiLine::from(wrapped));
        }
    }
    lines
}

fn frame_block<'a>(title: String, rounded: bool, colors: &Palette) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if rounded { ratatui::widgets::BorderType::Rounded } else { ratatui::widgets::BorderType::Plain })
        .border_style(colors.style(Color::DarkGray, true))
        .title(Span::styled(title, colors.style(Color::Cyan, false).add_modifier(Modifier::BOLD)))
}

/// Settings panel: tabs, "label · value" rows, explanation of the selected row.
fn panel_widget<'a>(
    panel: &'a crate::settings::Panel,
    rounded: bool,
    colors: &Palette,
    unicode: UnicodeLevel,
    width: usize,
    height: u16,
) -> Paragraph<'a> {
    use crate::settings::Action;
    let fancy = unicode >= UnicodeLevel::Bmp;
    let dim = colors.style(Color::DarkGray, true);
    let accent = colors.style(Color::Cyan, false);
    let mut lines: Vec<TuiLine> = Vec::new();

    // Tabs: the current one in relief, the others numbered to jump to with one key.
    let mut tabs: Vec<Span> = vec![Span::raw(" ")];
    for (i, tab) in panel.tabs.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, tab.title);
        if i == panel.tab {
            tabs.push(Span::styled(label, accent.add_modifier(Modifier::REVERSED | Modifier::BOLD)));
        } else {
            tabs.push(Span::styled(label, dim));
        }
        tabs.push(Span::raw(" "));
    }
    lines.push(TuiLine::from(tabs));
    lines.push(TuiLine::from(Span::styled(if fancy { "─".repeat(width) } else { "-".repeat(width) }, dim)));

    let rows = panel.rows();
    // Room for the rows: frame, tabs, rules and footer removed.
    let room = (height as usize).saturating_sub(7).max(1);
    let first = panel.row.saturating_sub(room.saturating_sub(1));
    let label_width =
        rows.iter().map(|r| display_width(&r.label, UnicodeLevel::Full)).max().unwrap_or(0).min(width / 2).max(8);
    for (i, row) in rows.iter().enumerate().skip(first).take(room) {
        let selected = i == panel.row;
        let pointer = if selected { if fancy { "› " } else { "> " } } else { "  " };
        let label = truncate(&row.label, label_width, UnicodeLevel::Full).to_owned();
        let pad = " ".repeat(label_width.saturating_sub(display_width(&label, UnicodeLevel::Full)) + 2);
        let value_room = width.saturating_sub(label_width + 8);
        let value = truncate(&row.value, value_room, UnicodeLevel::Full).to_owned();
        let (left, right) = match row.action {
            Action::Choice { .. } if selected => {
                if fancy {
                    ("◂ ", " ▸")
                } else {
                    ("< ", " >")
                }
            }
            Action::Run(_) | Action::Prompt(_) if selected => ("", if fancy { "  ↵" } else { "  <-" }),
            _ => ("", ""),
        };
        let label_style = if selected { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
        let value_style = match row.action {
            Action::None => dim,
            _ if selected => accent,
            _ => Style::default(),
        };
        let mut spans = vec![Span::styled(pointer, accent)];
        if row.featured {
            spans.extend(crate::render::wave(&label, colors, true));
        } else {
            spans.push(Span::styled(label, label_style));
        }
        spans.extend([
            Span::raw(pad),
            Span::styled(left, accent),
            Span::styled(value, value_style),
            Span::styled(right, accent),
        ]);
        lines.push(TuiLine::from(spans));
    }
    if rows.is_empty() {
        lines.push(TuiLine::from(Span::styled(format!("  {}", po_i18n::t!("view.panel.empty")), dim)));
    }

    // Footer: the explanation of the selected row, then the keys.
    let filler = (height as usize).saturating_sub(lines.len() + 5);
    for _ in 0..filler {
        lines.push(TuiLine::from(""));
    }
    lines.push(TuiLine::from(Span::styled(if fancy { "─".repeat(width) } else { "-".repeat(width) }, dim)));
    let hint = panel.current().map(|r| r.hint.as_str()).unwrap_or("");
    let hint = format!(" {}", truncate(hint, width.saturating_sub(2), UnicodeLevel::Full));
    if panel.current().is_some_and(|r| r.featured) {
        lines.push(TuiLine::from(crate::render::wave(&hint, colors, false)));
    } else {
        lines.push(TuiLine::from(Span::styled(hint, dim)));
    }
    let keys = if width >= 60 {
        format!(" {}", po_i18n::t!("view.panel.keys_wide"))
    } else {
        format!(" {}", po_i18n::t!("view.panel.keys_narrow"))
    };
    lines.push(TuiLine::from(Span::styled(
        if fancy {
            keys
        } else {
            keys.replace('↑', "^").replace('↓', "v").replace('←', "<").replace('→', ">").replace('·', "|")
        },
        accent,
    )));
    Paragraph::new(lines).block(frame_block(format!(" {} ", po_i18n::t!("view.panel.title")), rounded, colors))
}

/// Palette: the search, then the best matches.
fn finder_widget<'a>(
    finder: &'a crate::settings::Finder,
    rounded: bool,
    colors: &Palette,
    width: usize,
) -> Paragraph<'a> {
    let dim = colors.style(Color::DarkGray, true);
    let accent = colors.style(Color::Cyan, false);
    let mut lines = vec![TuiLine::from(vec![
        Span::styled(" › ", accent),
        Span::raw(finder.query.clone()),
        Span::styled("▏", accent),
        if finder.query.is_empty() { Span::styled(po_i18n::t!("view.finder.placeholder"), dim) } else { Span::raw("") },
    ])];
    let matches = finder.matches();
    if matches.is_empty() {
        lines.push(TuiLine::from(Span::styled(format!("   {}", po_i18n::t!("view.finder.none")), dim)));
    }
    for (i, item) in matches.iter().take(crate::settings::FINDER_ROWS).enumerate() {
        let selected = i == finder.selected;
        let label = truncate(&item.label, width.saturating_sub(4), UnicodeLevel::Full).to_owned();
        let room = width.saturating_sub(display_width(&label, UnicodeLevel::Full) + 6);
        let detail = truncate(&item.detail, room, UnicodeLevel::Full).to_owned();
        lines.push(TuiLine::from(vec![
            Span::styled(if selected { " › " } else { "   " }, accent),
            Span::styled(label, if selected { accent.add_modifier(Modifier::BOLD) } else { Style::default() }),
            Span::raw("  "),
            Span::styled(detail, dim),
        ]));
    }
    Paragraph::new(lines).block(frame_block(format!(" {} ", po_i18n::t!("view.finder.title")), rounded, colors))
}

fn split_at_char(text: &str, index: usize) -> (&str, &str) {
    let offset = text.char_indices().nth(index).map(|(offset, _)| offset).unwrap_or(text.len());
    text.split_at(offset)
}

fn help_widget<'a>(rounded: bool, colors: &Palette) -> Paragraph<'a> {
    let lines: Vec<TuiLine> = shortcuts()
        .into_iter()
        .map(|(key, description)| {
            TuiLine::from(vec![
                Span::styled(format!(" {key:<12}"), colors.style(Color::Cyan, false)),
                Span::raw(description.to_string()),
            ])
        })
        .collect();

    Paragraph::new(lines).block(Block::default().title(format!(" {} ", po_i18n::t!("view.help.title"))).borders(Borders::ALL).border_type(if rounded {
        ratatui::widgets::BorderType::Rounded
    } else {
        ratatui::widgets::BorderType::Plain
    }))
}

/// Body of an approval request: the command (or path) wrapped at spaces, an empty line, then
/// the keys. The last row is always the footer.
pub fn approval_lines(summary: &str, width: usize, colors: &Palette) -> Vec<TuiLine<'static>> {
    let mut lines: Vec<TuiLine<'static>> = Vec::new();
    for part in summary.lines() {
        let clean = po_term::width::sanitize_line(part);
        for row in crate::render::wrap_spans(vec![Span::styled(clean, colors.style(Color::Cyan, false))], width.max(8)) {
            lines.push(TuiLine::from(row));
        }
    }
    lines.push(TuiLine::from(""));
    lines.push(TuiLine::from(vec![
        Span::styled(po_i18n::t!("view.approval.key_allow"), colors.style(Color::Green, false).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {}   ", po_i18n::t!("view.approval.allow"))),
        Span::styled(po_i18n::t!("view.approval.key_deny"), colors.style(Color::Red, false).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {} ", po_i18n::t!("view.approval.or"))),
        Span::styled(po_i18n::t!("view.approval.esc"), colors.style(Color::Red, false).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {}", po_i18n::t!("view.approval.deny"))),
    ]));
    lines
}

/// List of local models, with the highlighted row and the server state at the foot.
fn picker_widget<'a>(
    picker: &Picker,
    rounded: bool,
    colors: &Palette,
    unicode: UnicodeLevel,
    width: usize,
) -> Paragraph<'a> {
    // The cursor must stay visible on a terminal without Unicode: the arrow gives way to an
    // ASCII chevron rather than a white square.
    let pointer = if unicode >= UnicodeLevel::Bmp { "›" } else { ">" };

    let mut lines: Vec<TuiLine> = picker
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let chosen = index == picker.selected();
            let tone = if !entry.viable {
                Color::DarkGray
            } else if entry.loaded {
                Color::Green
            } else {
                Color::Reset
            };
            let (left, right) = picker.row(index, width, unicode, pointer);
            TuiLine::from(vec![
                Span::styled(left, colors.style(tone, chosen)),
                Span::styled(right, colors.style(Color::DarkGray, false)),
            ])
        })
        .collect();

    // The server state is not sacrificed: it is the information the user comes looking for. If
    // the two do not fit side by side, they go on two rows rather than dropping one of them.
    let keys = po_i18n::t!("view.picker.keys");
    let server = format!(" {}", po_i18n::t!("view.picker.server", server = picker.server));
    lines.push(TuiLine::from(""));
    if display_width(&server, unicode) + display_width(keys, unicode) + 4 <= width {
        lines.push(TuiLine::from(Span::styled(format!("{server}    {keys}"), colors.style(Color::Cyan, false))));
    } else {
        lines.push(TuiLine::from(Span::styled(
            truncate(&server, width, unicode).to_owned(),
            colors.style(Color::Cyan, false),
        )));
        lines.push(TuiLine::from(Span::styled(
            truncate(&format!(" {keys}"), width, unicode).to_owned(),
            colors.style(Color::Cyan, false),
        )));
    }

    Paragraph::new(lines).block(
        Block::default()
            .title(format!(" {} ", po_i18n::t!("view.picker.title")))
            .borders(Borders::ALL)
            .border_type(if rounded {
                ratatui::widgets::BorderType::Rounded
            } else {
                ratatui::widgets::BorderType::Plain
            })
            .border_style(colors.style(Color::Cyan, false)),
    )
}

/// Centered area, bounded to what the screen can actually display.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_centered_area_stays_within_the_screen() {
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let popup = centered(area, 40, 10);
        assert_eq!(popup.width, 40);
        assert_eq!(popup.x, 20);
        assert_eq!(popup.y, 7);
    }

    #[test]
    fn a_window_larger_than_the_screen_is_bounded() {
        // On a phone in portrait mode, a 70-column window does not fit.
        let area = Rect { x: 0, y: 0, width: 30, height: 8 };
        let popup = centered(area, 70, 20);
        assert_eq!(popup.width, 30);
        assert_eq!(popup.height, 8);
        assert_eq!(popup.x, 0);
    }

    #[test]
    fn without_color_the_style_stays_neutral() {
        let palette = Palette::new(ColorDepth::None);
        assert_eq!(palette.style(Color::Yellow, false).fg, None);
        let colored = Palette::new(ColorDepth::TrueColor);
        assert_eq!(colored.style(Color::Yellow, false).fg, Some(Color::Yellow));
    }

    #[test]
    fn input_splits_at_the_cursor_without_breaking_utf8() {
        assert_eq!(split_at_char("café", 2), ("ca", "fé"));
        assert_eq!(split_at_char("café", 4), ("café", ""));
        assert_eq!(split_at_char("café", 99), ("café", ""));
        assert_eq!(split_at_char("", 0), ("", ""));
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    #[test]
    fn bold_and_code_are_recognized() {
        let pieces = inline_markdown("It declares **1 server** : `fs`.");
        assert_eq!(
            pieces,
            vec![
                ("It declares ".into(), Emphasis::Plain),
                ("1 server".into(), Emphasis::Bold),
                (" : ".into(), Emphasis::Plain),
                ("fs".into(), Emphasis::Code),
                (".".into(), Emphasis::Plain),
            ]
        );
    }

    #[test]
    fn code_inside_bold_and_headings_loses_its_backticks() {
        let pieces = inline_markdown(r"**`E:\LLM\`** : production");
        assert_eq!(pieces[0], (r"E:\LLM\".to_owned(), Emphasis::Code));
        assert_eq!(pieces[1], (" : production".to_owned(), Emphasis::Plain));
        let mut in_code = false;
        let heading_line = markdown_line(r"### `D:\Projects\` — main", &mut in_code, UnicodeLevel::Full, &Palette::new(ColorDepth::None));
        let text: String = heading_line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, r"D:\Projects\ — main");
    }

    #[test]
    fn an_unclosed_marker_stays_literal() {
        let pieces = inline_markdown("start **mid-stream");
        assert_eq!(pieces, vec![("start **mid-stream".into(), Emphasis::Plain)]);
        assert_eq!(inline_markdown("a ` b"), vec![("a ` b".into(), Emphasis::Plain)]);
    }

    #[test]
    fn no_text_is_lost() {
        for text in ["**a** `b` c", "`x`**y**", "****", "``", "é**è**à", "- **list**"] {
            let rendered: String = inline_markdown(text).into_iter().map(|(t, _)| t).collect();
            let expected = text.replace("**", "").replace('`', "");
            // Only paired markers disappear; the text never does.
            assert!(rendered.len() >= expected.len(), "{text} → {rendered}");
        }
    }

    #[test]
    fn code_blocks_span_lines() {
        let palette = Palette::new(ColorDepth::TrueColor);
        let mut in_code = false;
        markdown_line("```rust", &mut in_code, UnicodeLevel::Full, &palette);
        assert!(in_code);
        let line = markdown_line("let **x** = 1;", &mut in_code, UnicodeLevel::Full, &palette);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("**x**"), "no markdown inside a code block: {text}");
        markdown_line("```", &mut in_code, UnicodeLevel::Full, &palette);
        assert!(!in_code);
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    fn rows_of(n: usize, width: usize) -> Vec<TuiLine<'static>> {
        (0..n).map(|i| TuiLine::from(format!("{i:03} {}", "x".repeat(width)))).collect()
    }

    #[test]
    fn without_scrolling_the_end_is_visible() {
        // 50 lines of 2 rows each at 20 columns, a 10-row screen.
        let (window, top) = bottom_window(rows_of(50, 30), 20, 10, 0);
        let last = window.last().unwrap().spans[0].content.to_string();
        assert!(last.starts_with("049"), "{last}");
        let rows: usize =
            window.iter().map(|l| Paragraph::new(l.clone()).wrap(Wrap { trim: false }).line_count(20)).sum();
        assert_eq!(rows - top, 10, "the screen is filled to the bottom");
    }

    #[test]
    fn scrolling_goes_up_by_as_many_rows() {
        let (window, top) = bottom_window(rows_of(50, 5), 40, 10, 4);
        // One-row lines: the window holds 14 lines, 0 are skipped at the top and the last 4 are
        // below the screen.
        assert_eq!(window.len(), 14);
        assert_eq!(top, 0);
    }

    #[test]
    fn a_short_conversation_stays_at_the_top() {
        let (window, top) = bottom_window(rows_of(3, 5), 40, 10, 0);
        assert_eq!(window.len(), 3);
        assert_eq!(top, 0);
    }
}
