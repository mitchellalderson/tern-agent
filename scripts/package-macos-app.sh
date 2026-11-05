#!/bin/sh
# Build tern.app with its own `tern` inside and pack it into a .dmg (and a
# .zip), signed and notarized when credentials are set.
#
#   scripts/package-macos-app.sh <tern-binary> <out-dir>
#
# This is release-macos.sh, kept under the name release.yml calls. See that
# script for what it does and docs/desktop-release.md for the credentials
# (MACOS_SIGN_IDENTITY, and the notary profile, API key or Apple ID variables).
# Writes <out-dir>/tern-desktop-macos.dmg, .zip and their .sha256 files.
exec "$(dirname "$0")/release-macos.sh" "$@"
