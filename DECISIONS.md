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

## M1

- **Traits live in `tern-core`**: `Provider`, `Tool`, `ToolRegistry`, `ApprovalHandler`, plus the journal and agent loop. Implementation crates (`tern-providers`, `tern-tools`, `tern-sandbox`) depend on core, never the reverse. The registry is in core (not `tern-tools`, as §4 suggests) because the loop calls it.
- **`Tool::policy` takes `(ctx, args)`**, not `()`. `write_file`/`edit_file` need approval only outside the workspace, which depends on the path argument.
- **Journal API**: `step()` has the §5.2 signature. `step_with()` adds an `InFlight` policy (`Retry`, or `Substitute(output)` for non-idempotent tools) and returns `Outcome { value, replayed }`. Step bodies return `StepOutput { output, usage, messages }`; the messages are committed in the same transaction that completes the step. A `failed` row is re-executed on the next call.
- **Canonical hashing is explicit**: keys are sorted by our own serializer, so a dependency enabling `serde_json/preserve_order` can't silently change hashes.
- **Divergence also deletes messages** appended by the discarded steps (`messages.step_idx`, migration 2).
- **Runs freeze `model` and `system_prompt`** (migration 2). The prompt contains the current time, so rebuilding it on resume would diverge every LLM step. `runs.input_message_id` bounds which session messages a run sees, so a later run's messages don't leak into a replay.
- **LLM step input is a digest**: `{model, messages: n, request_digest}` where the digest is the hash of the full request. Any change is detected without storing the whole history again for every step.
- **Approval steps are always journaled when a tool requires approval**, even when the allowlist or `--yes` answers it. Otherwise "always allow" during the first attempt would remove the step on replay and shift every later index.
- **Allowlist patterns never match commands with shell metacharacters** (`; | & $ \` > < ( )` and newlines), so `git *` doesn't approve `git log; rm -rf ~`.
- **Retries happen inside the `llm` step** (up to 5 attempts; base 1 s doubling, capped at 30 s, jitter factor 0.5–1.0; `retry-after` honored). Only the final result is journaled. A stream that ends without a stop event counts as a retryable error.
- **Consecutive same-role messages are merged** when building requests, since each tool step appends its own `tool_result` message.
- **Path rules**: relative paths resolve in the workspace and may not escape it via `..` or symlinks. Absolute paths are allowed; writes to them need approval.
- **`max_tokens` defaults to 8192** per model call. Not yet configurable.
- **`tern run` output**: final answer on stdout; run/session ids and tool progress on stderr. Exit codes: 0 completed, 1 failed, 130 interrupted. Without `--yes`, approvals prompt on the terminal, or are denied when stdin isn't a TTY.
- **`tern resume` landed in M1** (cheap once the journal existed), with `--yes`. M2 adds daemon auto-resume and `runs show`.
- **IDs are UUIDv7** (hex, no dashes) for sessions and runs: time-ordered and collision-free without coordination.
- **Host shell kills only the direct child** on timeout/cancel. Grandchildren started in the background can outlive it. Killing the whole process group needs `unsafe` or another crate; revisit with the sandbox work in M6.
- **The mock provider picks its reply by the number of assistant turns** in the request, not by call count, so a resumed process continues the script where the crashed one stopped.
- **No real-provider run yet.** The M1 acceptance prompt runs against the mock in `tests/m1.rs`. The real-provider version is `#[ignore]`d and documented in the test.

## M2

- **Compaction summaries are messages, not deletions.** A summary is a user message with `messages.summary_of = N` (migration 3), appended by a `compaction` step. When building context, the latest summary goes first and every non-summary message with `id <= N` is dropped; full history stays in the database for `sessions show` and `session_search`. A later summary folds in the earlier one. `N` is the highest *non-summary* id being summarized; using the summary row's own id would drop kept messages that are older than the previous summary.
- **The compaction split never orphans a tool result**: the kept tail is moved back until it doesn't start with a `tool_result` message.
- **Token estimate** = chars/4 over system prompt, messages, and tool schemas, scaled by the ratio of provider-reported input tokens (including cache reads/writes) to our estimate for the previous request. The ratio comes from journaled usage, so replay makes the same compaction decisions.
- **Context window is configurable**: top-level `context_window` (default 128 000), overridable per provider. There is no per-model table yet.
- **Summarization uses the run's own model** with a fixed system prompt and no tools. Its text isn't streamed as assistant output.
- **`session_search` excludes the current session**, which is already in context and would otherwise match its own query.
- **FTS queries are built from words only**: each word quoted and OR-ed, so user text can't inject FTS5 syntax and BM25 ranks documents matching more terms higher.
- **Sessions get a title from their first message** (first 80 chars), set when the first run is created.
- **Startup recovery is `agent::triage_running_runs`**: runs still `running` that were updated in the last 24 h are returned for resumption; older ones are marked `interrupted`. The daemon (M5) will call it at startup; nothing calls it automatically yet.
- **Added `tern runs list`** (not in §12.1): `runs show` needs a run id, and there was no other way to find one.
- **Ctrl+C is registered before the run banner is printed**, using a unix signal listener that's active from creation, so an interrupt can't arrive before cancellation works.
