#!/bin/sh
# Run a tern daemon with the mock provider in a throwaway home, for
# developing the app without API keys. Prints the TERN_HOME to point the
# app at:  TERN_HOME=<printed path> flutter run -d macos
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
tern=${TERN_BIN:-$root/target/debug/tern}
[ -x "$tern" ] || (cd "$root" && cargo build -p tern)

home=$(mktemp -d /tmp/tern-demo.XXXXXX)
mkdir -p "$home/.tern"
echo "TERN_HOME=$home/.tern"
exec env HOME="$home" TERN_HOME="$home/.tern" \
  TERN_PROVIDER="mock:$here/demo_script.json" \
  "$tern" daemon
