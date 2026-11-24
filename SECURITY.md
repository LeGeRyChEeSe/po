# Security

## Reporting a vulnerability

Please **do not** open a public issue for a security problem. Use GitHub's private
vulnerability reporting on this repository (Security → Report a vulnerability). We will
acknowledge your report and keep you informed while we fix it.

## What Po protects

- **Workspace confinement.** File tools only read and write inside the workspace. Writing a
  file outside it asks you first, call by call, even in bypass mode, and only that file's folder
  opens, for that call; plan mode refuses it.
- **Your consent.** Commands are classified from their actual text; anything risky asks first,
  a few irreversible patterns are refused, and MCP tools always ask. The `bypass` work mode
  turns asking off — use it knowingly.
- **Your terminal.** Text coming from models, tools and third-party servers is sanitised so it
  cannot inject control sequences.
- **Remote control.** The session's MCP endpoint listens on 127.0.0.1 only, behind a random
  token in its URL. The URL is written to a file in your user profile (`po/live/`): anyone who
  can read your profile can drive your session, so keep it private on shared machines.
- **The phone link.** The phone app and Po's service talk directly over QUIC, each side pinning
  the other's public key; no server of ours relays anything. The devices find each other through
  a secret gist on your GitHub account, where addresses are encrypted under a key sealed to each
  device. Anyone holding a token of your GitHub account can add a device, so protect that
  account (two-factor authentication). `/rc` lists the devices and revokes one, which rotates
  the key.

Po is not a sandbox: a shell is a shell. Run untrusted projects in a container or a VM.
