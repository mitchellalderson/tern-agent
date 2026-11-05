# CI cost policy

macOS runners bill at roughly 10x a Linux minute on a private repo, so `ci.yml` is built to spend Linux minutes
freely and macOS minutes only where macOS can tell us something Linux cannot.

## Measured before (runs of 2026-11-05)

Per run: Linux `check` 11-23 min, macOS `check` 10-25 min (clippy 1.1, `cargo test-all` 11, dist e2e 13 when it got
that far), plus on a green main push `build-macos` x2 and `desktop-macos` (est. 8 + 8 + 10). Every push to main
started the whole matrix and was not cancelled by the next push. A green main push was about 45-55 macOS minutes
(~450-550 Linux-equivalent) plus ~35 Linux minutes. Most recent runs were red, burning macOS time anyway.

## Rules

| What | When it runs |
| --- | --- |
| gitleaks | every event, always |
| `changes` (diff classifier) | every event; docs/markdown/image/LICENSE-only diffs set `code=false`, which skips everything below |
| `check` (Linux): fmt, clippy, flutter analyze, shellcheck, `cargo test-all` (all Rust and Dart), dist e2e, Docker sandbox | every code change, PRs included |
| `build-linux` (musl x2) | after `check`, unless `skip_builds` |
| `check-macos`: clippy (cfg(macos) code), tern-sandbox/tools/tui tests (Seatbelt), `cargo test -p tern` and TUI smoke against the shipped aarch64 binary, the 4 platform-sensitive Dart test files | only after Linux `check` is green, and only on push to main, release checks (`workflow_call`), manual dispatch, nightly, or a PR labelled `ci-macos` |
| `desktop-macos` (universal DMG; reuses the aarch64 binary from `check-macos`, builds only x86_64) | `workflow_dispatch` and the nightly schedule (skipped if main has no commits in 24 h); release tags are built by `release.yml` |

Other rules: every job has `timeout-minutes` (macOS 30); `CARGO_INCREMENTAL=0`; rust-cache only saves from main;
a newer push to the same ref cancels the older run (bursts to main test only the tip), except tag runs, which are
never cancelled; unrelated PR labels do not re-trigger CI.

## Expected cost per event (estimates, not yet averaged over many runs)

| Event | Linux min | macOS min |
| --- | --- | --- |
| docs-only push | ~1 | 0 |
| PR / branch, no label | ~25-35 | 0 |
| PR with `ci-macos` | ~25-35 | ~15-20 |
| main push (tip of a burst) | ~35 | ~15-20 |
| red Linux main push | ~15-25 | 0 |
| manual dispatch with DMG | ~35 | ~30 |
| release tag | unchanged | check-macos + release.yml builds |

Before: ~45-55 macOS min per green main push, for every push in a burst. After: ~15-20, once per burst.

## What this can miss

- OS-independent tests no longer run on macOS (a macOS-only failure of a generic Rust or Dart test, e.g. path or
  line-ending behaviour, would not show). The macOS job still covers the crates with cfg(macos) code; if one more
  crate turns out macOS-sensitive, add it to the `cargo test -p` line in `check-macos`.
- Flutter tests other than the four platform files run only on Linux.
- Intermediate commits in a burst to main are cancelled and never tested individually.
- PRs without `ci-macos` merge with no macOS signal until the push to main; label PRs that touch sandbox,
  service, tray, paths or desktop packaging.
- The x86_64 macOS binary is built (and run through `lipo`) only for DMGs and releases; it is never executed in CI.
