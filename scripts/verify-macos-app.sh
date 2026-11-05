#!/bin/sh
# Check that a built tern.app is put together the way the desktop app expects,
# and that its signature is what the release mode says it should be.
#
#   scripts/verify-macos-app.sh <tern.app> [adhoc | developer-id | notarized]
#
#   adhoc         (default) structure, architectures, a valid ad-hoc signature.
#   developer-id  also: hardened runtime and a Developer ID authority on the
#                 app and the tray helper, a team id, no debug entitlement, and
#                 Gatekeeper's `spctl --assess` pointing at the signature.
#   notarized     also: a stapled ticket that `stapler validate` accepts, and
#                 `spctl` reporting "Notarized Developer ID".
#
# Runs every check, prints one line each, and exits non-zero if any failed.

# `cond && ok ... || fail ...` is fine here: ok never fails.
# shellcheck disable=SC2015
set -eu

[ $# -ge 1 ] && [ $# -le 2 ] || { echo "usage: $0 <tern.app> [adhoc|developer-id|notarized]" >&2; exit 2; }
app="${1%/}"
level="${2:-adhoc}"
case "$level" in adhoc | developer-id | notarized) ;; *) echo "unknown level: $level" >&2; exit 2 ;; esac

tray="$app/Contents/Library/LoginItems/tern Tray.app"
fails=0
ok() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1" >&2; fails=$((fails + 1)); }
check() { # check <description> <command...>
  desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then ok "$desc"; else fail "$desc"; fi
}

echo "verify: $app ($level)"

# Structure -------------------------------------------------------------
check "app Info.plist is valid" plutil -lint -s "$app/Contents/Info.plist"
[ "$(plutil -extract CFBundleIdentifier raw "$app/Contents/Info.plist" 2>/dev/null)" = dev.tern.ternDesktop ] \
  && ok "app bundle id is dev.tern.ternDesktop" || fail "app bundle id is dev.tern.ternDesktop"
exe="$app/Contents/MacOS/$(plutil -extract CFBundleExecutable raw "$app/Contents/Info.plist" 2>/dev/null || echo missing)"
check "app executable exists" test -x "$exe"
check "Flutter framework is embedded" test -d "$app/Contents/Frameworks/FlutterMacOS.framework"
check "tray helper Info.plist is valid" plutil -lint -s "$tray/Contents/Info.plist"
[ "$(plutil -extract LSUIElement raw "$tray/Contents/Info.plist" 2>/dev/null)" = true ] \
  && ok "tray helper is an agent app (LSUIElement)" || fail "tray helper is an agent app (LSUIElement)"
check "tray helper carries tern-tray (the bundled tern)" test -x "$tray/Contents/MacOS/tern-tray"
check "daemon plist for SMAppService is valid" \
  plutil -lint -s "$tray/Contents/Library/LaunchAgents/ai.tern.daemon.plist"
check "no stale Contents/Helpers copy of tern" test ! -e "$app/Contents/Helpers"

# The binary the app runs as `tern` must run as the CLI when given arguments.
if "$tray/Contents/MacOS/tern-tray" --version >/dev/null 2>&1; then
  ok "tern-tray --version runs ($("$tray/Contents/MacOS/tern-tray" --version 2>&1 | head -n1))"
else
  fail "tern-tray --version runs (wrong architecture for this Mac, or a broken binary)"
fi
printf '        archs: app %s, tern-tray %s\n' \
  "$(lipo -archs "$exe" 2>/dev/null || echo '?')" \
  "$(lipo -archs "$tray/Contents/MacOS/tern-tray" 2>/dev/null || echo '?')"

# Signature -------------------------------------------------------------
check "codesign --verify --deep --strict" codesign --verify --deep --strict "$app"
check "tray helper signature is valid" codesign --verify --deep --strict "$tray"

info_app="$(codesign -dvv "$app" 2>&1 || true)"
info_tray="$(codesign -dvv "$tray" 2>&1 || true)"
has() { printf '%s\n' "$1" | grep -q "$2"; }

if [ "$level" = adhoc ]; then
  has "$info_app" 'Signature=adhoc' && ok "app is ad-hoc signed" || fail "app is ad-hoc signed"
  if spctl --assess --type execute "$app" >/dev/null 2>&1; then
    printf '        spctl: accepted (unexpected for ad-hoc, but fine)\n'
  else
    printf '        spctl: rejected, as expected for an ad-hoc build (users allow it once)\n'
  fi
else
  for which in app tray; do
    if [ "$which" = app ]; then info="$info_app"; else info="$info_tray"; fi
    has "$info" 'flags=0x[0-9a-f]*(.*runtime' && ok "$which: hardened runtime" || fail "$which: hardened runtime"
    has "$info" 'Authority=Developer ID Application' && ok "$which: Developer ID Application authority" \
      || fail "$which: Developer ID Application authority"
    has "$info" 'TeamIdentifier=[A-Z0-9]\{10\}' && ok "$which: team identifier" || fail "$which: team identifier"
    has "$info" 'Timestamp=' && ok "$which: secure timestamp" || fail "$which: secure timestamp"
  done
  ents="$(codesign -d --entitlements - --xml "$app" 2>/dev/null || true)"
  printf '%s' "$ents" | grep -q 'get-task-allow' \
    && fail "app has no get-task-allow entitlement (it blocks notarization)" \
    || ok "app has no get-task-allow entitlement"
  spctl_out="$(spctl --assess --type execute -vv "$app" 2>&1 || true)"
  if [ "$level" = notarized ]; then
    check "stapled ticket validates" xcrun stapler validate "$app"
    printf '%s\n' "$spctl_out" | grep -q 'source=Notarized Developer ID' \
      && ok "spctl: Notarized Developer ID" || fail "spctl: Notarized Developer ID ($spctl_out)"
  else
    printf '%s\n' "$spctl_out" | grep -q 'accepted\|Unnotarized Developer ID' \
      && ok "spctl: Developer ID signature recognised" || fail "spctl: Developer ID signature ($spctl_out)"
  fi
fi

if [ "$fails" -gt 0 ]; then
  echo "verify: $fails check(s) failed" >&2
  exit 1
fi
echo "verify: all checks passed"
