#!/usr/bin/env bash
# Builds a release Spacee.app and packs it into a signed .dmg with an Applications
# shortcut to drag it onto. The image isn't notarized, so Gatekeeper only accepts it on
# machines that trust the signing identity (see "Signing" in README.md).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IDENTITY="${SPACEE_SIGN_IDENTITY:--}"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$ROOT/crates/app/Info.plist")"
DMG="$ROOT/target/release/Spacee-$VERSION.dmg"

PROFILE=release "$ROOT/scripts/run.sh" --no-open

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$ROOT/target/release/Spacee.app" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -quiet -volname Spacee -srcfolder "$STAGE" -format UDZO -fs HFS+ "$DMG"
codesign --force --timestamp=none --sign "$IDENTITY" "$DMG"
echo "$DMG"
