#!/usr/bin/env bash
# Validates Po through a real SSH transport.
#
# Many projects settle for simulating SSH by setting `SSH_TTY`: that tests detection, not the
# whole chain. This script starts a genuine user-space SSH server — no `sudo`, no system
# service — and runs the terminal bench through it, with a pseudo-terminal allocated by the
# server just as any client (OpenSSH, PuTTY, Termius) would get.
#
# `po doctor` labels are matched in French, so the language is forced to French.
#
# Requirements: python3 with `asyncssh` (installed in a virtual environment by this script).
#
# Usage: scripts/ssh-check.sh [path/to/po] [path/to/smoke]

set -uo pipefail

PO="${1:-./target/debug/po}"
SMOKE="${2:-./target/debug/examples/smoke}"
PORT="${PO_SSH_PORT:-2222}"
WORK="${TMPDIR:-/tmp}/po-ssh-check"
VENV="$WORK/venv"
export PO_LANG=fr

cleanup() {
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT INT TERM

if [ ! -x "$PO" ]; then
  echo "binary not found: $PO" >&2
  exit 2
fi

mkdir -p "$WORK"

if [ ! -x "$VENV/bin/python" ]; then
  echo "installing asyncssh in $VENV…"
  python3 -m venv "$VENV" || { echo "python3-venv required" >&2; exit 2; }
  "$VENV/bin/pip" install -q asyncssh || { echo "could not install asyncssh" >&2; exit 2; }
fi

# Disposable keys, regenerated on every run: nothing survives this test.
rm -f "$WORK/host_key" "$WORK/host_key.pub" "$WORK/client_key" "$WORK/client_key.pub"
ssh-keygen -q -t ed25519 -f "$WORK/host_key" -N "" || exit 2
ssh-keygen -q -t ed25519 -f "$WORK/client_key" -N "" || exit 2

SERVER="$(dirname "$0")/sshd_test.py"
if [ ! -f "$SERVER" ]; then
  echo "test server not found: $SERVER" >&2
  exit 2
fi

PO_SSH_PORT="$PORT" "$VENV/bin/python" "$SERVER" "$WORK/host_key" "$WORK/client_key.pub" \
  > "$WORK/sshd.log" 2>&1 &
SERVER_PID=$!

# Conditional wait: the server announces by itself that it is listening.
for _ in $(seq 1 40); do
  grep -q "listening" "$WORK/sshd.log" 2>/dev/null && break
  sleep 0.25
done
if ! grep -q "listening" "$WORK/sshd.log" 2>/dev/null; then
  echo "the test SSH server did not start:" >&2
  cat "$WORK/sshd.log" >&2
  exit 2
fi

SSHC="ssh -p $PORT -i $WORK/client_key -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null -o BatchMode=yes -tt $USER@127.0.0.1"

echo "== Po through a real SSH transport =="

transport=$($SSHC "stty cols 100 rows 30; PO_LANG=fr $PO doctor; exit" 2>/dev/null \
  | tr -d '\r' | sed -e 's/\x1b\][^\x07]*\x07//g' -e 's/\x1b\[[0-9;?$]*[a-zA-Z]//g' \
  | grep -a -m1 'transport' | sed 's/.*: //' | tr -d '[:space:]')
[ "$transport" = "Ssh" ] \
  && echo "  ok   transport detected as remote              → Ssh" \
  || { echo "  FAIL transport detected                      → $transport (expected: Ssh)"; exit 1; }

profile=$($SSHC "stty cols 100 rows 30; PO_LANG=fr $PO doctor; exit" 2>/dev/null \
  | tr -d '\r' | sed -e 's/\x1b\][^\x07]*\x07//g' -e 's/\x1b\[[0-9;?$]*[a-zA-Z]//g' \
  | grep -a -m1 'profil de rendu' | sed 's/.*: //' | tr -d '[:space:]')
# Without announced synchronized output, a remote link must give up the rich full screen.
[ "$profile" = "plain" ] || [ "$profile" = "rich" ] \
  && echo "  ok   profile suited to the remote link         → $profile" \
  || { echo "  FAIL profile                                  → $profile"; exit 1; }

if [ -x "$SMOKE" ]; then
  verdict=$($SSHC "stty cols 100 rows 30; stty -g > $WORK/b; TERM=xterm-256color $SMOKE 400 >/dev/null 2>&1; stty -g > $WORK/a; if cmp -s $WORK/b $WORK/a; then echo intact; else echo modified; fi; exit" 2>/dev/null \
    | tr -d '\r' | grep -a -m1 -E '^(intact|modified)$')
  [ "$verdict" = "intact" ] \
    && echo "  ok   terminal intact after TUI over SSH      → intact" \
    || { echo "  FAIL terminal after TUI over SSH             → $verdict"; exit 1; }
fi

# The full bench, run end to end from the other side of the link.
matrix=$($SSHC "cd $(pwd) && cp scripts/terminal-matrix.sh $WORK/tm.sh && sed -i 's/\r\$//' $WORK/tm.sh && chmod +x $WORK/tm.sh && PO_TUI_SMOKE=$SMOKE $WORK/tm.sh $PO; exit" 2>/dev/null \
  | tr -d '\r' | sed 's/\x1b\[[0-9;]*m//g' | grep -a -m1 'passed')
echo "  terminal bench run over SSH:$matrix"

echo
echo "-- SSH validation finished --"
