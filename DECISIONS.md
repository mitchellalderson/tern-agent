# Decisions

Choices made where the spec was ambiguous or silent. Newest at the bottom of each milestone.

## M0

- **Edition 2024, MSRV 1.88.** Built and tested on stable 1.99.1.
- **Lints live in `[workspace.lints]`** (`unsafe_code = "forbid"`, `clippy::unwrap_used`/`expect_used = "deny"`) instead of per-crate `#![forbid]` attributes; every crate opts in with `[lints] workspace = true`. This also covers the binary. `clippy.toml` allows unwrap/expect in tests; integration-test files add a file-level `allow` because their helpers aren't `#[test]` fns.
- **Hand-rolled migrations on `PRAGMA user_version`** rather than `rusqlite_migration`, to avoid coupling our rusqlite version to another crate's. Forward-only; a DB newer than the binary is refused. SQL lives in `crates/tern-store/migrations/NNNN_*.sql`.
- **Migration 1 adds indexes and FTS triggers** beyond the §14 DDL: `sessions(source, external_ref)`, `runs(session_id)`, `runs(status)`, `messages(session_id, id)`, `schedules(enabled, next_run_at)`, and insert/update/delete triggers for both external-content FTS5 tables.
- **`Store` is a cloneable `Arc<Mutex<rusqlite::Connection>>`** with synchronous methods. SQLite calls are short; revisit (connection pool or `spawn_blocking`) if contention shows up in the daemon.
- **Added `tern config init [--force]`** (not in §12.1). Creates the `~/.tern` layout, a commented `config.toml`, default `persona.md`/`user.md`, and the workspace directory. Never overwrites persona/user notes.
- **`tern doctor` is read-only.** It never creates `~/.tern` or the database. Missing setup is a warning (exit 0); broken things (unparseable config, loose `secrets.toml` permissions, missing secrets, failed integrity check, unreachable provider) are failures (exit 1). `--offline` skips network probes.
- **Provider reachability probe** is an authenticated `GET /v1/models` (Anthropic) or `GET {base_url}/models` (OpenAI-compatible) with a 5 s timeout.
- **`config.toml` uses `deny_unknown_fields`** so typos are reported by `doctor` instead of being silently ignored.
- **`tern config set`** edits with `toml_edit` to preserve comments, parses the value as a TOML literal (falling back to a string), and validates the whole file before an atomic rename.
- **File logging only once `~/.tern/logs` exists**; daily rotation, 14 files kept, filter via `TERN_LOG` (default `info`). No logs go to stderr.
- **Secrets file errors never echo values**: TOML parse errors report only the message, not the quoted source line.
- **E2E tests live in `crates/tern/tests/`** rather than a top-level `tests/` dir, because the workspace root isn't a package and `CARGO_BIN_EXE_tern` is only available to the binary crate's own tests.
- **Unimplemented commands** exist in the CLI with their final shape and exit with code 2 and the milestone they land in.
- **TLS: reqwest's default rustls (aws-lc-rs) with the platform verifier.** Open question for M1: on minimal Linux hosts without a CA bundle the platform verifier has no roots; we may want to bundle `webpki-roots` via a preconfigured rustls client so the static binary works anywhere.
- **Release profile**: `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
