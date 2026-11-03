#!/bin/sh
# Build tern.app with its own `tern` inside and pack it into a .dmg.
#
#   scripts/package-macos-app.sh <tern-binary> <out-dir>
#
# The binary goes to tern.app/Contents/Helpers/tern, which the app's
# "Start tern" uses before anything on PATH (apps/desktop/lib/state/daemon.dart).
# Pass a universal binary (lipo) for a universal app. Writes
# <out-dir>/tern-desktop-macos.dmg.
#
# Signing, all optional (without them the app is ad-hoc signed and users
# have to allow it once in System Settings > Privacy & Security):
#   MACOS_SIGN_IDENTITY   "Developer ID Application: …" (in the keychain)
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   notarize and staple the dmg
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <tern-binary> <out-dir>" >&2; exit 2; }
bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
mkdir -p "$2"
out="$(cd "$2" && pwd)"
root="$(cd "$(dirname "$0")/.." && pwd)"
identity="${MACOS_SIGN_IDENTITY:--}"

cd "$root/apps/desktop"
flutter build macos --release
app="$root/apps/desktop/build/macos/Build/Products/Release/tern.app"

mkdir -p "$app/Contents/Helpers"
install -m 755 "$bin" "$app/Contents/Helpers/tern"

# Adding a file breaks the bundle's seal, so sign again: the helper first,
# then the app (an unsealed app on Apple Silicon reports as "damaged").
if [ "$identity" = - ]; then
  codesign --force --sign - "$app/Contents/Helpers/tern"
  codesign --force --deep --sign - "$app"
else
  codesign --force --options runtime --timestamp --sign "$identity" "$app/Contents/Helpers/tern"
  codesign --force --deep --options runtime --timestamp \
    --entitlements macos/Runner/Release.entitlements --sign "$identity" "$app"
fi
codesign --verify --deep --strict "$app"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT INT TERM
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/tern-desktop-macos.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname tern -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg"

if [ "$identity" != - ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  if [ -n "${APPLE_ID:-}" ]; then
    xcrun notarytool submit "$dmg" --wait \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD"
    xcrun stapler staple "$dmg"
  fi
fi
echo "$dmg"
