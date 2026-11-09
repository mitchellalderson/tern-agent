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
| `changes` (diff classifier, `scripts/ci-classify.sh`) | every event; docs/markdown/image/LICENSE-only diffs set `code=false`, which skips everything below. On PRs it also sets `rust` and `dart` |
| Linux, four parallel jobs: `lint` (fmt, shellcheck, flutter analyze, clippy), `rust-tests` (`cargo test-all --rust`, Docker sandbox), `dart-tests` (`cargo test-all --dart`), `e2e-dist` (`scripts/smoke.sh` against the shipped `dist` binary) | every code change outside PRs; on PRs, see the table below |
| `build-linux` (musl x2) | after all four Linux jobs, unless `skip_builds` |
| `check-macos`: clippy (cfg(macos) code), tern-sandbox/tools/tui tests (Seatbelt), `scripts/smoke.sh` plus the `t8a` service tests against a debug binary, the 4 platform-sensitive Dart test files. The aarch64 `dist` build (~19 min) runs only on manual dispatch and the nightly, where the DMG needs it | only after all four Linux jobs are green, and only on push to main, release checks (`workflow_call`), manual dispatch, nightly, or a PR labelled `ci-macos` |
| `desktop-macos` (universal DMG; reuses the aarch64 binary from `check-macos`, builds only x86_64) | `workflow_dispatch` and the nightly schedule (skipped if main has no commits in 24 h); release tags are built by `release.yml` |

### PRs run only what the paths need

| Changed path | `lint` | `rust-tests`, `e2e-dist` | `dart-tests` |
| --- | --- | --- | --- |
| `crates/`, `xtask/`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rust-toolchain*` | shellcheck, fmt, clippy | yes | no |
| `apps/desktop/` | shellcheck, flutter analyze | no | yes |
| `.github/`, `scripts/`, `install.sh`, `.cargo/`, anything else | all of it | yes | yes |
| docs, markdown, images, LICENSE only | no | no | no |

A mixed PR runs the union. Skipped jobs count as passing for `check-macos` and `build-linux`. Pushes to `main`, tags,
manual runs, the nightly and release checks always run everything. No branch protection is set today; if required
checks are added, require only jobs that always run (`secrets`, `changes`).

Other rules: every job has `timeout-minutes` (macOS 30; the dist build on dispatch and nightly needs more); `CARGO_INCREMENTAL=0`; rust-cache only saves from main;
a newer push to the same ref cancels the older run (bursts to main test only the tip), except tag runs, which are
never cancelled; unrelated PR labels do not re-trigger CI.

## Expected cost per event (estimates, not yet averaged over many runs)

| Event | Linux min | macOS min |
| --- | --- | --- |
| docs-only push | ~1 | 0 |
| PR, no label, Rust change | ~25-35 | 0 |
| PR, no label, desktop-only change | ~8-12 | 0 |
| PR with `ci-macos` | as above | ~15-20 |
| main push (tip of a burst) | ~35 | ~15-20 |
| red Linux main push | ~15-25 | 0 |
| manual dispatch with DMG | ~35 | ~30 |
| release tag | unchanged | check-macos + release.yml builds |

Before: ~45-55 macOS min per green main push, for every push in a burst. After: ~15-20, once per burst.

## Measured after (cold cache, run 42114363950 on ci/cost-down)

`macOS-specific tests` 17.7 min (clippy 1.1, sandbox/tools/tui tests 0.4, dist build 7.9, `cargo test -p tern` + TUI
smoke 7.3, Dart 0.2); the x86_64 build plus DMG job added ~10-12 min. Warm caches on main should be lower.

## Split Linux jobs (2026-11-06)

The single Linux `check` job ran lint, `cargo test-all` (~11 min) and the dist e2e run (~13.5 min) in sequence, about
30 min, and a failing Rust suite skipped the e2e step, so its failures surfaced one run later. Split into four
parallel jobs: lint reports in ~5 min, every suite reports on every run, and the Linux phase (and so the start of
macOS) is bounded by the slowest job, `e2e-dist` (~15-17 min). Each job compiles on its own (own rust-cache key), so
Linux minutes go up roughly 1.5-2x; macOS minutes are unchanged. Four Linux caches plus the musl and macOS ones share
the repo's 10 GB Actions cache; if eviction shows up as cold builds, share a key between `rust-tests` and `dart-tests`.

## What this can miss

- OS-independent tests no longer run on macOS (a macOS-only failure of a generic Rust or Dart test, e.g. path or
  line-ending behaviour, would not show). The macOS job still covers the crates with cfg(macos) code; if one more
  crate turns out macOS-sensitive, add it to the `cargo test -p` line in `check-macos`.
- Flutter tests other than the four platform files run only on Linux.
- Intermediate commits in a burst to main are cancelled and never tested individually.
- PRs without `ci-macos` merge with no macOS signal until the push to main; label PRs that touch sandbox,
  service, tray, paths or desktop packaging.
- The x86_64 macOS binary is built (and run through `lipo`) only for DMGs and releases; it is never executed in CI.

## Test audit (2026-11-09)

Measured from run 42654243767 (the macOS job hit its 30 min limit while still passing) and a local run.

Where the time went:

| Job | Before | What dominated |
| --- | --- | --- |
| rust tests (linux) | 6.5 min | ~2.5 min of test binaries run one after another; 94 binaries |
| e2e-dist (linux) | 12.5 min | `dist` build 8.3 min, then `cargo test -p tern --tests` again (1.5 min compile, 74 s run) |
| macOS | 30+ min (cancelled) | `dist` build 19.1 min, 56 `-p tern` test binaries compiled and linked 4.6 min, then `ap3` alone ran 71 s |
| dart tests (linux) | 3.1 min | not touched |

Test run time was never the problem: 1261 tests add up to ~540 CPU-seconds. The cost is linking one binary per
`tests/*.rs` file and building `dist` twice.

What changed:

- `crates/tern`, `tern-auth`, `tern-bundle` and `tern-tools` each have one integration binary, `tests/it/`, with one
  module per old file. `crates/tern`: 56 binaries became 1 (239 tests in ~12 s on 8 cores, against ~70 s serially). Add
  new test files as modules there.
- `scripts/smoke.sh` is the shipped-binary smoke set: create and read back a file, crash and resume, the daemon API over
  the socket, the sandbox through the binary, an injected write held in the outbox, and the TUI. e2e-dist and macOS run
  it instead of the whole `-p tern` suite, which `rust-tests` already runs.
- macOS tests a debug binary. The 19-minute `dist` build runs only when its output is used (manual runs and the nightly).
- `[profile.dev] debug = "line-tables-only"`: cold test build 56 s to 48 s, target dir 6.1 GB to 4.7 GB (local).
- Not adopted: `cargo nextest` (the whole `tern` integration suite already runs in ~12 s; a per-test process would add
  startup cost) and `opt-level` for dependencies (not measured to help).
- The webhook test `a_signed_delivery_starts_one_tainted_run_and_everything_else_is_refused` was flaky: the daemon
  answers 413 and closes while the client is still uploading the oversized body, so the client sometimes saw a broken
  pipe instead of the 413. The test now accepts either (both mean refused; the final run count shows nothing started).
