# Po

[![CI](https://github.com/LeGeRyChEeSe/po/actions/workflows/ci.yml/badge.svg)](https://github.com/LeGeRyChEeSe/po/actions/workflows/ci.yml)

A terminal agent harness built for **local models**, that works just as well with cloud ones.

[Français](README.fr.md)

Po does what an agent harness does — agent loop, tool calls, permissions, context management,
MCP — and three things most harnesses don't:

1. **It runs local models, not just talks to them.** Hardware discovery, launch parameters,
   starting and stopping the server, switching models, freeing VRAM. It reads what each local
   server really serves (context window, parallel slots, image support) instead of guessing.
2. **It trades cost against quality by policy.** The orchestrator delegates to subagents
   according to the kind of task and a budget mode you can switch mid-session.
3. **It adapts to the terminal instead of assuming it.** Capabilities are probed, rendering
   degrades cleanly, the terminal is always restored — Windows, Linux, macOS, tmux, SSH, Termux,
   and phones over Termius.

And a companion **Android app** follows and drives your sessions from your phone.

## Install

Ready-made builds for Windows, Linux, macOS and Android (Termux), and the phone app, are on the
[releases page](https://github.com/LeGeRyChEeSe/po/releases). From source (Rust 1.90 or newer):

```bash
cargo build --release
./target/release/po
```

On Windows, `pwsh -File scripts/install.ps1` builds Po and installs it into `~/.cargo/bin`
(already on the PATH), so `po` runs from any folder and any terminal. `po --version` shows the
installed commit.

Release builds keep themselves up to date: once a day Po checks for a new version and installs
it in the background, verified against the release's checksums (`po update off` to opt out).
The phone app offers its own updates the same way.

No configuration is required. On startup Po looks for local inference servers on the usual
ports (llama.cpp, Strata, Ollama, LM Studio, vLLM) and for API keys in the environment
(`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`, `OPENROUTER_API_KEY`,
`DEEPSEEK_API_KEY`).

```bash
po                          # interactive session
po -p "your question"       # a single request, pipe-friendly
po --mode save-max-tokens   # favour savings
po --continue               # resume the last session in this folder
po local                    # what this machine can run
po load [model]             # load a local model in the background, ready for the next session
po unload                   # free the local model's memory
po service install          # keep the phone app's link to this PC, even with no Po open
po update                   # update Po now (release builds also do it by themselves, daily)
po doctor                   # what Po understood about the terminal
po sessions                 # saved sessions
```

## Language

The whole interface is available in **English and French**. Po follows your system language
the first time, then remembers your choice. Switch at any time — even mid-turn — with
`/language en` or `/language fr` (or in the settings, F2): the screen, the commands and the
help change immediately, without restarting.

## In a session

| Input | Effect |
|---|---|
| `/` | command list: ↑↓ to browse, Enter to pick, Enter again to run |
| `@path` | attach a file (or an image, for a model that can see) |
| `!command` | run a shell command now; its output is attached to your next request |
| F2 or `/settings` | settings panel — also usable while the model is working |
| Ctrl+K | search any command or setting |
| Shift+Tab | work mode: plan (read-only), builder (asks before risky actions), bypass |
| Ctrl+O, or a tap on a folded block | unfold reasoning, tool calls, subagent instructions |
| Esc | close what is open; twice to stop the agent and its subagents |
| F3 | voice dictation, when enabled |
| `/help` | every command |

`/verbosity` sets how much of a turn's work is shown, and applies at once, even mid-turn:

| Level | What you see |
|---|---|
| `low` | one live line saying what the model is doing (thinking, running a tool, writing), replaced by its answer when the turn ends |
| `medium` (default) | the answer, with each step's work folded into a summary line you can open |
| `high` | everything unfolded: reasoning, tool calls and their results |

Ctrl+O unfolds or folds the work at any level.

Settings changed during a turn are applied at the next safe point — right after the current
tool calls, before the model's next step — or at the end of the turn. You never need to
restart a session to change something.

The session title is written by the model after the first exchange. Under the input, an info
line shows the time, context used, and live prefill/generation speeds. On tmux, scrolling with
the wheel or a finger (Termius) needs `set -g mouse on` in `~/.tmux.conf`; Po tells you when it
is missing.

### Subagents

The `task` tool hands work to a subagent. Six roles ship with Po — `explorer`, `planner`,
`builder`, `verifier`, `reviewer`, `summarizer` — and you can add your own per project
(`.po/agents/<name>.md`) or globally (`%APPDATA%/po/agents` on Windows,
`~/.config/po/agents` elsewhere). `/agents create <description>` has the session model write
one for you.

- **Live view.** Running subagents appear as a deck of cards in the corner, each in its own
  colour, showing what the subagent says it is doing. Tap a card to open it full-screen: its
  whole conversation — reasoning, tools, text — rendered like the main one. Switch cards with
  `‹ prev · next ›`, the arrow keys, or a horizontal swipe where the terminal sends one.
- **Parallelism** follows the slots the server announces (`/props`). With one slot, the main
  agent waits for its subagent; with N, N subagents run together.
- **Cache-friendly.** On the session's server a subagent reuses the main agent's exact system
  prompt and tools, so the server's prompt cache is kept.

### Media

Po asks each server what its model can read (`input_modalities`, or `/props`). When the model
sees images, `@image.png` attaches one and the `read` tool can open images itself; video and
audio are supported for servers that announce them.

### Voice dictation (optional)

Off by default. `/voice on`, then `/voice install` downloads [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
and a model. A **dictate** button appears under the input (or press F3): tap to record, tap
again to transcribe into the input — nothing is sent until you press Enter. CPU (`small`, about
3.5 s per sentence) or GPU (`turbo`, about 2 s) — `/voice engine gpu` picks the freest card.

### Po Labs

**Po Labs** lets you shape Po just by talking about it. Off by default — turn it on with
`/labs on` or at the top of the settings. Once on, the model spots what in your messages is
about Po itself (a misrendered text, a colour to change, a bug, an idea), records it apart
from the rest of your request, and shows it in a card; `/labs` lists what was recorded. Next
steps: trying these changes live on your own Po, then offering them to everyone, credited to
you. See [docs/po-labs.md](docs/po-labs.md).

### Remote control

Each full-screen session exposes a local MCP server (127.0.0.1, secret token in the URL).
`/control` shows the address. Another agent — another Po, Claude Code, any MCP client — can
then see the screen, read the conversation, change a setting, send a message, or restart Po
on the same session with the freshly installed binary (`/restart`).

### Phone app

The Android app follows your sessions from anywhere and drives them: read the conversation as it
happens, send messages and dictate them, approve tool calls, switch models and settings, open a
new session in any of your projects, watch the PC's GPUs, memory and disks, and unload the local
model. There is nothing to copy between devices: sign in with the same GitHub account on both.

1. In Po, `/rc` signs this PC in with GitHub and names it.
2. `po service install` keeps the link alive in the background, started at logon, even when no
   Po is open (`po service status` tells how it is).
3. Install the app from the [releases page](https://github.com/LeGeRyChEeSe/po/releases) and sign
   in with the same account: the PC accepts the phone by itself within seconds.

The phone and the PC talk directly over QUIC, each pinning the other's key, on the local network
or across the internet: there is no server of ours in between. GitHub only serves as the
directory, a secret gist on your account where your devices find each other, their addresses
encrypted. An iOS app is planned: its core is
already shared with Android ([mobile/ios](mobile/ios/README.md)).

## Budget modes

| Mode | Orchestrator | Subagents |
|---|---|---|
| `max-quality` | best model | best model |
| `balanced` | cloud | local for search, summaries, classification |
| `save-max-tokens` | cloud | smallest local model above the task's quality floor |
| `offline` | local | local |

Ctrl+G cycles modes mid-conversation. Every task has a **quality floor** savings never cross:
a code edit never lands on a classification model.

## Local models

Po reads the GGUF header to know the model's real geometry, measures **free** VRAM, derives
`-ngl`, `-c`, `-np` and the cache type, starts `llama-server` and waits until it really
answers. Trade-offs are announced, never silent. An idle server gives its memory back; a server
started by Po stops with Po. Local compute time is measured and shown: a local model is free in
money, not in seconds.

A model can also stay loaded between sessions: when you leave Po it asks whether to keep the
model in memory, `po load` loads one in advance, and `po unload` (or `/model unload`, or the
phone app) frees it, including a Strata server you started yourself.

## Configuration

Optional, in `po.toml` (current folder, `.po/`, or your user config folder):

```toml
budget = "balanced"

[[provider]]
name = "local"
kind = "openai"
base_url = "http://127.0.0.1:8080/v1"

[[model]]
id = "qwen3-27b"
provider = "local"
tier = "strong"
context_window = 32768

[[mcp]]
name = "files"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
```

See [docs/configuration.md](docs/configuration.md) for every option and environment variable.

## Safety

- File tools are **confined to the workspace**; a path that leaves it is refused, not rewritten.
- Commands are classified **from their actual text**: `git status` runs quietly,
  `ls && rm -rf ~` asks first, a handful of irreversible patterns are refused outright. This is
  not a sandbox — a shell is a shell — but your consent is never bypassed.
- MCP tools always ask: third-party code whose effects can't be reasoned about.
- Text from the model, tools or third-party servers is sanitised so it can't repaint your
  terminal.

## Terminals

Three rendering profiles: `rich` (full screen), `plain` (sober, for slow links and small
screens), `stream` (line by line, no alternate screen — works everywhere, pipes and CI
included). Validated by real runs on Windows/ConPTY, Linux PTY, tmux, GNU screen, zellij, SSH,
Termux on Android 15 and ARM64.

## Documentation

- [Getting started](docs/getting-started.md)
- [Configuration](docs/configuration.md)
- [Po Labs](docs/po-labs.md)
- [Releasing](docs/releasing.md)
- [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE), at
your option. Unless you explicitly state otherwise, any contribution you submit is dual-licensed
the same way, without additional terms.
