#!/usr/bin/env bash
# Validation bench for Po's terminal portability.
#
# Each case runs `po doctor` in a real or simulated terminal environment and checks the chosen
# render profile. The last block is the most important: it verifies that the terminal state is
# *identical* before and after a session that took over the alternate screen — that is, that no
# residue is left in the user's shell.
#
# The `po doctor` labels are matched in French, so the language is forced to French.
#
# Usage: scripts/terminal-matrix.sh [path/to/po]

set -uo pipefail

PO="${1:-./target/debug/po}"
PASS=0
FAIL=0
export PO_LANG=fr

if [ ! -x "$PO" ]; then
  echo "binary not found or not executable: $PO" >&2
  exit 2
fi

# Runs po in a real PTY. `script` allocates the pseudo-terminal; it is the only reliable way
# to test a code path that depends on `isatty`.
#
# The environment of the shell launching this bench may already contain SSH_TTY, TMUX or
# TERMUX_VERSION. These variables would skew the cases meant to be local, so each case starts
# from a clean base and declares only what it wants to simulate.
# `script(1)` does not have the same interface on every system: util-linux expects
# `script -qec "command" file`, the BSD variant — macOS's — expects
# `script -q file command args…`. Without this distinction, the bench would not run at all on
# a Mac, which would be ironic for a portability bench.
if script --version 2>/dev/null | grep -qi "util-linux"; then
  PTY_FLAVOR=gnu
else
  PTY_FLAVOR=bsd
fi

in_pty() {
  if [ "$PTY_FLAVOR" = gnu ]; then
    env -u SSH_TTY -u SSH_CONNECTION -u SSH_CLIENT -u TMUX -u STY -u TERMUX_VERSION -u ZELLIJ \
      script -qec "$*" /dev/null 2>/dev/null | tr -d '\r'
  else
    env -u SSH_TTY -u SSH_CONNECTION -u SSH_CLIENT -u TMUX -u STY -u TERMUX_VERSION -u ZELLIJ \
      script -q /dev/null /bin/sh -c "$*" 2>/dev/null | tr -d '\r'
  fi
}

# `script` records the raw PTY stream: the control sequences emitted by the probe end up in it
# as is and may precede the first useful line. They are removed before analyzing the output —
# a real terminal consumes them without displaying them.
strip_ansi() {
  sed -e 's/\x1b\][^\x07]*\x07//g' -e 's/\x1b\[[0-9;?$]*[a-zA-Z]//g' -e 's/\x1b[()][A-B0-9]//g'
}

profile_of() {
  strip_ansi | grep -a -m1 'profil de rendu' | sed 's/.*: //' | tr -d '[:space:]'
}

check() {
  local label="$1" expected="$2" actual="$3"
  if [ "$actual" = "$expected" ]; then
    printf '  \033[32mok\033[0m   %-46s → %s\n' "$label" "$actual"
    PASS=$((PASS + 1))
  else
    printf '  \033[31mFAIL\033[0m %-46s → %s (expected: %s)\n' "$label" "$actual" "$expected"
    FAIL=$((FAIL + 1))
  fi
}

echo "== Render profile detection =="

# Without a TTY, the output goes into a pipe: no alternate screen must be attempted.
check "redirected output (pipe)" stream "$("$PO" doctor | profile_of)"

# Comfortable TTY: full rendering.
check "wide PTY, TERM=xterm-256color" rich \
  "$(TERM=xterm-256color COLORTERM=truecolor in_pty "stty cols 120 rows 40; $PO doctor" | profile_of)"

# Terminal that announces no usable capability.
check "TERM=dumb" stream "$(TERM=dumb in_pty "$PO doctor" | profile_of)"

# A phone in portrait (40 to 50 columns under Termius) keeps full screen and finger scrolling.
check "phone-width PTY (40 columns)" rich \
  "$(TERM=xterm-256color in_pty "stty cols 40 rows 30; $PO doctor" | profile_of)"

# Narrower still: even the block layout does not hold, fall back to plain rendering.
check "narrow PTY (25 columns)" plain \
  "$(TERM=xterm-256color in_pty "stty cols 25 rows 30; $PO doctor" | profile_of)"

# Short window, the case of the on-screen keyboard open on mobile.
check "short PTY (10 rows)" plain \
  "$(TERM=xterm-256color in_pty "stty cols 100 rows 10; $PO doctor" | profile_of)"

# Remote session without synchronized output: full screen would tear.
check "simulated SSH without synchronized output" plain \
  "$(SSH_TTY=/dev/pts/0 PO_NO_PROBE=1 TERM=xterm-256color in_pty "$PO doctor" | profile_of)"

# Termux: full screen acceptable, frills not.
check "simulated Termux" plain \
  "$(TERMUX_VERSION=0.118 PREFIX=/data/data/com.termux/files/usr TERM=xterm-256color \
     in_pty "$PO doctor" | profile_of)"

# Explicit forcing must take precedence over any deduction.
check "--render stream forced on a good terminal" stream \
  "$(TERM=xterm-256color COLORTERM=truecolor in_pty "$PO doctor --render stream" | profile_of)"
check "PO_RENDER=plain" plain \
  "$(PO_RENDER=plain TERM=xterm-256color in_pty "$PO doctor" | profile_of)"

if command -v tmux >/dev/null 2>&1; then
  echo
  echo "== Under tmux =="
  tmux kill-session -t po-matrix 2>/dev/null
  tmux new-session -d -s po-matrix -x 120 -y 40 2>/dev/null
  tmux send-keys -t po-matrix "$PO doctor > /tmp/po-tmux.out 2>&1; tmux wait -S po-done" C-m
  tmux wait -S po-done 2>/dev/null &
  tmux wait po-done 2>/dev/null
  sleep 1
  tmux kill-session -t po-matrix 2>/dev/null
  if [ -f /tmp/po-tmux.out ]; then
    mux=$(grep -m1 '^multiplexeur' /tmp/po-tmux.out | sed 's/.*: //' | tr -d '[:space:]')
    check "tmux detected as multiplexer" tmux "$mux"
    # In tmux, the output was redirected: we must stay in stream mode.
    check "tmux + redirection" stream "$(profile_of < /tmp/po-tmux.out)"
  else
    echo "  (tmux session with no usable output — skipped)"
  fi

  # Really attached tmux session, without redirection: this is a user's nominal case, and
  # they must get full rendering. tmux does not relay the DECRQM request, so synchronized
  # output must be deduced from its version — otherwise every remote tmux user would be
  # downgraded to `plain`.
  tmux kill-session -t po-attach 2>/dev/null
  tmux new-session -d -s po-attach -x 120 -y 40
  tmux send-keys -t po-attach "$PO doctor" C-m
  # Conditional wait rather than a fixed delay: an emulated binary takes several seconds to
  # start, and an arbitrary delay would produce a misleading failure.
  attached=""
  for _ in $(seq 1 40); do
    attached=$(tmux capture-pane -t po-attach -p 2>/dev/null)
    printf '%s' "$attached" | grep -q 'sortie synchro' && break
    sleep 0.25
  done
  tmux kill-session -t po-attach 2>/dev/null
  check "attached tmux (real TTY)" rich \
    "$(printf '%s' "$attached" | grep -a -m1 'profil de rendu' | sed 's/.*: //' | tr -d '[:space:]')"
  check "tmux announces synchronized output" oui \
    "$(printf '%s' "$attached" | grep -a -m1 'sortie synchro' | sed 's/.*: //' | tr -d '[:space:]')"

  # The full-screen TUI in tmux, then verification that the pane's terminal is intact.
  if [ -n "${PO_TUI_SMOKE:-}" ] && [ -x "$PO_TUI_SMOKE" ]; then
    rm -f /tmp/po-tmux-verdict
    tmux kill-session -t po-tui 2>/dev/null
    tmux new-session -d -s po-tui -x 100 -y 24 \
      "bash -c 'stty -g > /tmp/po-tb; TERM=xterm-256color $PO_TUI_SMOKE 300 >/dev/null 2>&1; stty -g > /tmp/po-ta; if diff -q /tmp/po-tb /tmp/po-ta >/dev/null; then echo intact > /tmp/po-tmux-verdict; else echo modified > /tmp/po-tmux-verdict; fi'"
    sleep 4
    tmux kill-session -t po-tui 2>/dev/null
    check "TUI in tmux: terminal intact" intact "$(cat /tmp/po-tmux-verdict 2>/dev/null || echo missing)"
  fi
fi

# GNU screen: the oldest of the multiplexers, and the one with the poorest capabilities — no
# mouse, no hyperlinks. This is the case that verifies Po knows how to step down.
if command -v screen >/dev/null 2>&1; then
  echo
  echo "== Under GNU screen =="
  export SCREENDIR="${SCREENDIR:-/tmp/po-screendir}"
  mkdir -p "$SCREENDIR" && chmod 700 "$SCREENDIR"
  screen -wipe >/dev/null 2>&1

  rm -f /tmp/po-sc-out
  screen -dmS po-matrix bash -c "$PO doctor --tty > /tmp/po-sc-out 2>&1"
  sleep 3
  screen -S po-matrix -X quit >/dev/null 2>&1
  sc_out=$(strip_ansi < /tmp/po-sc-out 2>/dev/null)
  check "screen detected as multiplexer" screen \
    "$(printf '%s' "$sc_out" | grep -a -m1 'multiplexeur' | sed 's/.*: //' | tr -d '[:space:]')"
  # screen swallows mouse events: enabling them would produce stray characters.
  check "mouse disabled under screen" non \
    "$(printf '%s' "$sc_out" | grep -a -m1 'souris' | sed 's/.*: //' | tr -d '[:space:]')"

  if [ -n "${PO_TUI_SMOKE:-}" ] && [ -x "$PO_TUI_SMOKE" ]; then
    rm -f /tmp/po-sc-verdict
    screen -dmS po-restore bash -c "stty -g > /tmp/po-scb; TERM=screen-256color $PO_TUI_SMOKE 300 >/dev/null 2>&1; stty -g > /tmp/po-sca; if cmp -s /tmp/po-scb /tmp/po-sca; then echo intact > /tmp/po-sc-verdict; else echo modified > /tmp/po-sc-verdict; fi"
    sleep 4
    screen -S po-restore -X quit >/dev/null 2>&1
    check "TUI in screen: terminal intact" intact "$(cat /tmp/po-sc-verdict 2>/dev/null || echo absent)"
  fi
fi

# Zellij is the multiplexer Po claims to detect without any test having verified it.
# It does not accept a detached session without a terminal: we provide one, and pass it the
# command through a layout rather than simulated keystrokes.
if command -v zellij >/dev/null 2>&1; then
  echo
  echo "== Under zellij =="
  export ZELLIJ_CONFIG_DIR="${ZELLIJ_CONFIG_DIR:-/tmp/po-zj-config}"
  export ZELLIJ_CACHE_DIR="${ZELLIJ_CACHE_DIR:-/tmp/po-zj-cache}"
  mkdir -p "$ZELLIJ_CONFIG_DIR" "$ZELLIJ_CACHE_DIR"
  zellij delete-all-sessions -y >/dev/null 2>&1
  sleep 1

  rm -f /tmp/po-zj-out
  cat > /tmp/po-zj.kdl <<KDL
layout {
    pane command="bash" {
        args "-c" "$PO doctor --tty > /tmp/po-zj-out 2>&1; sleep 1"
    }
}
KDL
  timeout 40 script -qec "zellij --layout /tmp/po-zj.kdl" /dev/null >/dev/null 2>&1
  zj_out=$(strip_ansi < /tmp/po-zj-out 2>/dev/null)
  check "zellij detected as multiplexer" zellij \
    "$(printf '%s' "$zj_out" | grep -a -m1 'multiplexeur' | sed 's/.*: //' | tr -d '[:space:]')"

  if [ -n "${PO_TUI_SMOKE:-}" ] && [ -x "$PO_TUI_SMOKE" ]; then
    rm -f /tmp/po-zj-verdict
    cat > /tmp/po-zj-restore.kdl <<KDL
layout {
    pane command="bash" {
        args "-c" "stty -g > /tmp/po-zjb; TERM=xterm-256color $PO_TUI_SMOKE 300 >/dev/null 2>&1; stty -g > /tmp/po-zja; if cmp -s /tmp/po-zjb /tmp/po-zja; then echo intact > /tmp/po-zj-verdict; else echo modified > /tmp/po-zj-verdict; fi; sleep 1"
    }
}
KDL
    timeout 40 script -qec "zellij --layout /tmp/po-zj-restore.kdl" /dev/null >/dev/null 2>&1
    check "TUI in zellij: terminal intact" intact "$(cat /tmp/po-zj-verdict 2>/dev/null || echo absent)"
  fi
  zellij delete-all-sessions -y >/dev/null 2>&1
fi

echo
echo "== Terminal state restoration =="

# The decisive test: the `stty` state must be bit-for-bit identical before and after a session
# that enabled raw mode, the alternate screen, the mouse and bracketed paste.
restore_test() {
  local label="$1" cmd="$2"
  local script_body out
  script_body="stty -g > /tmp/po-stty-before; $cmd >/dev/null 2>&1; stty -g > /tmp/po-stty-after"
  if [ "$PTY_FLAVOR" = gnu ]; then
    script -qec "$script_body" /dev/null >/dev/null 2>&1
  else
    script -q /dev/null /bin/sh -c "$script_body" >/dev/null 2>&1
  fi
  if diff -q /tmp/po-stty-before /tmp/po-stty-after >/dev/null 2>&1; then out=same; else out=differs; fi
  check "$label" same "$out"
}

restore_test "normal exit after --demo" "TERM=xterm-256color $PO doctor --demo"
restore_test "exit on unknown command" "TERM=xterm-256color $PO nonexistent-command"

# The full-screen TUI takes over the terminal much more deeply than `--demo`: alternate
# screen, mouse, keyboard. This is the case where a forgotten restoration costs the most.
if [ -n "${PO_TUI_SMOKE:-}" ] && [ -x "$PO_TUI_SMOKE" ]; then
  restore_test "exit after full-screen TUI render" "TERM=xterm-256color $PO_TUI_SMOKE 50"
  # Two outcomes are acceptable: the render took place, or the terminal refused raw mode and
  # the fallback happened cleanly. What is not acceptable is a crash or a hang.
  out=$(in_pty "stty cols 100 rows 30; $PO_TUI_SMOKE 50" | strip_ansi | grep -cE "terminal restauré|repli en" || true)
  check "the TUI ends cleanly" 1 "$out"
  # A phone screen: centered windows must be bounded, not overflow.
  out=$(in_pty "stty cols 32 rows 12; $PO_TUI_SMOKE 50" | strip_ansi | grep -cE "terminal restauré|repli en" || true)
  check "the TUI survives a 32x12 screen" 1 "$out"
fi

echo
echo "-- $PASS passed, $FAIL failed --"
[ "$FAIL" -eq 0 ]
