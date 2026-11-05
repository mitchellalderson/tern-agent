#!/bin/sh
# Add tern to the application menu from an unpacked tern-desktop-linux tarball,
# for the current user only (no root):
#
#   ./install-local.sh              copy the desktop entry and icons into
#                                   ${XDG_DATA_HOME:-~/.local/share}; the entry
#                                   launches this folder's tern_desktop
#   ./install-local.sh --uninstall  remove them again
#
# Moving the folder afterwards means running it again. `tern` (the daemon and
# CLI, next to tern_desktop) is not put on PATH; link it yourself if you want
# it there:  ln -s "$PWD/tern" ~/.local/bin/tern
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
entry="$data/applications/dev.tern.tern_desktop.desktop"

if [ "${1:-}" = --uninstall ]; then
  rm -f "$entry"
  for size in 16 32 64 128 256 512; do rm -f "$data/icons/hicolor/${size}x${size}/apps/tern.png"; done
  rm -f "$data/icons/hicolor/scalable/apps/tern.svg"
  echo "removed the tern menu entry and icons"
  exit 0
fi

[ -x "$here/tern_desktop" ] || { echo "run this from the unpacked tern-desktop folder (no tern_desktop next to it)" >&2; exit 1; }
[ -f "$here/share/applications/dev.tern.tern_desktop.desktop" ] || { echo "share/ is missing from $here" >&2; exit 1; }

# An Exec path needs quoting rules that differ from the shell's. Plain paths
# need none, so only those are accepted.
case "$here" in
  *[!A-Za-z0-9._/+-]*)
    echo "the folder path has characters a desktop entry can't hold safely: $here" >&2
    echo "move it somewhere with a plain path (letters, digits and . _ / + -) and run this again" >&2
    exit 1 ;;
esac

mkdir -p "$data/applications" "$data/icons/hicolor"
sed "s|^Exec=.*|Exec=$here/tern_desktop|" "$here/share/applications/dev.tern.tern_desktop.desktop" > "$entry"
cp -R "$here/share/icons/hicolor/." "$data/icons/hicolor/"
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$data/applications" >/dev/null 2>&1 || true; fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -t "$data/icons/hicolor" >/dev/null 2>&1 || true; fi
echo "installed: $entry"
