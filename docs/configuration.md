# Configuration

Po works without any configuration. Everything below is optional.

## `po.toml`

Po reads the first file it finds among `./po.toml`, `./.po/po.toml` and `po/po.toml` in your user
configuration folder (`%APPDATA%` on Windows, `~/.config` elsewhere). The file completes what
Po discovers on its own; a model declared here replaces a discovered one with the same id.

```toml
# Budget mode at startup: max-quality, balanced, save-max-tokens, offline
budget = "balanced"

# false: use only the models declared here — no API keys, no local port probing.
discovery = true

[[provider]]
name = "local"
kind = "openai"                      # wire format: "openai" or "anthropic"
base_url = "http://127.0.0.1:8080/v1"
api_key_env = "MY_KEY"               # optional: name of the variable holding the key

[[model]]
id = "qwen3-27b"                     # id sent to the server
provider = "local"
display_name = "Qwen3 27B"           # optional
tier = "strong"                      # small, strong or frontier
context_window = 32768
max_output_tokens = 4096
input_per_mtok = 0.0                 # price per million tokens, for cloud models
output_per_mtok = 0.0

[[mcp]]                              # MCP servers started with the session
name = "files"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[[mcp]]                              # an HTTP MCP server
name = "remote"
url = "https://example.com/mcp"
headers = { Authorization = "Bearer …" }
```

MCP servers declared in the project's `.mcp.json` are listed by `/mcp` and started on demand
(`/mcp enable <name>`): running third-party code takes an explicit gesture.

## Project files

| File | Role |
|---|---|
| `AGENTS.md`, `CLAUDE.md`, `PO.md` | project instructions, read at startup and given to the model |
| `.po/agents/<name>.md` | subagents for this project |
| `.po/skills/<name>/SKILL.md`, `.claude/skills/…` | skills the model loads when relevant |
| `.mcp.json` | MCP servers available for this project |

## User files

In `%APPDATA%\po` on Windows, `~/.config/po` elsewhere:

| File | Content |
|---|---|
| `ui.json` | interface language, Po Labs on or off |
| `subagents.json` | model per subagent role, parallelism |
| `agents/<name>.md` | subagents available in every project |
| `voice.json` | voice dictation settings |
| `local.json` | extra GGUF folders and the `llama-server` binary |
| `sessions/` | saved sessions |
| `labs/` | Po Labs requests |
| `live/` | addresses of running sessions (remote control) |

Voice dictation binaries and models live in `%LOCALAPPDATA%\po\whisper` (Windows) or
`~/.local/share/po/whisper`.

## Environment variables

| Variable | Effect |
|---|---|
| `PO_LANG` | force the interface language (`en`, `fr`) |
| `PO_REASONING_EFFORT` | initial reasoning effort of local models (`none`, `low`, `medium`, `high`) |
| `PO_SHELL` | shell used by the `bash` tool (default on Windows: Git Bash if present, then PowerShell, then cmd) |
| `PO_LLAMA_SERVER` | path to the `llama-server` binary |
| `PO_MODELS_DIR` | extra folder to scan for GGUF models |
| `PO_RENDER` | force the rendering profile: `rich`, `plain`, `stream` |
| `PO_COLOR` | force the colour depth: `0`, `16`, `256`, `truecolor` |
| `PO_UNICODE` | force the Unicode level: `ascii`, `bmp`, `full` |
| `PO_NO_PROBE` | don't query the terminal for its capabilities |
| `NO_COLOR` | disable colours |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY` | enable the matching cloud models |
