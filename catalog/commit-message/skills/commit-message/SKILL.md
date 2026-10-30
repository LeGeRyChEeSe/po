---
name: commit-message
description: Writes the commit message of the changes in progress, in the Conventional Commits format. Use when the user asks for a commit message or to commit.
---

# Commit message

1. Read the changes: `git status --short`, then `git diff --staged` (or `git diff` when nothing is
   staged yet).
2. Pick the type that fits the whole change: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`,
   `build`, `chore`. Add a scope in parentheses when one part of the project is concerned:
   `fix(parser): …`.
3. Write the first line in the imperative, at most 72 characters, without a final period:
   what the change does for whoever uses the project.
4. When the reason is not obvious, add a blank line and a short paragraph: why the change was
   made, what it fixes or allows. Never a list of the files touched: the diff says it.
5. Show the message to the user. Commit only if they asked to, with exactly this message.
