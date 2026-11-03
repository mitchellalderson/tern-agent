# Research spike: an optional Inngest tie-in

Status: research only. No production code; nothing in the workspace depends on Inngest.
Date: 2026-11-03. SDK examined: `inngest/inngest-rs` at `7b32112` (main), crates.io `inngest 0.1.1`.

## TL;DR / recommendation

**Do a narrow piece first. Don't replace the journal with Inngest.**

1. **Now (cheap, no SDK):** an opt-in *event sink*. When `[inngest]` is configured, tern publishes run lifecycle
   events (`tern/run.started`, `tern/run.finished`, `tern/approval.requested`, ...) to the Inngest Event API with the
   reqwest client we already have, and gains an `inngest_send_event` tool. This lets people chain their own Inngest
   workflows off tern. It needs no new dependencies, no inbound HTTP, and it does nothing unless configured.
2. **Later (after M5, gated on SDK work):** inbound *triggers* through a `tern-inngest` crate behind a cargo feature.
   Inngest functions start tern runs, coarse-grained: one `step.run` starts the run and `step.wait_for_event` waits for
   `tern/run.finished`. The SQLite journal stays the only source of truth for steps. This is blocked on two SDK gaps
   that we'd fix upstream first: TLS/dependency hygiene, and Connect (outbound worker connections).
3. **Don't do yet:** Inngest as the per-step executor (`step.run` per LLM/tool step). It's feasible, and the replay
   models line up surprisingly well, but it duplicates state the journal already holds durably. It also inverts
   control flow through `drive_inner`, needs a reachable HTTP endpoint, and adds ~3 MB plus OpenSSL to a binary
   whose point is to be small, static and offline.

The core reason: tern's journal already gives it the property Inngest sells (memoized, crash-safe step replay) on a
single host. Inngest's distinctive value for tern is **externally triggered, long-lived waits and event plumbing**,
not durability. So the tie-in should start at the edges (events in and out), not in the core loop.

## What inngest-rs is today

| Aspect | Finding |
|---|---|
| Maturity | README says **"Alpha"**. Last published release `0.1.1` on **2024-11-01**. A `v0.2.0` release PR (#54) has been open since April 2026 and was last refreshed 2026-08-26. The 14 unreleased commits on main are mostly protocol-conformance hardening (step identity, signature checks, config surface, durable `send_event`). 55 commits total; ~6.8k lines in `inngest/src`. |
| Step surface | `step.run`, `sleep`, `sleep_until`, `wait_for_event`, `invoke`, `send_event(s)`. Parallel scopes exist (`__enter_parallel_scope`, still internal-looking). |
| Function config | Triggers: event (+ `if` expression) and cron. `retries`, `cancel` (cancel-on-event), `idempotency`, `batch_events`, `rate_limit`, `debounce`, `priority`, `concurrency`, `throttle`, `singleton`, `timeouts`, `on_failure`. |
| Missing | **Connect** (outbound WebSocket worker, so no public URL is needed): unchecked in `docs/plans/007-advanced-runtime-modes.org`. **Middleware**, streaming, checkpointing. Open issues from 2024: step macros, docs, middleware. |
| Serving | HTTP only: axum **0.6** handlers (`serve::axum::{introspect, register, invoke}`). The Inngest server must be able to `POST` to tern. |
| Control flow | Each new step returns `Err(Error::Interrupt(..))` after executing (or planning) and unwinds to the handler. The server re-invokes the whole function per step and passes memoized step state in the request. Step IDs are `sha1(name:position)`. |
| Errors | `step.run` needs a user error type that is `Serialize + Deserialize + std::error::Error + Into<inngest::Error>`. `NoRetry` and `RetryAt(after)` exist. |
| Hygiene | Uses `eprintln!`/`println!` for warnings (e.g. step-order divergence, event deserialization failures). That would scribble over the TUI and breaks our "no stderr logs" rule. Uses `RwLock::unwrap()` throughout. |

### Dependencies vs. our constraints (checked with a throwaway prototype)

The SDK's `Cargo.toml` uses `reqwest 0.11` **with default features**, `axum 0.6`, `hyper 0.14 (full)`, `tower 0.4` and
`tokio (full)`. A downstream crate can't turn reqwest's default features off, so:

- **OpenSSL is pulled in on Linux.** `cargo tree --target x86_64-unknown-linux-musl -i openssl-sys` gives
  `openssl-sys ← native-tls ← hyper-tls ← reqwest 0.11 ← inngest`. That breaks the musl/rustls/no-OpenSSL rule
  (DECISIONS M0). Our `cargo zigbuild` musl CI job would need a musl OpenSSL, or `openssl/vendored` forced through
  feature unification. Both are what we chose to avoid.
- **The HTTP stack gets duplicated**: two versions each of `reqwest` (0.11/0.13), `hyper` (0.14/1.x), `http`
  (0.2/1.x), `h2`, `http-body`, `tower`, `base64`, `socket2`, `sync_wrapper`, `syn`.
- **Crate count:** 132 → 215 (+83) for a tern-like baseline (reqwest 0.13 rustls + tokio + serde).
- **Binary size** (macOS arm64, our release profile: LTO, `codegen-units=1`, strip, `panic=abort`): baseline
  3.65 MB → **6.62 MB (+2.97 MB)** with one registered function served over axum. For scale, today's
  `target/release/tern` is 9.86 MB, so that's roughly +30%. On Linux it would be larger once OpenSSL is linked
  statically.

Prototype: `/private/tmp/claude-501/-Users-mitch-Development-ox-agent/5a4ec69b-31d3-479c-8bff-f99d1f3cbd6a/scratchpad/proto`.
It's a standalone crate outside the repo and has nothing to do with the workspace or CI. It compiles a
`tern/run.requested` function with one `step.run` and serves it on axum. It was not run against a dev server.

**Fix path:** an upstream PR to inngest-rs that moves to `reqwest 0.12+` with `default-features = false` and
`rustls`/`native-tls` features, `axum 0.8`/`hyper 1`, a trimmed `tokio` feature set, and `tracing` instead of
`eprintln!`. Since the user works at Inngest, that's the natural first step if piece 2 goes ahead, and it helps every
Rust user of the SDK, not just tern.

## Where Inngest could fit

| Candidate | Fit | Notes |
|---|---|---|
| Run lifecycle / observability **events out** | **Good, cheap** | Plain `POST {event_api}/e/{event_key}`; no SDK needed. Gives the user an Inngest-side event stream and triggers for their own workflows. Doesn't give step-level traces of tern runs; that needs the executor option. |
| `inngest_send_event` **tool** | **Good, cheap** | The agent can kick off the user's existing Inngest workflows. Approval-gated like other side-effecting tools (`idempotent: false`). |
| **Event triggers in** (webhooks, GitHub, "run this prompt") | **Good, but blocked** | Inngest webhooks + transforms are a nicer front door than hand-rolled webhook handlers in M5. They need Inngest to reach tern: without Connect, tern must be publicly reachable or run next to a dev/self-hosted server. A laptop or home box behind NAT is exactly tern's deployment. |
| **Telegram** via Inngest | Poor | M5's Telegram gateway long-polls Bot API outbound and works offline-from-Inngest. Routing it through Inngest adds a hop and a dependency for no gain. |
| **Cron** (§12.5 / `schedules` table) | Poor | Inngest crons are *static* function config, set at app sync. tern schedules are rows created at runtime (by the user or the agent), so each change would mean re-syncing the app. The dynamic alternative (one function per schedule looping `sleep_until` → send run event, cancelled by `tern/schedule.deleted`) works, but it's more moving parts than a tokio timer over `schedules(enabled, next_run_at)`. Keep cron local. The only win would be "fires while tern is down", and that's moot because the run still executes on the tern host. |
| **Approvals** via `step.wait_for_event` | Good *if* runs are Inngest-driven | Clean mapping (below). The real draw is approving from anywhere (Slack, phone) after the process restarted. tern can't use it unless the run itself executes as an Inngest function. |
| **Fan-out / subagents** | Later | `step.invoke` and parallel steps map onto subagents well, but tern has no subagents yet. Revisit with the executor question. |
| Per-step **executor** | Feasible, not now | See the mapping table and the costs below. |

## Proposed boundary (keeps tern offline-first)

Rules:

- `tern-core` defines small traits with local implementations. No Inngest types, deps or features in core.
- A new crate, **`tern-inngest`**, holds every Inngest-specific piece. The `tern` binary depends on it only under
  `--features inngest`, **off by default and not built by the release CI**, so the shipped static binary is unchanged.
- Runtime is opt-in on top of compile-time: even with the feature, nothing touches the network unless
  `[inngest]` exists in `config.toml`. Event and signing keys are secrets in `secrets.toml`, and both are fed to
  `Journal::with_redactions`.
- `tern doctor` grows an `inngest` check (key present, Event API reachable, skipped with `--offline`).

Piece 1 needs no SDK, so its sink can live in `tern-inngest` with only `reqwest`, and the feature costs ~0 bytes.

```rust
// tern-core/src/events.rs (alongside AgentEvent): fan-out beyond the in-process bus.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Best-effort; failures are logged, never fail a run.
    async fn publish(&self, ev: &AgentEvent) -> Result<(), SinkError>;
}

// tern-core (M5): where run requests come from. Local impls: CLI/TUI, Telegram, cron table.
#[async_trait]
pub trait RunSource: Send + Sync {
    async fn next(&self, cancel: &CancellationToken) -> Option<RunRequest>;
}
pub struct RunRequest {
    pub session: SessionTarget,      // new | existing id | (source, external_ref)
    pub prompt: String,
    pub trigger: &'static str,       // "user" | "cron" | "telegram" | "inngest"
    pub reply: Option<ReplyTarget>,  // where the final text goes (deliver_to)
}
```

```rust
// crates/tern-inngest (feature = "inngest" in crates/tern)
pub struct InngestConfig { pub app_id: String, pub event_api: Url, pub api_origin: Url, /* keys via secrets */ }

/// Piece 1: AgentEvent -> `tern/*` events on the Event API. Uses tern's reqwest; no SDK.
pub struct InngestSink { /* client, key, filters */ }
impl EventSink for InngestSink { /* ... */ }

/// Piece 2: serves `/api/inngest` on the daemon's HTTP listener (or Connect, once the SDK has it).
pub struct InngestRunSource { /* channel fed by the function below */ }
impl RunSource for InngestRunSource { /* ... */ }

// Registered function, coarse-grained: the journal stays authoritative for steps.
//   trigger: event "tern/run.requested"
//   cancel:  "tern/run.cancel" if event.data.run_id == async.data.run_id
//   1. step.run("start")          -> enqueue RunRequest, return run_id   (idempotent on event id)
//   2. step.wait_for_event("done", "tern/run.finished", if run_id matches, timeout 24h)
//   3. return final_text / status
```

`RunSource`/`EventSink` are useful without Inngest. Telegram and cron become `RunSource`s, and a future webhook or
ntfy sink is an `EventSink`. So the trait work lands in M5 anyway, and Inngest is just one more implementation.

### If we ever do the executor (sketch, not a proposal)

The journal's public surface is generic (`step_with<I, O, F>`), so a dyn trait needs an erased form:

```rust
#[async_trait]
pub trait StepExecutor: Send + Sync {
    async fn run_step(
        &self, run: &str, idx: u32, kind: StepKind, input: Value,
        in_flight: InFlight<Value>,
        f: BoxFuture<'_, Result<StepOutput<Value>, StepError>>,
    ) -> Result<Outcome<Value>, StepError>;
}
// LocalExecutor  = today's Journal.
// InngestExecutor = step.run(id) { journal.step_with(...) }: layered, not replacing (see the non-idempotent row below).
```

and `StepError` gains `Suspend` (the SDK's `Interrupt`). `drive()` must pass it through **without** marking the run
`failed` or emitting `RunFinished`. Every side effect between steps in `drive_inner` (events, `set_run_status`) has
to be replay-safe, since the function body re-runs once per step.

## Mapping: tern steps → Inngest semantics

| tern | Inngest | Notes / gaps |
|---|---|---|
| Run (`runs` row, driven from step 0) | Function run, re-invoked per step from the top with memoized state | **Same replay model.** tern already rebuilds context from SQLite and replays from step 0, so the loop is already "Inngest-shaped". Each step is one HTTP round trip that replays all prior steps. |
| Step index `idx` (+ input hash for divergence) | Step ID `sha1("{name}:{pos}")` | Use names like `llm:{idx}:{hash8}`. A changed input then yields a *new* Inngest step instead of a stale memo; that matches our "divergence truncates" rule (the SDK otherwise only warns and enters recovery mode). |
| `llm` step, `InFlight::Retry`, 5 in-step attempts with backoff + `retry-after` | `step.run`; function `retries(n)`; `RetryAt(after)` | Keep retries *inside* the step so both backends behave the same, and set Inngest retries low (1–2) as a crash backstop. Streaming deltas only reach the TUI if the process serving the HTTP call is the one holding the event bus, i.e. the daemon. |
| `tool`, idempotent (`InFlight::Retry`) | `step.run`, normal retries | Direct fit. |
| `tool`, non-idempotent (`InFlight::Substitute`) | **No equivalent**: Inngest steps are at-least-once; retries are configured per function, not per step | Layer the local journal *inside* `step.run`: a crashed attempt leaves a `started` row, the Inngest retry hits `step_with`, and it substitutes `INTERRUPTED_TOOL_MESSAGE` instead of re-executing. That only works if the retry lands on the same host and DB, so the journal can't be dropped. |
| `approval` (allowlist → ask → timeout; always journaled) | `step.run("approval-check:{idx}")` (allowlist + emit `tern/approval.requested`) → `step.wait_for_event("tern/approval.decided", if run_id && call_id match, timeout = approval_timeout)` → `step.run("approval-apply:{idx}")` (writes "always allow") | `None` from the wait = `Timeout`. One tern step becomes 2–3 Inngest steps, which is fine because IDs are strings. The TUI, Telegram or any external UI answers by sending an event. The run is suspended, not holding a process, which is a real gain over today's in-memory oneshot. |
| `compaction` (Retry, `summary_of`) | `step.run` | Direct fit. The decision is deterministic from journaled usage, as both backends require. |
| Cancellation (`CancellationToken`, run → `interrupted`) | `cancel` on `tern/run.cancel` with an `if` on `run_id` | Inngest cancel stops *scheduling* further steps; it doesn't abort an in-flight request. Keep the local token for the streaming call, and map Inngest-cancelled runs to `interrupted` locally. |
| Crash recovery (`triage_running_runs`) | Inngest re-delivers the step | Triage must skip runs owned by Inngest (new `runs.executor` column) or they'd be driven twice. |
| `max_iterations` (40) | Inngest per-run step cap and state size limits | Check the current server limits on steps per run, step output size and total run state. LLM turns with large tool results get re-sent on every invocation. |
| Run failure → `failed` | `on_failure` handler | Sets local status. |
| `trigger: cron` | Cron trigger / `sleep_until` loop | See "Where Inngest could fit": keep local. |

## Costs and risks

- **Offline / local-first.** Every inbound or executor mode needs an Inngest server: Cloud (public URL, signing key),
  the dev server (`inngest dev`, a separate binary), or self-hosted `inngest start`. None of these is "one static
  binary with no services". Hence opt-in at compile time *and* run time, and the event sink as the first step,
  since it degrades to a no-op.
- **Reachability.** Without Connect, Inngest must `POST` to tern. That rules out the typical NAT'd laptop or home
  server unless the user runs a tunnel or a local dev/self-hosted server. Connect support in the Rust SDK is the
  single biggest unlock for piece 2.
- **Dependencies.** OpenSSL on Linux, a duplicated HTTP stack, +83 crates, ~+3 MB (+30%). Fixable upstream (see above),
  but not something to take as-is into the default build.
- **SDK maturity.** Alpha, last release two years old, the release PR has sat for months, no middleware, and the
  ergonomics are still rough (`into_dev_result!`, `__`-prefixed parallel helpers, required serde error type). The
  surface may well change before 1.0, so we'd pin a git rev and keep all usage inside `tern-inngest`.
- **Two sources of truth (executor mode).** Messages, FTS, memories and usage live in SQLite and `tern runs show`
  reads them. Inngest would hold a second copy of step outputs, and the two must agree across replays and retries.
- **Payload growth (executor mode).** Memoized state travels with every invocation, so a 40-iteration run with large
  tool outputs re-sends megabytes per step.
- **Secrets.** Signing and event keys must be redacted. Step outputs sent to Inngest Cloud would include conversation
  content, which is a privacy shift for a personal agent and should be stated plainly in docs and config.

## Open questions

1. Is Connect for inngest-rs on Inngest's roadmap, and on what timeline? It decides whether piece 2 is practical for
   tern's NAT'd, single-host deployment.
2. Would Inngest accept the dependency-modernization PR (reqwest 0.12+/rustls feature, axum 0.8, tracing), and is
   v0.2.0 going to ship first?
3. For piece 2, does the M5 daemon get an HTTP listener anyway (webhooks, health)? If not, adding one only for
   Inngest raises the cost.
4. Which events should the sink publish by default: lifecycle only, or also tool calls and approvals? Conversation
   text should probably never be sent unless explicitly enabled.
5. Is there real user demand for approving from outside the TUI/Telegram (the strongest argument for the
   wait-for-event approval path), or is Telegram already enough?
6. What are the current server limits (steps per run, step output size, run state size, request timeout) for
   self-hosted vs. Cloud? They bound the executor option and long single-step runs in piece 2.

## Effort (rough)

| Piece | Effort | Depends on |
|---|---|---|
| 1. `EventSink` trait + `InngestSink` (reqwest, no SDK) + `inngest_send_event` tool + config/secrets/doctor + wiremock tests | **2–3 days** | Nothing; can land any time after M5's event plumbing or before it. |
| Upstream SDK PR: rustls / no default features, axum 0.8 / hyper 1, tracing | 2–4 days plus review | Inngest maintainers. |
| 2. `RunSource` trait in M5 + `tern-inngest` coarse-grained trigger function + dev-server e2e test (not in default CI) | **1–1.5 weeks** | M5 daemon; the SDK PR; ideally Connect. |
| 3. Per-step executor (`StepExecutor`, `Suspend` plumbing, wait-for-event approvals, ownership in triage, size limits) | **3–4 weeks**, plus ongoing upkeep against an alpha SDK | Pieces 1–2 and a stable SDK. |
