#!/bin/bash
# loca uninstaller for macOS, installed by the unified .pkg at
# <prefix>/uninstall.sh. The double-underscore placeholders below are
# substituted at build time by scripts/release/unified-installers.ts.
#
# Run with sudo. Flags:
#   --keep-data     keep ~/.loca/<channel>/core (settings, sessions, memories)
#   --purge-models  also remove the shared ~/.loca/models downloads
#   --keep-client   leave the Client app in /Applications
set -euo pipefail

CHANNEL="__CHANNEL__"
PREFIX="__PREFIX__"
APP_NAME="__APP_NAME__"
BIN_NAME="__BIN_NAME__"
PKG_IDS="__PKG_IDS__"
SERVICE_LABEL="__SERVICE_LABEL__"

KEEP_DATA=0
PURGE_MODELS=0
KEEP_CLIENT=0
for arg in "$@"; do
  case "$arg" in
    --keep-data) KEEP_DATA=1 ;;
    --purge-models) PURGE_MODELS=1 ;;
    --keep-client) KEEP_CLIENT=1 ;;
    *)
      echo "unknown flag: $arg" >&2
      echo "usage: sudo $0 [--keep-data] [--purge-models] [--keep-client]" >&2
      exit 2
      ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "this uninstaller needs root; re-run it with sudo" >&2
  exit 1
fi

# The install is per-user state owned by the console user (mirrors the pkg
# postinstall); resolve them the same way so the teardown targets the right
# home. Headless over SSH has no console session (/dev/console stats as root),
# so fall back to whoever ran sudo.
CONSOLE_USER="$(stat -f%Su /dev/console)"
if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "root" ]; then
  CONSOLE_USER="${SUDO_USER:-}"
fi
if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "root" ]; then
  echo "no console user; cannot resolve the install to remove" >&2
  exit 1
fi
# sed, not awk '{print $2}': a home dir containing spaces must survive whole.
USER_HOME="$(dscl . -read "/Users/$CONSOLE_USER" NFSHomeDirectory | sed 's/^[^ ]* //')"

# Quit the Client so nothing holds the Core alive or restarts it mid-teardown.
# pkill matches an ERE, so the app name (channel builds carry parentheses:
# "Loca (latest).app") must be escaped to match literally.
if [ "$KEEP_CLIENT" -eq 0 ]; then
  APP_PATTERN="$(printf '%s' "/Applications/$APP_NAME/Contents/MacOS/" | sed 's/[][\.*^$()+?{}|]/\\&/g')"
  pkill -f "$APP_PATTERN" 2>/dev/null || true
fi

# The Core binary owns the real teardown (launchd agent, keychain master key,
# ~/.loca/<channel>/core); run it as the console user, exactly like install.
BIN="$USER_HOME/.loca/$CHANNEL/core/bin/$BIN_NAME"
if [ -x "$BIN" ]; then
  ARGS=(uninstall-service)
  if [ "$KEEP_DATA" -eq 1 ]; then ARGS+=(--keep-data); fi
  if [ "$PURGE_MODELS" -eq 1 ]; then ARGS+=(--purge-models); fi
  sudo -u "$CONSOLE_USER" env HOME="$USER_HOME" LOCA_CHANNEL="$CHANNEL" "$BIN" "${ARGS[@]}" || true
else
  echo "core binary not found at $BIN; removing what is left on disk" >&2
  # Without the binary, uninstall-service can't unload the launchd agent; take
  # it down directly so a KeepAlive agent doesn't respawn a missing binary.
  USER_UID="$(id -u "$CONSOLE_USER")"
  launchctl bootout "gui/$USER_UID/$SERVICE_LABEL" 2>/dev/null || true
  rm -f "$USER_HOME/Library/LaunchAgents/$SERVICE_LABEL.plist"
  pkill -f "/.loca/$CHANNEL/core/bin/$BIN_NAME" 2>/dev/null || true
  if [ "$KEEP_DATA" -eq 0 ]; then
    rm -rf "$USER_HOME/.loca/$CHANNEL/core"
  fi
  if [ "$PURGE_MODELS" -eq 1 ]; then
    rm -rf "$USER_HOME/.loca/models"
  fi
fi

if [ "$KEEP_CLIENT" -eq 0 ]; then
  rm -rf "/Applications/$APP_NAME"
fi

# Remove the system-prefix payload copy (this script included) and drop the
# installer receipts so a later install starts clean.
rm -rf "$PREFIX"
for id in $PKG_IDS; do
  pkgutil --forget "$id" >/dev/null 2>&1 || true
done

echo "loca is uninstalled."
if [ "$KEEP_DATA" -eq 1 ]; then
  echo "kept: $USER_HOME/.loca/$CHANNEL/core (--keep-data)"
fi
if [ "$PURGE_MODELS" -eq 0 ] && [ -d "$USER_HOME/.loca/models" ]; then
  echo "kept: $USER_HOME/.loca/models (shared model downloads; --purge-models removes them)"
fi
