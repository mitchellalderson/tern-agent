#!/bin/sh
# Pack the Linux desktop app, with its own `tern` beside it, into a .tar.gz
# (a relocatable folder) and a .deb.
#
#   scripts/package-linux-app.sh [--arch x86_64|aarch64] [--bundle <dir>] <tern-binary> <out-dir>
#
# Without --bundle it runs `flutter build linux --release` (Linux only) and
# packs build/linux/<x64|arm64>/release/bundle. --bundle packs a bundle that
# was built elsewhere; it only has to look like one (tern_desktop, lib/,
# data/), which is how the packaging is tested on a Mac.
#
# Output, in <out-dir> (unversioned names, so releases/latest/download/ works):
#   tern-desktop-linux-<arch>.tar.gz   unpack anywhere; run ./tern_desktop
#   tern-desktop-linux-<arch>.deb      Debian, Ubuntu and friends
#   each with a .sha256
#
# <arch> is the Rust spelling (x86_64, aarch64); the .deb itself says
# amd64 or arm64. Why these two formats, what the system needs, and what's
# untested: docs/desktop-release.md.
#
# The layout, in both:
#   tern_desktop            the Flutter app
#   tern                    the daemon and CLI; the app finds it right here
#                           (apps/desktop/lib/state/daemon.dart), and so does
#                           the tray's "Open tern" (it looks for tern_desktop
#                           beside tern)
#   lib/, data/             Flutter's engine and assets; the executable's
#                           RPATH is $ORIGIN/lib, so the folder can move
#   share/                  desktop entry and icons (hicolor)
# The .deb puts that folder in /usr/lib/tern-desktop and links tern_desktop
# and tern into /usr/bin.
set -eu

arch=
bundle=
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) arch="${2:?--arch needs a value}"; shift ;;
    --bundle) bundle="${2:?--bundle needs a directory}"; shift ;;
    --) shift; break ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) break ;;
  esac
  shift
done
[ $# -eq 2 ] || { echo "usage: $0 [--arch x86_64|aarch64] [--bundle <dir>] <tern-binary> <out-dir>" >&2; exit 2; }

if [ -z "$arch" ]; then
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) echo "can't tell the architecture from $(uname -m); pass --arch" >&2; exit 2 ;;
  esac
fi
case "$arch" in
  x86_64) deb_arch=amd64; flutter_arch=x64 ;;
  aarch64) deb_arch=arm64; flutter_arch=arm64 ;;
  *) echo "unknown --arch: $arch (x86_64 or aarch64)" >&2; exit 2 ;;
esac

bin="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[ -x "$bin" ] || { echo "not an executable file: $1" >&2; exit 1; }
mkdir -p "$2"
out="$(cd "$2" && pwd)"
root="$(cd "$(dirname "$0")/.." && pwd)"
desktop="$root/apps/desktop"
packaging="$desktop/linux/packaging"
name="tern-desktop-linux-$arch"

step() { printf '\n==> %s\n' "$*"; }

if [ -z "$bundle" ]; then
  [ "$(uname -s)" = Linux ] || { echo "flutter build linux needs Linux; pass --bundle <dir> to pack an existing bundle" >&2; exit 1; }
  step "flutter build linux --release"
  (cd "$desktop" && flutter build linux --release)
  bundle="$desktop/build/linux/$flutter_arch/release/bundle"
fi
[ -x "$bundle/tern_desktop" ] || { echo "no tern_desktop in $bundle" >&2; exit 1; }
[ -d "$bundle/lib" ] && [ -d "$bundle/data" ] || { echo "$bundle has no lib/ and data/ (not a Flutter bundle)" >&2; exit 1; }

version="$(sed -n 's/^version: \([^+]*\).*/\1/p' "$desktop/pubspec.yaml")"
[ -n "$version" ] || { echo "no version in apps/desktop/pubspec.yaml" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

# --- the folder ---------------------------------------------------------------
step "assemble tern-desktop/ ($version, $arch)"
app="$work/tern-desktop"
mkdir -p "$app"
cp -R "$bundle/." "$app/"
install -m 755 "$bin" "$app/tern"
mkdir -p "$app/share/applications"
cp "$packaging/dev.tern.tern_desktop.desktop" "$app/share/applications/"
icons="$desktop/macos/Runner/Assets.xcassets/AppIcon.appiconset"
for size in 16 32 64 128 256 512; do
  dir="$app/share/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dir"
  cp "$icons/app_icon_$size.png" "$dir/tern.png"
done
mkdir -p "$app/share/icons/hicolor/scalable/apps"
cp "$desktop/assets/icon/tern-icon.svg" "$app/share/icons/hicolor/scalable/apps/tern.svg"
install -m 755 "$packaging/install-local.sh" "$app/install-local.sh"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$app/"
# Readable by everyone, writable by the owner; directories traversable.
find "$app" -type d -exec chmod 755 {} +
find "$app" -type f ! -perm -u+x -exec chmod 644 {} +

# --- tar.gz -------------------------------------------------------------------
step "$name.tar.gz"
if tar --version 2>&1 | grep -q bsdtar; then
  owner="--uid 0 --gid 0 --uname root --gname root"
else
  owner="--owner=0 --group=0 --numeric-owner"
fi
rm -f "$out/$name.tar.gz"
# shellcheck disable=SC2086 # $owner is a fixed list of words
COPYFILE_DISABLE=1 tar $owner -czf "$out/$name.tar.gz" -C "$work" tern-desktop

# --- deb ----------------------------------------------------------------------
step "$name.deb"
command -v dpkg-deb >/dev/null 2>&1 || { echo "dpkg-deb not found (apt install dpkg-dev; brew install dpkg)" >&2; exit 1; }
pkg="$work/deb"
mkdir -p "$pkg/DEBIAN" "$pkg/usr/lib" "$pkg/usr/bin" "$pkg/usr/share"
cp -R "$app" "$pkg/usr/lib/tern-desktop"
# Desktop entry and icons go where the desktop looks for them; the copies
# inside the folder are for the tarball.
cp -R "$app/share/applications" "$app/share/icons" "$pkg/usr/share/"
rm -rf "$pkg/usr/lib/tern-desktop/share" "$pkg/usr/lib/tern-desktop/install-local.sh"
mkdir -p "$pkg/usr/share/doc/tern-desktop"
mv "$pkg/usr/lib/tern-desktop/LICENSE-MIT" "$pkg/usr/lib/tern-desktop/LICENSE-APACHE" "$pkg/usr/share/doc/tern-desktop/"
ln -s ../lib/tern-desktop/tern_desktop "$pkg/usr/bin/tern_desktop"
ln -s ../lib/tern-desktop/tern "$pkg/usr/bin/tern"
size_kb="$(du -sk "$pkg/usr" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: tern-desktop
Version: $version
Architecture: $deb_arch
Maintainer: Mitchell Alderson <mitchell@inngest.com>
Installed-Size: $size_kb
Section: utils
Priority: optional
Depends: libc6, libgtk-3-0t64 | libgtk-3-0, libglib2.0-0t64 | libglib2.0-0
Recommends: zenity | kdialog
Suggests: gnome-shell-extension-appindicator
Homepage: https://github.com/mitchellalderson/tern-agent
Description: Desktop app for the tern personal AI agent
 Conversations, approvals, routines and a run inspector for tern. Carries its
 own tern daemon (tern, linked into /usr/bin), so Start tern works without
 installing the CLI separately. The tray icon needs a StatusNotifier host:
 KDE and most panels have one; GNOME needs the AppIndicator extension.
CONTROL
cat > "$pkg/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
  if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q /usr/share/applications || true; fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true; fi
fi
exit 0
POSTINST
cat > "$pkg/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
if [ "$1" = remove ] || [ "$1" = purge ]; then
  if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q /usr/share/applications || true; fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true; fi
fi
exit 0
POSTRM
chmod 755 "$pkg/DEBIAN/postinst" "$pkg/DEBIAN/postrm"
find "$pkg" -type d -exec chmod 755 {} +
rm -f "$out/$name.deb"
dpkg-deb --root-owner-group --build "$pkg" "$out/$name.deb" >/dev/null

# --- checks and checksums -----------------------------------------------------
step "check the packages"
fails=0
need() { # need <listing> <path>
  if printf '%s\n' "$1" | grep -q "$2"; then printf '  ok    %s\n' "$2"; else printf '  FAIL  missing %s\n' "$2" >&2; fails=$((fails + 1)); fi
}
tar_list="$(tar -tzf "$out/$name.tar.gz")"
echo "tar.gz:"
for p in 'tern-desktop/tern_desktop$' 'tern-desktop/tern$' 'tern-desktop/lib/' 'tern-desktop/data/' \
  'tern-desktop/share/applications/dev.tern.tern_desktop.desktop$' \
  'tern-desktop/share/icons/hicolor/256x256/apps/tern.png$' 'tern-desktop/install-local.sh$'; do
  need "$tar_list" "$p"
done
deb_list="$(dpkg-deb --contents "$out/$name.deb")"
echo "deb:"
for p in './usr/lib/tern-desktop/tern_desktop$' './usr/lib/tern-desktop/tern$' './usr/lib/tern-desktop/lib/' \
  './usr/bin/tern_desktop -> ../lib/tern-desktop/tern_desktop$' './usr/bin/tern -> ../lib/tern-desktop/tern$' \
  './usr/share/applications/dev.tern.tern_desktop.desktop$' './usr/share/icons/hicolor/256x256/apps/tern.png$'; do
  need "$deb_list" "$p"
done
dpkg-deb --info "$out/$name.deb" | sed -n '/^ Package:/,/^ Installed-Size:/p'
if command -v desktop-file-validate >/dev/null 2>&1; then
  if desktop-file-validate "$packaging/dev.tern.tern_desktop.desktop"; then echo "  ok    desktop-file-validate"; else fails=$((fails + 1)); fi
fi
if [ "$(uname -s)" = Linux ] && command -v ldd >/dev/null 2>&1; then
  # Only meaningful for a real bundle on a Linux host with GTK installed.
  if ldd "$app/tern_desktop" 2>/dev/null | grep -q 'not found'; then
    echo "  FAIL  tern_desktop has unresolved libraries:" >&2
    ldd "$app/tern_desktop" | grep 'not found' >&2
    fails=$((fails + 1))
  else
    echo "  ok    ldd tern_desktop: all libraries resolve"
  fi
fi
[ "$fails" -eq 0 ] || { echo "$fails package check(s) failed" >&2; exit 1; }

(cd "$out" && for f in "$name.tar.gz" "$name.deb"; do
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$f"; else shasum -a 256 "$f"; fi > "$f.sha256"
done)
step "done"
ls -l "$out/$name".tar.gz "$out/$name".deb
cat "$out/$name.tar.gz.sha256" "$out/$name.deb.sha256"
