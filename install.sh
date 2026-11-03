#!/bin/sh
# Install tern: one static binary, no dependencies.
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/tern/main/install.sh | sh
#
# Environment:
#   TERN_REPO         GitHub repo to install from (owner/name)
#   TERN_VERSION      release tag, e.g. v0.1.0 (default: latest)
#   TERN_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   TERN_DOWNLOAD_URL download from this base URL instead of GitHub (a mirror)
set -eu

REPO="${TERN_REPO:-OWNER/tern}"
VERSION="${TERN_VERSION:-latest}"
INSTALL_DIR="${TERN_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'tern-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

if [ -z "${TERN_DOWNLOAD_URL:-}" ]; then
  case "$REPO" in
    OWNER/*) die "set TERN_REPO=owner/name (this script hasn't been pointed at a repository yet)" ;;
  esac
fi

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) die "unsupported OS $(uname -s); build from source with cargo" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) die "unsupported CPU $(uname -m); build from source with cargo" ;;
esac
target="$arch-$os"
asset="tern-$target.tar.gz"

proto="=https"
if [ -n "${TERN_DOWNLOAD_URL:-}" ]; then
  base="${TERN_DOWNLOAD_URL%/}"
  proto="=http,https"
elif [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --proto "$proto" --tlsv1.2 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "need curl or wget"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "need sha256sum or shasum to verify the download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset from $base"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || die "checksum download failed"

expected="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
actual="$(sha256 "$tmp/$asset")"
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/tern" "$INSTALL_DIR/tern" 2>/dev/null || { cp "$tmp/tern" "$INSTALL_DIR/tern" && chmod 755 "$INSTALL_DIR/tern"; }

say "installed $("$INSTALL_DIR/tern" --version) to $INSTALL_DIR/tern"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "add $INSTALL_DIR to your PATH, e.g.: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
say "next: tern config init && tern doctor"
