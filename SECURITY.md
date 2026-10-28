# Security

## Reporting a vulnerability

Please **do not** open a public issue for a security problem. Use GitHub's private
vulnerability reporting on this repository (Security → Report a vulnerability). We will
acknowledge your report and keep you informed while we fix it.

## What Po protects

- **Workspace confinement.** File tools only read and write inside the workspace; a path that
  leaves it is refused.
- **Your consent.** Commands are classified from their actual text; anything risky asks first,
  a few irreversible patterns are refused, and MCP tools always ask. The `bypass` work mode
  turns asking off — use it knowingly.
- **Your terminal.** Text coming from models, tools and third-party servers is sanitised so it
  cannot inject control sequences.
- **Remote control.** The session's MCP endpoint listens on 127.0.0.1 only, behind a random
  token in its URL. The URL is written to a file in your user profile (`po/live/`): anyone who
  can read your profile can drive your session, so keep it private on shared machines.

Po is not a sandbox: a shell is a shell. Run untrusted projects in a container or a VM.
