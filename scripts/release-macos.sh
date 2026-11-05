#!/bin/sh
# The macOS desktop release pipeline: build, bundle tern, sign, notarize,
# staple, and pack a .dmg and a .zip with checksums.
#
#   scripts/release-macos.sh [--dry-run] [--skip-flutter-build] <tern-binary> <out-dir>
#
# Output, in <out-dir>:
#   tern-desktop-macos.dmg   (+ .sha256)   for people: drag tern to Applications
#   tern-desktop-macos.zip   (+ .sha256)   for auto-updaters (Sparkle and the like)
#
# Modes, from what's in the environment:
#   unsigned   no MACOS_SIGN_IDENTITY (or --dry-run): ad-hoc signature, nothing
#              leaves this Mac. Everything else still happens, so the assembly,
#              the structure checks and the packaging are tested without Apple
#              credentials. Users have to allow the app once.
#   signed     MACOS_SIGN_IDENTITY set: Developer ID signature, hardened
#              runtime, secure timestamp (needs network).
#   notarized  signed, and notary credentials set: the app and the dmg go to
#              Apple's notary service, and the tickets are stapled.
#   --dry-run  is unsigned even if credentials are set, and prints the
#              notarytool and stapler commands it would have run.
#
# The one-time setup, and every variable, is in docs/desktop-release.md.
# Credentials only ever come from the environment or the keychain:
#
#   MACOS_SIGN_IDENTITY       "Developer ID Application: Name (TEAMID)"
#   one of, for notarizing:
#     NOTARY_KEYCHAIN_PROFILE            a `notarytool store-credentials` profile
#                                        (NOTARY_KEYCHAIN: its keychain, optional)
#     APPLE_API_KEY_PATH, APPLE_API_KEY_ID, APPLE_API_ISSUER
#                                        an App Store Connect API key (.p8)
#     APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD
#                                        an Apple ID and app-specific password
#   REQUIRE_NOTARIZE=1        fail instead of falling back to a signed-only
#                             build when notary credentials are missing
#
# The binary goes into the menu-bar helper, once (the app finds it there, see
# apps/desktop/lib/state/daemon.dart):
#
#   tern.app/Contents/Library/LoginItems/tern Tray.app     (LSUIElement)
#     Contents/MacOS/tern-tray                             this binary; with no
#                                                          arguments it's `tern tray`
#     Contents/Library/LaunchAgents/ai.tern.daemon.plist   the daemon, for SMAppService
#
# Pass a universal binary (lipo) for a universal app.
set -eu

dry=0
skip_build=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry=1 ;;
    --skip-flutter-build) skip_build=1 ;;
    --) shift; break ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) break ;;
  esac
  shift
done
[ $# -eq 2 ] || { echo "usage: $0 [--dry-run] [--skip-flutter-build] <tern-binary> <out-dir>" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo "release-macos.sh needs macOS" >&2; exit 1; }

bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[ -x "$bin" ] || { echo "not an executable file: $1" >&2; exit 1; }
mkdir -p "$2"
out="$(cd "$2" && pwd)"
root="$(cd "$(dirname "$0")/.." && pwd)"
desktop="$root/apps/desktop"
# The app compares the core's version with this one (Settings > About, version banner).
workspace_version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")"
[ -n "$workspace_version" ] || { echo "no workspace version in Cargo.toml" >&2; exit 1; }

# Apple's version fields are stricter than semver: CFBundleShortVersionString
# is up to three dot-separated integers and CFBundleVersion is integers and
# dots, so neither may carry "-alpha.1". The marketing version is the core
# X.Y.Z, and the build number comes from the pre-release counter so builds
# still sort, and a final release sorts after its pre-releases:
#   0.1.0-alpha.N -> 0.1.0 (N)    0.1.0-beta.N -> (100+N)    0.1.0-rc.N -> (200+N)
#   0.1.0         -> 0.1.0 (1000)
# (documented in docs/desktop-release.md). TERN_APP_VERSION keeps the full string.
apple_version="${workspace_version%%-*}"
case "$workspace_version" in
  *-alpha.[0-9]*) apple_build="${workspace_version##*alpha.}" ;;
  *-beta.[0-9]*) apple_build=$((100 + ${workspace_version##*beta.})) ;;
  *-rc.[0-9]*) apple_build=$((200 + ${workspace_version##*rc.})) ;;
  *-*) echo "unsupported pre-release in Cargo.toml: $workspace_version (use alpha.N, beta.N or rc.N)" >&2; exit 1 ;;
  *) apple_build=1000 ;;
esac
case "$apple_build" in '' | *[!0-9]*) echo "bad pre-release number in $workspace_version" >&2; exit 1 ;; esac
app="$desktop/build/macos/Build/Products/Release/tern.app"
name=tern-desktop-macos

identity="${MACOS_SIGN_IDENTITY:-}"
if [ "$dry" = 1 ]; then identity=; fi

# Which way to authenticate to the notary service, if any.
notary=
if [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]; then
  notary=profile
elif [ -n "${APPLE_API_KEY_PATH:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ]; then
  notary=apikey
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  notary=appleid
fi

if [ -n "$identity" ]; then
  if [ -n "$notary" ]; then
    mode=notarized
  elif [ "${REQUIRE_NOTARIZE:-0}" = 1 ]; then
    echo "REQUIRE_NOTARIZE=1 but no notary credentials are set (see docs/desktop-release.md)" >&2
    exit 1
  else
    mode=signed
    echo "warning: no notary credentials; the build will be signed but not notarized" >&2
  fi
else
  mode=unsigned
  if [ "$dry" = 0 ] && [ "${REQUIRE_NOTARIZE:-0}" = 1 ]; then
    echo "REQUIRE_NOTARIZE=1 but MACOS_SIGN_IDENTITY isn't set" >&2
    exit 1
  fi
fi
echo "release-macos: mode=$mode${notary:+ (notary: $notary)}$([ "$dry" = 1 ] && echo ' [dry-run]')"

step() { printf '\n==> %s\n' "$*"; }

# Run a command that talks to Apple, or say what it would run.
apple() {
  if [ "$mode" = notarized ]; then
    "$@"
  else
    printf '    (not run: %s) %s\n' "$mode" "$*"
  fi
}

# Submit <file> to the notary service and wait. notarytool can exit 0 for an
# Invalid submission, so read the status out of its plist output.
notarize() {
  file="$1"
  if [ "$mode" != notarized ]; then
    case "$notary" in
      profile) how="--keychain-profile <profile>" ;;
      apikey) how="--key <p8> --key-id <id> --issuer <issuer>" ;;
      appleid) how="--apple-id <id> --team-id <team> --password <redacted>" ;;
      *) how="<credentials>" ;;
    esac
    printf '    (not run: %s) xcrun notarytool submit %s --wait %s\n' "$mode" "$file" "$how"
    return 0
  fi
  case "$notary" in
    profile)
      set -- --keychain-profile "$NOTARY_KEYCHAIN_PROFILE"
      if [ -n "${NOTARY_KEYCHAIN:-}" ]; then set -- "$@" --keychain "$NOTARY_KEYCHAIN"; fi ;;
    apikey) set -- --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" ;;
    appleid) set -- --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" ;;
  esac
  result="$(xcrun notarytool submit "$file" --wait --output-format plist "$@")" || {
    printf '%s\n' "$result" >&2
    echo "notarytool failed for $file" >&2
    return 1
  }
  status="$(printf '%s' "$result" | plutil -extract status raw -o - -)"
  id="$(printf '%s' "$result" | plutil -extract id raw -o - -)"
  echo "    submission $id: $status"
  if [ "$status" != Accepted ]; then
    xcrun notarytool log "$id" "$@" >&2 || true
    echo "notarization of $file was not accepted (log above)" >&2
    return 1
  fi
}

# --- 1. build ---------------------------------------------------------------
if [ "$skip_build" = 1 ]; then
  step "using the existing build (--skip-flutter-build)"
  [ -d "$app" ] || { echo "no $app; build it first" >&2; exit 1; }
else
  step "flutter build macos --release"
  (cd "$desktop" && flutter build macos --release --build-name="$apple_version" --build-number="$apple_build" --dart-define=TERN_APP_VERSION="$workspace_version")
fi
[ -d "$app" ] || { echo "Flutter didn't produce $app" >&2; exit 1; }

# --- 2. bundle tern ---------------------------------------------------------
step "bundle tern into tern Tray.app"
tray="$app/Contents/Library/LoginItems/tern Tray.app"
version="$apple_version"
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
	<string>$apple_build</string>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
PLIST
plutil -lint -s "$tray/Contents/Info.plist" "$tray/Contents/Library/LaunchAgents/ai.tern.daemon.plist"

# --- 3. sign ----------------------------------------------------------------
# Inside out, never --deep: Apple's notary service rejects binaries that were
# signed without the hardened runtime or a secure timestamp, and --deep
# applies one set of options to everything. Adding the tray bundle broke the
# app's seal, so the app is always signed again (an unsealed app reports as
# "damaged" on Apple Silicon).
entitlements="$desktop/macos/Runner/Release.entitlements"
sign() { # sign <path> [entitlements]
  if [ -z "$identity" ]; then
    codesign --force --sign - "$1"
  elif [ $# -eq 2 ]; then
    codesign --force --options runtime --timestamp --entitlements "$2" --sign "$identity" "$1"
  else
    codesign --force --options runtime --timestamp --sign "$identity" "$1"
  fi
}

step "sign ($([ -n "$identity" ] && echo "$identity" || echo 'ad-hoc'))"
if [ -n "$identity" ]; then
  security find-identity -v -p codesigning | grep -F "$identity" >/dev/null \
    || { echo "signing identity not found in the keychain: $identity" >&2; exit 1; }
fi
# Nested code, deepest first: frameworks, dylibs, helper apps, XPC services.
nested="$(mktemp)"
trap 'rm -f "$nested"' EXIT INT TERM
find "$app/Contents" \( -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' \) -o -type f -name '*.dylib' \) -print |
  awk '{ print gsub("/", "/") " " $0 }' | sort -rn | cut -d' ' -f2- > "$nested"
while IFS= read -r path; do
  echo "    $path"
  sign "$path"
done < "$nested"
echo "    $app"
sign "$app" "$entitlements"

# --- 4. check ---------------------------------------------------------------
case "$mode" in
  unsigned) level=adhoc ;;
  signed) level=developer-id ;;
  notarized) level=developer-id ;;
esac
step "verify the signed app ($level)"
"$root/scripts/verify-macos-app.sh" "$app" "$level"

# --- 5. notarize and staple the app -----------------------------------------
work="$(mktemp -d)"
trap 'rm -rf "$work"; rm -f "$nested"' EXIT INT TERM
step "notarize the app"
ditto -c -k --keepParent "$app" "$work/notarize.zip"
notarize "$work/notarize.zip"
step "staple the app"
apple xcrun stapler staple "$app"
if [ "$mode" = notarized ]; then
  "$root/scripts/verify-macos-app.sh" "$app" notarized
fi

# --- 6. zip (for updaters) --------------------------------------------------
step "$name.zip"
rm -f "$out/$name.zip"
ditto -c -k --sequesterRsrc --keepParent "$app" "$out/$name.zip"

# --- 7. dmg -----------------------------------------------------------------
step "$name.dmg"
stage="$work/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$out/$name.dmg"
hdiutil create -quiet -volname tern -srcfolder "$stage" -fs HFS+ -format UDZO "$out/$name.dmg"
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$out/$name.dmg"
fi
notarize "$out/$name.dmg"
apple xcrun stapler staple "$out/$name.dmg"

# --- 8. check the artifacts, checksums --------------------------------------
step "check the artifacts"
hdiutil verify -quiet "$out/$name.dmg" && echo "    dmg: hdiutil verify ok"
mnt="$work/mnt"
mkdir -p "$mnt"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$out/$name.dmg"
detach() { hdiutil detach -quiet "$mnt" || hdiutil detach -quiet -force "$mnt"; }
if ! "$root/scripts/verify-macos-app.sh" "$mnt/tern.app" "$([ "$mode" = notarized ] && echo notarized || echo "$level")"; then
  detach
  exit 1
fi
detach
mkdir -p "$work/unzip"
ditto -x -k "$out/$name.zip" "$work/unzip"
codesign --verify --deep --strict "$work/unzip/tern.app" && echo "    zip: unpacked app verifies"
if [ "$mode" = notarized ]; then
  xcrun stapler validate "$out/$name.dmg"
  spctl --assess --type open --context context:primary-signature -vv "$out/$name.dmg"
fi
(cd "$out" && shasum -a 256 "$name.dmg" > "$name.dmg.sha256" && shasum -a 256 "$name.zip" > "$name.zip.sha256")

step "done ($mode)"
ls -l "$out/$name".dmg "$out/$name".zip "$out/$name".dmg.sha256 "$out/$name".zip.sha256
cat "$out/$name.dmg.sha256" "$out/$name.zip.sha256"
