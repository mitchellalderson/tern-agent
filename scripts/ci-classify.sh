#!/usr/bin/env bash
# Classify changed paths (one per line on stdin) for ci.yml's `changes` job.
# Prints code=, rust= and dart= lines (true/false).
#   code: false when every path is docs/markdown/image/LICENSE.
#   rust: crates/, xtask/, Cargo.toml, Cargo.lock, clippy.toml, toolchain files.
#   dart: apps/desktop/.
# Anything else (.github/, scripts/, install.sh, unknown paths) sets both, so
# an unclassified path runs everything.
set -euo pipefail

code=false rust=false dart=false
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case "$f" in
    *.md | *.png | *.jpg | *.jpeg | *.gif | *.svg | *.webp | *.ico | docs/* | LICENSE* | .gitleaks*)
      continue ;;
  esac
  code=true
  case "$f" in
    crates/* | xtask/* | Cargo.toml | Cargo.lock | clippy.toml | rust-toolchain | rust-toolchain.toml)
      rust=true ;;
    apps/desktop/*)
      dart=true ;;
    *)
      rust=true dart=true ;;
  esac
done

echo "code=$code"
echo "rust=$rust"
echo "dart=$dart"
