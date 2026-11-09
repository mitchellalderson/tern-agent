# Working on tern with Claude

How release work is run in this repo. Read this first in a new session.

## Rules

- **One task, one session.** Start each new task in its own sub-session on Sonnet 5.5, in its own worktree.
- **One feature, one branch and PR.** The owner merges. Never merge, push to `main`, or tag without being asked.
- **Tag and release only when the owner says so**, following `docs/release-checklist.md`.
- **Clean up after each task:** once its PR is merged, archive the session and remove its worktree.
- **Changelog:** add a fragment, `changelog.d/<slug>.<kind>.md`. Never edit `CHANGELOG.md` directly
  (see `changelog.d/README.md`).
- **UI:** follow the tern design system: `docs/brand/brand-guide.md`, `docs/brand/design-system.md`,
  `docs/brand/tokens.json`, `apps/desktop/lib/theme.dart`. Keep it clean, simple, with plenty of white space.
- **Copy:** shorter, clearer, fewer words. One short line per option, with details in tooltips or docs.

## Checks

- `cargo` isn't on the default PATH. Use `export PATH=/opt/homebrew/opt/rustup/bin:$PATH`.
- Integration tests: `crates/tern/tests/it/` is one binary, one module per file
  (`cargo test -p tern --test it -- t1c::`). Add new files as modules there, not as new `tests/*.rs` binaries (each one is a link step).
- Flutter isn't installed on the owner's Mac. Verify with PR CI: `gh pr checks <n> --watch`.
- PR CI runs only the jobs the changed paths need (`scripts/ci-classify.sh`, `docs/ci-cost.md`). macOS jobs need
  the `ci-macos` label.
- UI changes are checked by `flutter analyze` and widget tests only. Ask the owner to look at them in the app.

## Current release

The status, open work and next steps are in `docs/releases/`.
