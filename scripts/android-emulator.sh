#!/usr/bin/env bash
# Prepares an Android emulator as a terminal test bench for Po.
#
# A phone plugged in over ADB makes an excellent bench, but it is not always there and it does
# not belong to the project. The Android SDK emulator, on the other hand, is reproducible: same
# image, same answers, on anyone's machine. This script brings an AVD to the state where a real
# Android terminal — Termux — can open an SSH session to the host machine, which is exactly
# the path taken by Po's mobile user.
#
# It does not test Po itself: `terminal-matrix.sh` and `termux-check.sh` do that.
# It makes the bench available, and says frankly what is missing when it does not work.
#
# Usage: scripts/android-emulator.sh [avd_name]

set -uo pipefail

SDK="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/AppData/Local/Android/Sdk}}"
EMULATOR="$SDK/emulator/emulator"
[ -x "$EMULATOR" ] || EMULATOR="$SDK/emulator/emulator.exe"
PORT="${PO_EMULATOR_PORT:-5558}"
SERIAL="emulator-$PORT"
TERMUX_VERSION="${PO_TERMUX_VERSION:-0.118.3}"

if [ ! -x "$EMULATOR" ]; then
  echo "emulator not found under $SDK" >&2
  echo "Set ANDROID_SDK_ROOT, or install the SDK \"emulator\" package." >&2
  exit 2
fi

AVD="${1:-}"
if [ -z "$AVD" ]; then
  AVD=$("$EMULATOR" -list-avds 2>/dev/null | head -1)
  [ -n "$AVD" ] || { echo "no AVD defined — create one with avdmanager." >&2; exit 2; }
  echo "AVD chosen: $AVD"
fi

if ! adb devices | grep -q "^$SERIAL"; then
  echo "starting $AVD on port $PORT…"
  "$EMULATOR" -avd "$AVD" -no-snapshot-save -no-boot-anim -port "$PORT" >/dev/null 2>&1 &
fi

echo -n "waiting for boot"
for _ in $(seq 1 120); do
  [ "$(adb -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ] && break
  echo -n "."
  sleep 5
done
echo
if [ "$(adb -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" != "1" ]; then
  echo "the emulator did not finish booting" >&2
  exit 1
fi

API=$(adb -s "$SERIAL" shell getprop ro.build.version.sdk | tr -d '\r')
ABI=$(adb -s "$SERIAL" shell getprop ro.product.cpu.abi | tr -d '\r')
PAGE=$(adb -s "$SERIAL" shell getconf PAGESIZE 2>/dev/null | tr -d '\r')
echo "== Emulator =="
printf '  API %s · %s · %s-byte pages\n' "$API" "$ABI" "${PAGE:-unknown}"

# The measured trap: on an image with 16 KiB pages, the Termux bootstrap — aligned on
# 4 KiB — cannot be linked at all ("program alignment (4096) cannot be smaller than system
# page size"). The app installs, opens, and never shows a prompt. Better to say so up front
# than to leave people searching.
if [ "${PAGE:-4096}" != "4096" ]; then
  echo "  WARNING: $PAGE-byte pages — the Termux bootstrap cannot link against them."
  echo "  Pick an AVD with 4 KiB pages (image without the ps16k marker)."
fi

if adb -s "$SERIAL" shell pm list packages 2>/dev/null | grep -q com.termux; then
  echo "  Termux: already installed"
else
  APK="${TMPDIR:-/tmp}/termux-$TERMUX_VERSION.apk"
  case "$ABI" in
    x86_64) VARIANT=x86_64 ;;
    arm64-v8a) VARIANT=arm64-v8a ;;
    *) VARIANT=universal ;;
  esac
  URL="https://github.com/termux/termux-app/releases/download/v$TERMUX_VERSION/termux-app_v${TERMUX_VERSION}%2Bgithub-debug_${VARIANT}.apk"
  echo "  Termux: downloading ($VARIANT)…"
  curl -sL -o "$APK" "$URL" || { echo "download failed" >&2; exit 1; }
  adb -s "$SERIAL" install -r "$APK" >/dev/null || { echo "installation failed" >&2; exit 1; }
  echo "  Termux: installed"
fi

# Android 12 and later kill "phantom" processes — those an application lets live in the
# background. A long SSH session is one of them; without this switch, it dies after a few
# minutes with no explanation.
adb -s "$SERIAL" shell settings put global settings_enable_monitor_phantom_procs false >/dev/null 2>&1

echo
echo "== Next =="
echo "  1. adb -s $SERIAL shell am start -n com.termux/.app.TermuxActivity"
echo "  2. in Termux: pkg install -y openssh"
echo "  3. from Termux: ssh <user>@10.0.2.2   (10.0.2.2 = the host machine)"
echo "  4. run \"po doctor\" there: transport must be Ssh, and the probe must succeed."
