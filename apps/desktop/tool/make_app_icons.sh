#!/bin/sh
# Regenerate the macOS app icon PNGs from assets/icon/tern-icon.svg.
#
#   tool/make_app_icons.sh
#
# The circle is inset to roughly Apple's icon grid (about 84% of the canvas)
# so it sits at the same size as other Dock icons. Rendering goes through
# AppKit (tool/render_svg.swift); qlmanage would flatten the transparent
# corners to white.
set -eu

cd "$(dirname "$0")/.."
svg=assets/icon/tern-icon.svg
out=macos/Runner/Assets.xcassets/AppIcon.appiconset
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Widen the 512 viewBox by 48 on each side: 512/608 = 0.84.
sed 's/viewBox="0 0 512 512" width="512" height="512"/viewBox="-48 -48 608 608" width="1024" height="1024"/' \
  "$svg" > "$tmp/padded.svg"
grep -q 'viewBox="-48' "$tmp/padded.svg" || { echo "unexpected viewBox in $svg" >&2; exit 1; }

for px in 16 32 64 128 256 512 1024; do
  swift tool/render_svg.swift "$tmp/padded.svg" "$out/app_icon_$px.png" "$px"
done
echo "wrote $out"
