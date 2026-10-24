#!/usr/bin/env bash
# Builds Spacee.app and signs it with a stable identity. macOS ties the Full
# Disk Access grant to that identity; an ad-hoc signature would lose it on
# every rebuild.
set -euo pipefail

PROFILE="${PROFILE:-debug}"
IDENTITY="${SPACEE_SIGN_IDENTITY:--}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/$PROFILE/Spacee.app"

if [[ "$PROFILE" == release ]]; then
  cargo build --manifest-path "$ROOT/Cargo.toml" -p spacee --release
else
  cargo build --manifest-path "$ROOT/Cargo.toml" -p spacee
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/$PROFILE/spacee" "$APP/Contents/MacOS/spacee"
cp "$ROOT/crates/app/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/crates/app/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

codesign --force --options runtime --timestamp=none --sign "$IDENTITY" "$APP"

if [[ "${1:-}" != --no-open ]]; then
  open "$APP"
fi
