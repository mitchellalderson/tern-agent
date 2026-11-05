#!/bin/sh
# Install tern: one static binary, no dependencies.
#
#   curl -fsSL https://raw.githubusercontent.com/mitchellalderson/tern-agent/main/install.sh | sh
#
# Downloads tern-<target>.tar.gz and its .sha256 from a GitHub release (the
# assets .github/workflows/release.yml publishes), checks the checksum, and
# installs the binary. Needs curl or wget, and sha256sum or shasum.
#
# Environment:
#   TERN_REPO         GitHub repo to install from (default: DEFAULT_REPO below)
#   TERN_VERSION      release tag, e.g. v0.1.0 (default: latest)
#   TERN_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   TERN_DOWNLOAD_URL download from this base URL instead of GitHub (a mirror)
set -eu

DEFAULT_REPO="mitchellalderson/tern-agent"

REPO="${TERN_REPO:-$DEFAULT_REPO}"
VERSION="${TERN_VERSION:-latest}"
INSTALL_DIR="${TERN_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'tern-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

if [ -z "${TERN_DOWNLOAD_URL:-}" ]; then
  case "$REPO" in
    "") die "set TERN_REPO=owner/name" ;;
    */*) ;;
    *) die "TERN_REPO must look like owner/name, got '$REPO'" ;;
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
# A shell running under Rosetta reports x86_64 on Apple Silicon; use the native build.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] &&
  [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
  arch=aarch64
fi
target="$arch-$os"
asset="tern-$target.tar.gz"

# HTTPS only: the checksum comes from the same server as the archive, so a
# plain-HTTP mirror would let an on-path attacker replace both.
proto="=https"
if [ -n "${TERN_DOWNLOAD_URL:-}" ]; then
  base="${TERN_DOWNLOAD_URL%/}"
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
if ! fetch "$base/$asset" "$tmp/$asset"; then
  if [ -z "${TERN_DOWNLOAD_URL:-}" ]; then
    say "error: couldn't download $base/$asset"
    say "There may be no published release yet (or the repository is private, or TERN_VERSION=$VERSION doesn't exist)."
    say "See https://github.com/$REPO/releases. To build from source instead (Rust 1.89+):"
    say "  git clone https://github.com/$REPO && cd ${REPO##*/} && cargo build --profile dist -p tern"
    say "  install -m 755 target/dist/tern ~/.local/bin/tern"
    exit 1
  fi
  die "download failed: $base/$asset"
fi
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
pidfile="${TERN_HOME:-$HOME/.tern}/tern.pid"
if [ -r "$pidfile" ] && pid="$(cat "$pidfile" 2>/dev/null)" && [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
  say "tern is running (pid $pid) and still on the old version; restart it to use the new one: tern stop && tern start"
fi
say "next: tern setup (then tern doctor to check everything)"
