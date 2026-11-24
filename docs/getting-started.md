# Getting started

## 1. Install

Ready-made builds are on the [releases page](https://github.com/LeGeRyChEeSe/po/releases): unpack
the archive for your system and put `po` on your PATH. To build from source instead, you need a
Rust toolchain (1.90 or later).

```bash
git clone https://github.com/LeGeRyChEeSe/po
cd po
cargo build --release
```

The binary is `target/release/po`. On Windows, `pwsh -File scripts/install.ps1` builds it and
copies it into `~/.cargo/bin`, so `po` works from any folder.

## 2. Give Po a model

Po needs at least one model. Any of these is enough:

- **A local server already running** — llama.cpp (`llama-server`), Strata, Ollama, LM Studio or
  vLLM on its usual port. Po finds it on startup and reads its real context window, its number
  of parallel slots, and whether it can read images.
- **A GGUF file on disk** — `po local` shows what your machine can run; in a session,
  `/local` lists the models found and `/local 1` starts the first one with launch parameters
  computed from your free VRAM (needs `llama-server`, found on the PATH or set with
  `PO_LLAMA_SERVER`).
- **A cloud API key** in the environment: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  `GROQ_API_KEY`, `OPENROUTER_API_KEY` or `DEEPSEEK_API_KEY`.

## 3. First session

```bash
cd your-project
po
```

Type a request and press Enter. A few things worth knowing:

- **`/`** opens the command list; **F2** the settings (they also work while the model is
  busy); **Ctrl+K** searches everything; **F1** lists the shortcuts.
- **Shift+Tab** cycles the work mode: *plan* (read-only, the model proposes a plan),
  *builder* (it asks before risky actions — the default), *bypass* (it never asks).
- **`@path/to/file`** attaches a file to your message; **`!command`** runs a shell command and
  attaches its output to your next message.
- **Esc** closes what is open; press it twice to stop the agent.
- Your session is saved after every turn. `po --continue` resumes the last one in the current
  folder, `po sessions` lists them.

## 4. Make it yours

- `/language en` or `/language fr` switches the whole interface instantly.
- `/verbosity low|medium|high` sets how much of the model's work is shown: one live line, folded
  summaries (the default), or everything.
- `/agents` shows the subagents; `/agents create <description>` writes a new one for you.
- `/voice on` then `/voice install` enables voice dictation (whisper.cpp, runs on your machine).
- `/labs on` turns on **Po Labs**: tell Po what you'd like to change about Po itself, and it
  keeps track of it. See [Po Labs](po-labs.md).

## 5. On your phone

The Android app follows and drives your sessions from anywhere. In Po, `/rc` signs this PC in
with GitHub; `po service install` keeps the link alive in the background; then install the app
from the [releases page](https://github.com/LeGeRyChEeSe/po/releases) and sign in with the same
GitHub account. The PC accepts the phone by itself within seconds.

## Troubleshooting

- **The finger or the wheel doesn't scroll under tmux** — add `set -g mouse on` to
  `~/.tmux.conf`.
- **Odd characters or colours** — `po doctor` shows what Po detected; `PO_UNICODE=ascii` or
  `PO_RENDER=plain` force a simpler rendering.
- **What does my terminal send?** — `/mouse test` (or `/souris test`) shows every mouse event
  and special key Po receives.
