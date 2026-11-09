#!/usr/bin/env bash
# Smoke tests against a built tern binary (the shipped `dist` build in CI):
# one end-to-end run per surface (CLI, daemon API, sandbox, taint, TUI), not
# the whole suite again. `cargo test --workspace` already ran that.
# Usage: scripts/smoke.sh path/to/tern [extra test filters...]
set -euo pipefail
bin=${1:?usage: scripts/smoke.sh path/to/tern [extra test filters...]}
shift
cd "$(dirname "$0")/.."

TERN_TEST_BIN="$bin" cargo test -p tern --test it -- \
  m1::create_then_read_back_a_file \
  m1::crash_mid_run_then_resume \
  m5::socket_api_runs_streams_and_approves \
  sandbox:: \
  t3::an_injected_write_waits_in_the_outbox_and_runs_once_when_approved \
  "$@"
python3 scripts/tui-smoke.py "$bin"
