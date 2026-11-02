#!/usr/bin/env bash
# Builds Spacee.app and signs it with the identity in SPACEE_SIGN_IDENTITY, or ad
# hoc without one. macOS ties the Full Disk Access grant to that identity; an
# ad-hoc signature loses it on every rebuild.
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
# Compiles the Icon Composer icon into Assets.car, which has the light and dark icons
# for macOS 26, and AppIcon.icns for older systems.
xcrun actool "$ROOT/crates/app/AppIcon.icon" --compile "$APP/Contents/Resources" \
  --app-icon AppIcon --platform macosx --minimum-deployment-target 13.0 \
  --target-device mac --output-partial-info-plist /dev/null >/dev/null

codesign --force --options runtime --timestamp=none --sign "$IDENTITY" "$APP"

if [[ "${1:-}" != --no-open ]]; then
  open "$APP"
fi
