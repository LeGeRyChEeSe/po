Map of Po's source (paths relative to the source root). Read only the file a request points to.

Look and layout (crate po-tui):
- crates/po-tui/src/render.rs: the conversation, block by block.
  - render_block: user and Po messages, reasoning, notices, end-of-turn rule.
  - render_tools: tool call rows. The ✓/✗ icon color is `icon_style`, the tool name is bold, its target dimmed.
  - render_diff / diff_counts: red/green lines of file changes.
  - render_work: the folded "Work" line of the medium verbosity; render_low: the single live line of the low one.
  - render_card / card_body: boxed command reports.
  - Glyphs: symbols and their ASCII fallbacks.
- crates/po-tui/src/view.rs: everything around the conversation.
  - Palette::style(color, dim) gives the style of a ratatui `Color` (nothing but dim/bold on a colorless terminal); `Color::Rgb(r, g, b)` gives a precise shade, e.g. orange `Color::Rgb(255, 165, 0)`.
  - title_widget (top row), status_widget (row above the input), input_widget, queued_widget (waiting messages).
  - meta_widget / segment_spans: the status line under the input.
  - suggestions_widget (slash completion), panel_widget (F2 settings), approval_lines, labs_indicator, voice_button.
- crates/po-tui/src/statusline.rs: status line segments, defaults, /statusline editing.
- crates/po-tui/src/mascot.rs: the panda of the welcome screen, its animation and greetings.
- crates/po-tui/src/lib.rs: App state, key bindings (map_key, queue_key), the event loop, turn driving.
- crates/po-tui/src/monitor.rs: subagent cards.

Texts:
- crates/po-i18n/locales/*.toml: every text shown to the user, with `fr` and `en` (tui*.toml for the interface, commands.toml for slash commands and their help, agent.toml, labs.toml…). Code reads them with `po_i18n::t!("key")` or `t!("key", name = value)`. A new text needs both languages.

Behavior:
- crates/po/src/commands.rs: slash commands (dispatch in `execute`), their help is `cmd.help.text` in commands.toml.
- crates/po/src/settings_panel.rs: the rows of the F2 settings panel.
- crates/po/src/chat.rs: startup, the system prompt (system_prompt), line mode output (render), restart.
- crates/po/src/labs.rs: Po Labs itself (po_labs, po_apply).
- crates/po/src/catalog.rs: the community catalog (/catalogue), extensions installed and removed.
- catalog/<name>/: extensions of the catalog (format in catalog/README.md): skills, subagents, status lines, installed without a build.
- crates/po-agent/src/lib.rs: the agent loop, tool dispatch and permissions, subagents, compaction; session.rs: history and compaction.
- crates/po-tools/src/: tools (fs.rs read/write/edit/list, shell.rs bash, search.rs grep/glob, diff.rs).
- crates/po-provider/src/: model APIs (openai.rs, anthropic.rs). crates/po-router/src/: model choice.

Conventions: rustfmt with max_width 120. Comments explain why, in English. Keep changes small and in the style of the surrounding code.
