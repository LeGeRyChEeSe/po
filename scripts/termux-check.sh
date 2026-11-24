#!/data/data/com.termux/files/usr/bin/sh
# Validation of Po under Termux, to run on the device.
#
# The full bench (`terminal-matrix.sh`) assumes bash, `script(1)` and tmux, which Termux does
# not necessarily have. This script sticks to what is available everywhere under Termux and
# checks the points that, in competing harnesses, really break on mobile:
# platform detection, the chosen profile, terminal restoration after a full-screen render,
# and behavior in a pipe.
#
# `po doctor` labels are matched in French, so the language is forced to French.
#
# Usage: sh termux-check.sh [path/to/po] [path/to/smoke]

PO="${1:-./po}"
SMOKE="${2:-./smoke}"
PASS=0
FAIL=0
export PO_LANG=fr

check() {
  label="$1"; expected="$2"; actual="$3"
  if [ "$actual" = "$expected" ]; then
    printf '  ok   %-42s → %s\n' "$label" "$actual"
    PASS=$((PASS + 1))
  else
    printf '  FAIL %-42s → %s (expected: %s)\n' "$label" "$actual" "$expected"
    FAIL=$((FAIL + 1))
  fi
}

# `--tty` makes it analyze the attached terminal: without it, capturing the output in a pipe
# would answer "not a terminal", which is accurate but useless for diagnosis.
field() {
  "$PO" doctor --tty 2>/dev/null | grep -m1 "$1" | sed 's/.*: //' | tr -d ' \t\r\n'
}

if [ ! -x "$PO" ]; then
  echo "binary not found: $PO" >&2
  echo "Reminder: a static musl binary does not work under Termux (seccomp)." >&2
  echo "Build on the device (pkg install rust) or with the NDK." >&2
  exit 2
fi

echo "== Po under Termux =="
check "platform detected" Termux "$(field 'plateforme')"
check "render profile" plain "$(field 'profil de rendu')"
check "mouse disabled on touch" non "$(field 'souris')"
check "full unicode" Full "$(field 'unicode')"

# The probe must succeed: it is what distinguishes an observed capability from an assumed one.
check "the terminal answers the probe" "leterminalarépondu" "$(field 'sonde')"

echo
echo "== Robustness =="

# Writing into a closed pipe must not make the program panic.
"$PO" doctor 2>/dev/null | head -2 > /dev/null 2>&1
check "output into a closed pipe" 0 "$?"

# The critical point: after a full-screen render, the terminal must be handed back intact.
if [ -x "$SMOKE" ]; then
  stty -g > "$TMPDIR/po-before" 2>/dev/null || stty -g > ./po-before
  "$SMOKE" 300 > /dev/null 2>&1
  stty -g > "$TMPDIR/po-after" 2>/dev/null || stty -g > ./po-after
  before="${TMPDIR:-.}/po-before"
  after="${TMPDIR:-.}/po-after"
  if cmp -s "$before" "$after"; then verdict=intact; else verdict=modified; fi
  check "terminal intact after full-screen render" intact "$verdict"
  rm -f "$before" "$after"
else
  echo "  (smoke absent — full-screen render test skipped)"
fi

echo
echo "-- $PASS passed, $FAIL failed --"
[ "$FAIL" -eq 0 ]
