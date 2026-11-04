#!/bin/sh
# Build tern.app with its own `tern` inside and pack it into a .dmg.
#
#   scripts/package-macos-app.sh <tern-binary> <out-dir>
#
# The binary goes into the menu-bar helper, once:
#
#   tern.app/Contents/Library/LoginItems/tern Tray.app     (LSUIElement)
#     Contents/MacOS/tern-tray                             this binary; with no
#                                                          arguments it's `tern tray`
#     Contents/Library/LaunchAgents/ai.tern.daemon.plist   the daemon, for SMAppService
#
# The app runs that binary as its `tern` (before anything on PATH; see
# apps/desktop/lib/state/daemon.dart), and `tern service install --tray` run
# from it registers both login items with SMAppService
# (docs/personal-agent-tooling-spec.md §11.5). Pass a universal binary
# (lipo) for a universal app. Writes <out-dir>/tern-desktop-macos.dmg.
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

tray="$app/Contents/Library/LoginItems/tern Tray.app"
version="$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")"
rm -rf "$app/Contents/Helpers" "$tray"
mkdir -p "$tray/Contents/MacOS" "$tray/Contents/Library/LaunchAgents"
install -m 755 "$bin" "$tray/Contents/MacOS/tern-tray"
"$bin" service bundle-plist > "$tray/Contents/Library/LaunchAgents/ai.tern.daemon.plist"
cat > "$tray/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>dev.tern.ternDesktop.tray</string>
	<key>CFBundleName</key>
	<string>tern Tray</string>
	<key>CFBundleDisplayName</key>
	<string>tern</string>
	<key>CFBundleExecutable</key>
	<string>tern-tray</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
PLIST
plutil -lint -s "$tray/Contents/Info.plist" "$tray/Contents/Library/LaunchAgents/ai.tern.daemon.plist"

# Adding files breaks the bundle's seal, so sign again, inside out: the
# tray bundle, then the app (an unsealed app on Apple Silicon reports as
# "damaged").
if [ "$identity" = - ]; then
  codesign --force --sign - "$tray"
  codesign --force --deep --sign - "$app"
else
  codesign --force --options runtime --timestamp --sign "$identity" "$tray"
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
