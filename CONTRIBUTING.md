# Contributing to Po

Thanks for helping! Po is built around one idea: a harness that runs **local models** well, on
any terminal. Contributions that make it more reliable, more portable or easier to use are the
most welcome.

## Before you start

- Open an issue for anything larger than a fix, so we can agree on the approach first.
- One pull request per change. Small, focused PRs get reviewed faster.

## Building and checking

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # must be warning-free
```

`po dev` starts a session against a built-in fake server (fake subagents, no real model): handy
to try interface changes without a model.

Formatting follows `rustfmt` with `max_width = 120` and `use_small_heuristics = "Max"`. Please
format only the files you touch: running `cargo fmt` on the whole workspace reformats code you
did not change.

## Project layout

| Path | What it holds |
|---|---|
| `crates/po` | the `po` binary: startup, slash commands, settings, the phone service |
| `crates/po-agent` | the agent loop: turns, tool calls, context management, subagents |
| `crates/po-core` | types shared by the rest |
| `crates/po-provider` | access to models (OpenAI-compatible, Anthropic, local servers) |
| `crates/po-router` | which model answers what, at what cost (budget modes) |
| `crates/po-local` | local inference: hardware, GGUF, launch plans, llama.cpp and Strata |
| `crates/po-tools` | the agent's tools and their guardrails |
| `crates/po-mcp` | the Model Context Protocol client |
| `crates/po-term` | terminal detection, input and safe output |
| `crates/po-tui` | the full-screen interface |
| `crates/po-i18n` | interface texts in English and French |
| `crates/po-link` | the phone-to-PC link (QUIC, directory, NAT traversal) |
| `crates/po-wire` | the remote-control protocol shared by the PC and the phone |
| `crates/po-mobile` | the phone's core, exposed to Kotlin (and later Swift) through UniFFI |
| `mobile/android` | the Android app (Kotlin, Jetpack Compose) |
| `catalog` | the community extensions catalog |

## The Android app

`pwsh -File scripts/build-android.ps1 -Test` builds the Rust core for the phone, generates its
Kotlin bindings, builds the APK and runs the app's unit tests. It needs the Android SDK and NDK,
the Rust targets `aarch64-linux-android` and `x86_64-linux-android`, `cargo-ndk`, and a JDK 17+
(see the top of the script). Texts shown by the app live in `res/values/strings.xml` (English)
and `res/values-fr/strings.xml` (French).

## Continuous integration

Every pull request runs the test suite and clippy on Linux, macOS and Windows, the terminal
portability bench, rustdoc, a dependency audit, the minimum Rust version check, and the Android
builds. Pushing a `vX.Y.Z` tag builds every platform and drafts a release.

## Interface text

Every text shown to the user goes through the translation catalogs in
`crates/po-i18n/locales/*.toml` and the `t!` macro, with both an English and a French version:

```toml
[tui.toast.example]
fr = "exemple : {n} éléments"
en = "example: {n} items"
```

`cargo test -p po-i18n` fails if a key used in the code is missing, or if the two languages
don't have the same `{parameters}`. Text written **for the model** (system prompt, tool
descriptions, tool results) is plain English in the code and is not translated.

## The core

Some parts of Po are its core and must keep working whatever else changes: the agent loop,
providers, tools and permissions, session saving and restoring, the terminal restore on exit,
and the update/rollback path. Changes there need tests that show the behaviour is preserved.

## Po Labs

Users can record feedback about Po from inside a session with **Po Labs** (`/labs on`). Those
requests are the raw material for future improvements; contributions that turn them into real
changes are credited to the people who proposed them.

## License

By contributing, you agree that your contributions are dual-licensed under the MIT and Apache
2.0 licenses, like the rest of the project.
