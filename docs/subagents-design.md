# tern sub-agents: delegation, fan-out and coordination

Status: S0–S3 built (2026-11-03); S4–S5 not started. See "Implementation notes" at the end and DECISIONS.md
"S0–S1" and "S2–S3" for where the code differs from this design. Related: [gui-spec.md](gui-spec.md) §7.2 (agent
profiles and "per-role models", which this builds toward), [research/inngest-tie-in.md](research/inngest-tie-in.md)
("Fan-out / subagents: later"), and the "Claude Duet" channels question in gui-spec §11.2, which is a different idea
and not covered here.

## TL;DR

- **A sub-agent is a child run in its own session.** The parent's `agent_spawn` tool creates a session
  (`source = "subagent"`) and a run linked by `runs.parent_run_id` / `parent_call_id`. The child gets a fresh context
  (its brief is its first user message), the same workspace and journal, and is driven concurrently on its own task.
  It is an ordinary run in every other way: journaled, resumable, inspectable, cancellable.
- **Communication is handle-based and journaled** (option B, §3): `agent_spawn` returns at once; `agent_wait` blocks
  on any/all children; `agent_send` answers or steers a child; `agent_cancel` and `agent_status` round it out. A child
  asks its parent with `agent_ask_parent`. Anything that arrives while the parent is busy (a child finished, a child
  asked a question) is delivered at the parent's next model call through a new journaled `inbox` step, so nothing is
  lost and replay stays deterministic.
- **A parent run cannot finish with live children.** If the model ends its turn while children run, the loop waits for
  them (an `inbox` step) and hands their results back before the run may complete. Cancelling a run cancels its
  subtree.
- **Crash safety falls out of the journal.** `agent_spawn` is idempotent (keyed by the tool call id), `agent_wait` is
  re-executable, children are resumed by the existing startup triage, and a parent waiting on a child that nobody is
  driving resumes it.
- **Front ends see child runs as first-class runs** with `parent_run_id`. The desktop shows an agents card per fan-out
  with live per-child activity, expandable to the child's transcript; the side panel's NOW section lists children; the
  run inspector shows the run tree. The TUI shows one live line per child.
- **The system prompt change that makes tern delegate proactively ships last** (§6), once the machinery and the UI to
  watch it are in place.

## 1. Model

### 1.1 What a sub-agent is

| Concept | Representation | Why |
|---|---|---|
| Sub-agent | a **run** with `parent_run_id` set | Runs are already the unit of journaling, resume, cancel, events and inspection. |
| Its conversation | its **own session**, `source = "subagent"`, `external_ref = parent_run_id` | `Store::run_context` (crates/tern-store/src/repo.rs) builds context from *all* session messages up to the run's input. Children in the parent's session would leak into each other's and the parent's context. A separate session gives a clean context for free. |
| Its identity | `label` (short slug, unique per parent run: `research-1`, `reviewer`) | What the model, the UI and approvals call it. |
| Its brief | the child run's input message | Already how runs start (`Agent::start_run`). |
| Its result | the child's final assistant text (`RunOutcome::final_text`), capped at 16 KB | Returned to the parent through `agent_wait` or the inbox. The full transcript stays in the child session. |

A child session is never reused: one spawn, one session, one run. Follow-up work for the same child goes through
`agent_send` while it runs; after it finishes the parent spawns a new one (and can include the old result in the
brief). This keeps "one active run per session" (`Manager::start`) true without exceptions.

### 1.2 Store changes

New migration `crates/tern-store/migrations/0006_subagents.sql`:

```sql
-- A run started by another run's agent_spawn call.
ALTER TABLE runs ADD COLUMN parent_run_id TEXT REFERENCES runs(id);
ALTER TABLE runs ADD COLUMN parent_call_id TEXT;     -- the tool call that spawned it
ALTER TABLE runs ADD COLUMN label TEXT;              -- unique per parent run
ALTER TABLE runs ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN tool_scope TEXT;         -- JSON array of tool names; NULL = all
ALTER TABLE runs ADD COLUMN budget_tokens INTEGER;   -- NULL = unlimited
ALTER TABLE runs ADD COLUMN max_iterations INTEGER;  -- NULL = config default
-- Makes agent_spawn idempotent: re-executing the step finds the same child.
CREATE UNIQUE INDEX runs_parent_call ON runs(parent_run_id, parent_call_id);
CREATE INDEX runs_parent ON runs(parent_run_id);

-- Messages between runs of one tree, consumed by journal steps.
CREATE TABLE agent_mail (
  id INTEGER PRIMARY KEY,
  to_run_id TEXT NOT NULL REFERENCES runs(id),
  from_run_id TEXT,                 -- NULL = the user (POST /v1/runs/{id}/message)
  kind TEXT NOT NULL,               -- finished | question | answer | message
  body TEXT NOT NULL,               -- JSON
  dedupe_key TEXT NOT NULL UNIQUE,  -- e.g. finished:<child>, send:<run>:<call_id>
  created_at INTEGER NOT NULL,
  consumed_run_id TEXT, consumed_step INTEGER   -- set in the consuming step's transaction
);
CREATE INDEX agent_mail_pending ON agent_mail(to_run_id, consumed_step);
```

`RunRow` and `NewRun` gain the new fields; `Store` gains `children(run_id)`, `child_by_call(run_id, call_id)`,
`root_run(run_id)`, `post_mail(..)` (insert-or-ignore on `dedupe_key`), `pending_mail(run_id)`. Consuming mail
happens inside `Journal::complete`'s transaction: `StepOutput` gains `consumes: Vec<i64>`, alongside `messages` and
`summary_of`. A crash before completion leaves the mail unconsumed, and the re-executed step picks up the same rows.

`list_sessions` excludes `source = 'subagent'` by default; `session_usage` gets a variant that sums a session's run
trees.

### 1.3 Who drives children

Sub-agents must work in every mode that drives runs: the daemon (`Manager`), the embedded TUI (`tui_backend.rs`) and
`tern run`. So the coordinator lives with `Runtime`, not the daemon:

- `crates/tern-core/src/subagent.rs` (new): the `SubagentHost` trait the tools call, request/response types, and
  `SubagentLimits`. No daemon types in core (DECISIONS M1: traits live in core, implementations outside).
- `crates/tern-tools/src/agents.rs` (new): `AgentSpawn`, `AgentWait`, `AgentSend`, `AgentCancel`, `AgentStatus`,
  `AgentAskParent`, each holding an `Arc<dyn SubagentHost>`.
- `crates/tern/src/subagents.rs` (new): `Subagents`, the host implementation. It builds the child's system prompt
  (`prompt::system_prompt` with the sub-agent role, §6), picks the model, creates session + run, and hands the run to a
  `RunDriver`. Late-bound into the tools with a `OnceLock`, the same pattern as `daemon::manager::Routes`, because
  tools are built before the `Agent` exists (`Runtime::open`).
- `RunDriver` (trait in `crates/tern/src/subagents.rs`): `drive(run_id, resume)`, `cancel(run_id)`,
  `is_active(run_id)`. `Manager` implements it in the daemon (so children show in `/v1/runs/active` and obey
  `cancel_run`); a small task-spawning implementation serves the TUI and `tern run`.

`Manager::spawn` delivers results to gateways; it must skip runs with a `parent_run_id` (children never deliver).

### 1.4 Crash recovery and resume

Every piece is already journaled or re-derivable:

1. **Spawn.** `agent_spawn` is `idempotent: true`. Executing it looks up `child_by_call(parent, call_id)` first, so
   re-executing a step a crash left `started` returns the existing child instead of creating a second one. Replaying a
   completed step returns the stored handle without touching anything.
2. **Children.** A child is an ordinary `running` run, so `triage_running_runs` (crates/tern-core/src/agent.rs)
   resumes it at daemon start, like any other.
3. **Waiting.** `agent_wait` and the `inbox` step are `InFlight::Retry`. Re-executed after a crash, they read child
   status and mail from the store, so they return the same kind of answer. Their output is journaled once complete, so
   replay is deterministic even though the live result depended on timing.
4. **Nobody driving a child.** Triage marks runs older than 24 h `interrupted`, and `tern run` has no triage at all.
   So every wait path calls `host.ensure_driven(child)`: if the child is `running` or `interrupted` and
   `driver.is_active` is false, it is resumed. Resuming a parent therefore resumes its subtree lazily, and
   `tern resume <parent>` works in the CLI.
5. **Lost "finished" mail.** A child finishing writes `finished:<child>` mail to its parent after its status. At
   startup the host reconciles: any terminal child without that mail gets it (`dedupe_key` makes this safe to repeat).

The inbox step needs one journal addition. It only exists on the live frontier (when there is mail), so replay must
not consult the live mailbox where the original run had none. `Journal::peek(run, idx) -> Option<StepKind>`: before
an `llm` step, if a step is already recorded at `idx`, replay whatever kind it is; only when nothing is recorded check
mail and, if any, record an `inbox` step first. New `StepKind::Inbox` (`"inbox"`), shown in `runs show` and the
inspector.

### 1.5 Cancellation

- Cancelling a run cancels its whole subtree. `Manager::cancel_run` and `stop_session` call `host.cancel_tree(run)`,
  which walks `children()` recursively and cancels each active one through the driver.
- When a parent run ends for any reason, the host cancels any child still running. With the "no finishing with live
  children" rule (§2.4) this only happens on failure, cancel, or the iteration limit.
- Cancelling a child (from the UI or `agent_cancel`) affects only that subtree; the parent sees
  `status: "cancelled"` with the reason in its next `agent_wait` or inbox message and carries on.
- A child stopped by cancellation is `interrupted` with `error = "cancelled by the user"` or
  `"cancelled by the parent agent: <reason>"`, so a later `ensure_driven` does not resurrect it. Only crash
  interruptions (no error, or the triage message) are resumable that way.

### 1.6 Limits

New `[subagents]` table in `config.toml` (crates/tern-core/src/config.rs; `deny_unknown_fields`, so it must be added
before anyone sets it):

```toml
[subagents]
enabled = true
# Model for sub-agents when agent_spawn doesn't name one. Unset = the parent's model.
default_model = "anthropic:claude-sonnet-4-5"
# Names the parent can pass as `model` (the "per-role models" idea).
models = { fast = "anthropic:claude-haiku-4-5", strong = "anthropic:claude-opus-4-1" }
max_depth = 1             # 1 = children can't spawn; 2 = one level of grandchildren
max_concurrent = 4        # running children per parent run
max_children = 12         # children per parent run, in total
max_concurrent_global = 8 # across the daemon; spawns beyond this queue (status "queued")
max_iterations = 30       # per child, unless agent_spawn asks for fewer
budget_tokens = 400000    # per child: input + output + cache tokens, unless agent_spawn asks for fewer
ask_parent_timeout_secs = 900
```

- **Depth** is stored on the run. `agent_spawn` at the depth limit, and `agent_ask_parent` for top-level runs, are left out of the tool specs
  where they can't be used, so the model never sees them.
- **Concurrency.** Beyond `max_concurrent`, `agent_spawn` still succeeds but the child is `queued` (run row created,
  not driven) and starts when a sibling finishes. The model never has to retry.
- **Budget.** Checked in the agent loop after each `llm` step from journaled usage (the same source compaction uses),
  so replay makes the same decision. Over budget, the run completes with `[stopped: token budget]`, like
  `max_iterations` today. A parent's own budget, if set, counts its children's usage too.
- **Iterations.** `AgentConfig::max_iterations` becomes the default; the run row's `max_iterations` overrides it.

### 1.7 Tools and model a sub-agent gets

- **Model**: `agent_spawn.model` (an alias from `[subagents].models` or a `provider:model` checked with
  `Runtime::check_model`), else `default_model`, else the parent's model. Frozen on the run row as today.
- **Tools**: `agent_spawn.tools` is `"all"` (default), `"read_only"`, or a list of names. `read_only` is `read_file`,
  `list_dir`, `search_files`, `http_fetch`, `memory_search`, `session_search`, `skill_read`, plus MCP/connector tools
  whose policy says no approval. Always removed for children: `schedule_create`, `schedule_delete`,
  `connector_activate` (persistent, user-facing changes belong to the main agent; open question 3). Always added:
  `agent_ask_parent`; `agent_*` only if `depth < max_depth`.
- **Enforcement**: the scope is frozen on the run (`tool_scope`). `Agent::drive_inner` gets the specs from
  `ToolRegistry::specs_for(scope)` (new), and `ToolRegistry::call` refuses an out-of-scope name with an error result,
  so a hallucinated call can't escape.

### 1.8 Approvals inside a sub-agent

A child's tool calls go through the same `approval_step`, the same allowlist and the same `ApprovalHandler`. What
changes is routing and labelling:

- `daemon::manager::Router` finds the target with `store.run_deliver_to(root_run(run_id))` instead of the run itself,
  so a child of a Telegram-started run asks in that Telegram chat. Children never get `deliver_to` themselves.
- `ApprovalRequest` gains `parent_run_id: Option<String>` and `agent_label: Option<String>`. `ApiApproval` gains
  those plus `root_session_id`, so the desktop puts the card in the conversation the user is looking at, labelled
  "research-1 wants to run …". The Telegram gateway prefixes the prompt with the label.
- "Always allow" from a child adds to the global allowlist, as now.
- Concurrent approvals from several children are normal: each is its own pending entry in `ApiApprovals`.
- Children don't add new approval prompts: spawning, waiting and messaging need no approval (`requires_approval:
  false`). A child can only do what an approval or the allowlist already lets a run do.

## 2. Communication

### 2.1 Options considered

| | A. Blocking task tool | **B. Handles + inbox (recommended)** | C. Mailbox / event-driven agents |
|---|---|---|---|
| Shape | `agent_run(task)` blocks until the child finishes and returns its report (Claude Code's Task tool) | `agent_spawn` returns a handle; `agent_wait` / `agent_send` / `agent_cancel`; results also arrive in an inbox step | Agents are peers posting to channels; a run ends and is woken by a new message as a new run |
| Parallel fan-out | Needs concurrent tool execution: `drive_inner` runs a turn's tool calls one by one, with journal indexes assigned in order | Free: several spawns in one turn each return at once, then one `agent_wait` | Free |
| Parent ↔ child mid-run | No | Yes, both directions | Yes |
| Parent works while children run | No | Yes | Yes |
| Journal/replay impact | Small | One new step kind (`inbox`) and a mail table | Large: runs become event-triggered; "one run per session" and run completion semantics change |
| Prompting | Easiest | Moderate (spawn, then wait) | Hardest |

**Recommendation: B.** It gets A's simplicity in the common case (the model emits `agent_spawn` × N and
`agent_wait` in one assistant turn; the tool loop executes them in order and the wait returns all reports) without
changing the sequential tool loop, and it supports questions, steering and doing other work in between. C is closer
to "Claude Duet channels" and Inngest-style event triggers; B's mail table is a step towards it if that is ever
wanted (open question 6).

### 2.2 Tools (parent side)

All are `idempotent: true`, `requires_approval: false`. Results are JSON text.

**`agent_spawn`**: start a sub-agent on a task; returns immediately.

```json
{
  "type": "object",
  "required": ["label", "task"],
  "properties": {
    "label": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]{0,31}$",
      "description": "Short name for this sub-agent, unique among yours, e.g. 'pricing-research'." },
    "task": { "type": "string",
      "description": "The complete brief. The sub-agent sees nothing else: goal, what you already know, constraints, and the exact shape of the report you want." },
    "tools": { "description": "'all' (default), 'read_only', or a list of tool names.",
      "oneOf": [ { "enum": ["all", "read_only"] }, { "type": "array", "items": { "type": "string" } } ] },
    "model": { "type": "string", "description": "A model alias (fast, strong, …) or provider:model. Default: the configured sub-agent model." },
    "max_iterations": { "type": "integer", "minimum": 1 },
    "budget_tokens": { "type": "integer", "minimum": 10000 }
  }
}
```

Returns `{"agent":"pricing-research","run_id":"…","status":"running"|"queued","model":"anthropic:…"}`. Errors
(label taken, limits reached, unknown model) are tool errors the model can fix. A label collision on re-execution
after a crash is not an error: the call id lookup returns the original.

**`agent_wait`**: block until sub-agents finish or ask something.

```json
{
  "type": "object",
  "properties": {
    "agents": { "type": "array", "items": { "type": "string" },
      "description": "Labels to wait for. Default: all of yours that haven't been reported yet." },
    "mode": { "enum": ["all", "any"], "default": "all" },
    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 900 }
  }
}
```

Returns as soon as the condition is met *or any awaited child asks a question* (a blocked child would otherwise
deadlock an `all` wait):

```json
{ "agents": [
  { "agent": "pricing-research", "status": "completed", "result": "…report…",
    "iterations": 9, "tokens": 48210, "run_id": "…" },
  { "agent": "competitor-scan", "status": "question", "question_id": "q_17",
    "question": "Include private companies? I'd default to no." },
  { "agent": "docs-review", "status": "running", "activity": "Running http_fetch", "elapsed_secs": 71 }
] }
```

Statuses: `queued`, `running`, `question`, `completed`, `failed`, `cancelled`, `stopped` (budget or iterations; the
partial result is included). A report is delivered once: delivering it consumes the `finished` mail, so it doesn't
come back through the inbox.

**`agent_send`**: answer a question or redirect a running sub-agent.
`{"agent": string, "message": string, "question_id"?: string}`. Mail with `dedupe_key = send:<parent>:<call_id>`.
The child gets it as the `agent_ask_parent` result (if `question_id` matches) or in its next inbox step.

**`agent_cancel`**: `{"agent": string, "reason"?: string}`. Cancels the subtree; idempotent.

**`agent_status`**: `{}`. Non-blocking list of your sub-agents with status, current activity, elapsed time and
tokens. For checking in between other work without waiting.

### 2.3 Tools (child side)

**`agent_ask_parent`**: `{"question": string, "recommendation"?: string}`. Posts `question` mail to the parent and
blocks until the `answer` arrives, the parent cancels, or `ask_parent_timeout_secs` passes (result: "No answer
within 15 minutes. Proceed with your recommendation and say in your report what you assumed."). Journaled as a tool
step (`InFlight::Retry`; the question's `dedupe_key` is `ask:<child>:<call_id>`, so a re-execution doesn't ask
twice). Children cannot reach the user directly, except through approval prompts.

### 2.4 Delivery into the parent loop

Changes in `Agent::drive_inner` (crates/tern-core/src/agent.rs):

1. **Inbox before each model call.** On the live frontier (§1.4), if `pending_mail(run)` is non-empty, record an
   `inbox` step whose input is the mail ids and whose output appends one user message:
   `[sub-agent pricing-research completed]\n<report>` / `[sub-agent competitor-scan asks (q_17)] …` /
   `[message from the user to this agent] …`. Consumes the mail in the same transaction. The child side uses the same
   mechanism to receive `agent_send` steering messages.
2. **No finishing with live children.** When a turn has no tool calls but the run has children in `queued`,
   `running` or `question`, the loop records an `inbox` step that blocks until there is mail (as `agent_wait` with
   `mode: any`), appends it, and goes round again instead of returning `Completed`. So reports always reach the
   model, and the user's final answer can rely on them. The iteration limit still applies.
3. **Activity while waiting.** `agent_wait` and the blocking inbox step are long-running steps. Their progress sink
   (`tool::progress_sink`) emits short lines ("pricing-research completed") so the TUI and the app can show progress
   without new plumbing.

## 3. Events and API

### 3.1 `AgentEvent` (crates/tern-core/src/events.rs)

Additive only, so the Dart parser's `UnknownEvent` fallback keeps old apps working.

```rust
RunStarted {
    run_id: String,
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_run_id: Option<String>,
},
/// Emitted on the parent's run when agent_spawn creates (not replays) a child.
SubagentSpawned {
    run_id: String,            // parent
    call_id: String,           // the agent_spawn tool call, ties the card to the work step
    child_run_id: String,
    child_session_id: String,
    label: String,
    task: String,              // first 500 chars of the brief
    model: String,
    status: String,            // running | queued
},
/// A question, answer or steering message between runs of one tree.
SubagentMessage {
    run_id: String,            // the run that sent it
    to_run_id: String,
    kind: String,              // question | answer | message
    text: String,
},
```

Child runs emit all the usual events (`text_delta`, `tool_started`, …) under their own `run_id`. The SSE layer
(`with_session` in crates/tern/src/daemon/api.rs) already adds `session_id`; it also adds `parent_run_id` and
`root_session_id` for runs that have a parent, cached per connection like the session lookup. That lets a client
place any child event without having seen `subagent_spawned` (attaching mid-run). Queued children emit
`run_started` when they actually start.

Embedded front ends (TUI) get the raw `AgentEvent`s and use `SubagentSpawned` to learn the tree.

### 3.2 Endpoints

| Method | Path | Change |
|---|---|---|
| GET | `/v1/runs/{id}/children[?recursive=1]` | new: `[ChildRun]` |
| GET | `/v1/sessions/{id}/subagents` | new: every `ChildRun` under the session's runs (resync on open) |
| POST | `/v1/runs/{id}/message` | new (S3): `{text}` from the user to a running child, as `message` mail |
| POST | `/v1/runs/{id}/cancel` | cascades to the subtree (§1.5) |
| POST | `/v1/runs/{id}/resume` | on a parent, resumes lazily through `ensure_driven` |
| GET | `/v1/runs/active` | `ActiveRun` gains `parent_run_id`, `label` |
| GET | `/v1/sessions` | excludes `source = subagent` unless `?source=subagent` |
| GET | `/v1/sessions/{id}` | works unchanged for a child session (its transcript) |
| GET | `/v1/sessions/{id}/runs[?include=subagents]` | `RunItem` gains `parent_run_id`, `label`, `depth`; the flag adds descendant runs |
| GET | `/v1/sessions/{id}/usage[?include=subagents]` | sums the run trees |
| GET | `/v1/approvals` and the `approval_requested` event | `ApiApproval` gains `parent_run_id`, `agent_label`, `root_session_id` |
| GET | `/v1/health` | capability flag `subagents` (gui-spec §7.3) |

```rust
pub struct ChildRun {
    pub run_id: String,
    pub session_id: String,
    pub parent_run_id: String,
    pub call_id: String,
    pub label: String,
    pub task: String,              // first 500 chars
    pub model: Option<String>,
    pub depth: u32,
    pub status: String,            // queued | running | question | completed | failed | interrupted
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub usage: Usage,
    pub result_preview: Option<String>,   // first 300 chars of the final text
    pub pending_question: Option<String>,
}
```

New routes go in a new `crates/tern/src/daemon/api_subagents.rs`, merged in `api::router` like `api_runs`. The Rust
fixture test that writes every API type for `tern_client` (gui-spec §6.4) gains `ChildRun`, the new events and the
extended `ApiApproval`.

`tern runs list` hides children (count shown per parent); `tern runs show <id>` lists children under the
`agent_spawn` step that made them and prints `inbox` steps (crates/tern/src/inspect.rs).

## 4. Desktop app (apps/desktop)

### 4.1 State

- `tern_client`: `SubagentSpawned`, `SubagentMessage` events; `parentRunId` on `RunStarted`; `parentRunId` /
  `rootSessionId` read from every `RunEvent` (an `Expando` like `_sessions`); `ChildRun`; `client.subagents(session)`,
  `client.children(run)`, `client.messageRun(run, text)`; new `ApiApproval` fields.
- `TernStore` (lib/state/store.dart): child events already route to `transcripts[childSessionId]` through
  `_runSession`, so the child's own transcript builds itself with the existing reducer. New maps
  `_parentOf[childRun]` and `children[parentRun] -> List<ChildRun>`, filled from `subagent_spawned`, event enrichment
  and `GET /v1/sessions/{id}/subagents` on open and resync. Approvals route by `rootSessionId ?? sessionId`. Unread
  dots and notifications ignore child sessions (they're hidden from the sidebar anyway).
- `Transcript` (lib/state/transcript.dart): a new `AgentsItem` holding the children spawned in one parent turn
  (`List<AgentChild>`: label, runId, sessionId, task, model, status, started/finished, tokens, result). Built from
  `ToolStarted(name: agent_spawn)` + `SubagentSpawned` live, and from `ToolUseBlock(agent_spawn)` + the subagents
  list in `fromSession`. `agent_*` tool steps are not added to `WorkItem`s; the card replaces them. `RunPhase` gains
  `waiting(int running, int done)` ("Waiting on 3 agents · 1 done"), used while the parent's running step is
  `agent_wait` or a blocking inbox step.

### 4.2 Conversation: the agents card

One card per fan-out, in the same visual family as `WorkCard` (12 px radius, `c.card`, hairline):

```
┌ Delegated to 3 agents · 1:42 ──────────────── Inspect ┐
│ ✓ pricing-research   sonnet · 9 steps · 48k   1:12  ›  │
│ ◌ competitor-scan    Running http_fetch …      1:42  ›  │
│ ? docs-review        Asked: "Include drafts?"  0:55  ›  │
└────────────────────────────────────────────────────────┘
```

- Each row: status glyph (spinner, ✓, ✗, `?` amber for a question, ⏸ queued), label, live activity from the child
  transcript's `phase.label` (reusing `ActivityLine`'s logic) or the result's first line when done, elapsed, model
  and token count on hover.
- Clicking a row expands it inline: the brief (collapsed after 3 lines), then the child's transcript rendered with
  `ItemView` at reduced density (its `WorkCard`s, approvals, final message), capped at 480 px with "Open full
  transcript" (a sheet showing the child session) and "Inspect run". The expansion stays while the child runs, so
  the user can watch it.
- Row actions (hover menu): Stop (`/cancel`), Message… (`POST /v1/runs/{id}/message`, S3).
- A question from a child shows as a line under its row ("Asked the main agent: …", then "Answered: …") from
  `SubagentMessage`. These are agent-to-agent; the user isn't asked to reply, but can see the exchange.
- An approval raised by a child appears as a normal `ApprovalCard` in the parent conversation with a chip
  "from pricing-research", and the child's row shows "Waiting for approval". Notification title: "pricing-research
  wants to run shell".
- The parent's `WorkCard`s and the agents card interleave in order; when children report back through the inbox,
  a quiet pill "pricing-research reported back" marks the spot (new `PillKind.agent`).
- `ActivityLine` under the newest item shows "Waiting on 2 agents · 1 done · 1:42".

### 4.3 Side panel: Activity NOW

Below the running parent's steps (lib/ui/side_panel.dart), an indented **Agents** list for the current run's
children, one line each: status icon, label, phase label ("Running cargo test", "Waiting for approval", "Done"),
elapsed. Clicking opens the run inspector on that child. Parent steps hide `agent_*` tools as in the transcript.
USAGE adds a "Sub-agents" line (from `usage?include=subagents`) when there are any.

### 4.4 Run inspector

- The run list (`_RunRow` in lib/ui/inspector.dart) loads `runs?include=subagents` and shows the tree: children
  indented under their parent with their label and status.
- In a parent's timeline, an `agent_spawn` tool step gets "Open run →" (child run id from its output), and `inbox`
  steps render as "Inbox: pricing-research completed / competitor-scan asked …" with the mail bodies.
- In a child's timeline, a header shows "Sub-agent of run 3f2a · pricing-research" with a link back, the brief, the
  tool scope, model and budget.
- The live refresh (`_live`) also treats child runs as live.

## 5. TUI (crates/tern-tui)

The TUI today applies every `AgentEvent` to the transcript without checking `run_id` (`App::on_agent_event` in
crates/tern-tui/src/app.rs). With concurrent children that would interleave their text into the parent's, so this
filter is the first TUI change and is needed even before any display work.

- `App` tracks `tree: HashMap<String, ChildState>` (child run id → label, state, activity, tool count, tokens,
  started), filled from `SubagentSpawned`. Events for the running run apply as today; events for a run in `tree`
  update its `ChildState` only; others are ignored.
- New `Entry::Agent { run_id, label, state, activity, tools, started }`, one line per child, updated in place:
  `⑂ pricing-research  Running shell cargo test · 3 tools · 0:42`, finishing as
  `✓ pricing-research  1:12 · 9 tools · 48k tok → first line of the report`. `?` and amber for a pending question,
  `!` for a pending approval.
- `agent_*` calls don't produce `Entry::Tool` lines; the spinner text reads "waiting on 2 agents".
- Approvals from a child show the label in the prompt: `[pricing-research] shell: git push … — (y)es / (a)lways /
  (n)o`.
- Commands: `/agents` lists the last run's children with status and run ids; `/agent <label>` opens the child's
  transcript in a read-only overlay (Esc closes), via `Backend::session`; `/agent <label> <message>` steers it (S3).
  Ctrl+C cancels the whole tree, as it cancels the run today.
- `Backend` (crates/tern-tui/src/lib.rs) gains `children(run_id)` and `message_run(run_id, text)`, implemented in
  `tui_backend.rs` (store + host) and `remote.rs` (the new endpoints).
- `tern run` prints child progress to stderr as `[pricing-research] ✓ read_file src/main.rs`, and the final answer
  on stdout as today.

## 6. System prompt

The prompt is assembled in `crates/tern-core/src/prompt.rs` (`system_prompt`), with the persona from
`crates/tern-core/assets/persona.md` (or `~/.tern/persona.md`), and frozen on the run row (`Runtime::start_run`), so
in-flight runs keep their old prompt when this changes. `PromptInputs` gains:

```rust
pub enum AgentRole<'a> {
    Main { sub: Option<SubagentPromptInfo> },          // None = sub-agents disabled
    Subagent { label: &'a str, can_spawn: bool, sub: Option<SubagentPromptInfo> },
}
pub struct SubagentPromptInfo { pub max_concurrent: u32, pub models: Vec<String>, pub can_nest: bool }
```

The **Sub-agents** section goes after Workspace and before Skills, for the main agent (and for children that may
spawn). The **You are a sub-agent** section goes right after the persona for children. Exact text (`{…}` filled
from config):

```markdown
## Sub-agents

You can delegate work to sub-agents: separate instances of you that run in parallel, each with a fresh context, the
same workspace and, unless you limit them, the same tools. Delegating well is one of the most useful things you do.
It finishes independent work in parallel, keeps your own context free of detail you only need the conclusion of, and
gives you independent checks. You coordinate; they do the legwork.

Delegate by default when:
- the work splits into parts that don't depend on each other (several companies, files, repos, sources or
  approaches): spawn one sub-agent per part, all in the same turn;
- a part needs a lot of reading, searching or trial and error, and you only need what it found;
- a step would flood your context (long logs, large files, many web pages);
- you want a second opinion on your own work: a fresh reviewer who hasn't seen your reasoning.
Do it yourself when the task takes only a few tool calls, when each step depends on the one before, or when it needs
the conversation with the user that only you have. Never delegate talking to the user.

Fanning out:
- Call `agent_spawn` once per part, then `agent_wait`, all in one turn. Up to {max_concurrent} run at once; more
  are queued.
- Split by outcome, not by tool. Give each sub-agent a distinct slice, and say what is out of its scope and who
  covers it, so no two do the same work.
- Give `tools: "read_only"` for research and review. Give write access only to a sub-agent that must change files,
  and never let two of them write the same files.
- Pick `model` for the job: {models}. A light model is fine for lookups and summaries; use a strong one for hard
  reasoning and code.

Briefing: a sub-agent knows only what you put in `task`. Write it like a handoff to a capable colleague who has just
walked in:
- the goal, why it matters, and what done looks like;
- what you already know or have ruled out, with exact paths, URLs, names and numbers, so it doesn't rediscover them;
- constraints: what not to touch, what to avoid that would need the user's approval, how far to go;
- the exact shape of the report you want back, for example "a table of name, URL, stage and one line of evidence,
  under 300 words; list what you couldn't verify".
Give your hypotheses as things to confirm or refute, not as facts.

While they work:
- Do other useful work if there is any, but never the same work they are doing. Their reports arrive through
  `agent_wait` or as messages before your next step.
- A sub-agent that asks you a question is blocked until you answer. Answer at once with `agent_send`.
- Redirect one that is off track with `agent_send`; stop one you no longer need with `agent_cancel`.

When reports come back:
- Treat each report as a claim, not a fact. Before you act on it or tell the user, check what matters: open the file
  it changed, rerun the test, look at the source it cites. A confident tone is not evidence.
- If reports conflict, find out why instead of picking one. Spawn a focused follow-up if you need to.
- If a sub-agent failed or ran out of budget, retry it with a better brief, take that part over yourself, or tell
  the user what is missing.
- Write one answer in your own words. The user has not seen the reports, so don't refer to them as if they had, and
  say what you checked and what you didn't.

Sub-agents can't talk to the user except through approval prompts, which the user sees with the sub-agent's name.
{can_nest ? "They can start sub-agents of their own, one level deep." : "They can't start sub-agents of their own."}
```

```markdown
## You are a sub-agent

The main tern agent started you, as "{label}", to do one task, described in the first message. The user doesn't see
this conversation. Your final message is returned to the main agent as your report.

- Stay within the task. If you notice something important outside it, mention it in your report rather than acting
  on it.
- Work on your own. Ask the main agent with `agent_ask_parent` only when you are blocked on a decision that is
  really its call: one clear question, with the answer you'd choose. Otherwise make a sensible assumption and state
  it in your report.
- Messages that start with "[message from the main agent]" are new instructions from it; follow them.
- Don't ask the user anything. Don't save memories, create routines or change connectors unless the task says to.
- Check your work before you finish: rerun the test, re-read the file, confirm the source.
- Your final message is the report. Lead with the answer in the shape the brief asked for, then what you checked,
  what you assumed, and anything left undone. Use exact paths, links and figures. No preamble, and no
  "let me know if…".
```

The section is only included when `[subagents].enabled` and the tools are registered, so a disabled config produces
today's prompt byte for byte. `persona.md` stays as is; its "Prefer doing over describing" line already fits.

## 7. Implementation plan

Each milestone ends shippable and tested, in the style of the M and G milestones. Test helpers: the mock provider
picks its reply by the number of assistant turns in the request (DECISIONS M1), so a child would get the parent's
script. S1 adds script selection by a marker in the first user message (`"when_task_contains"`), so one script file
can drive a parent and its children.

| Milestone | Scope | Tests |
|---|---|---|
| **S0: child runs** | migration 0006; `RunRow` fields; `Store` child/mail queries; `StepKind::Inbox`, `Journal::peek`, `StepOutput::consumes`; `subagent.rs` trait; `Subagents` host + `RunDriver` (Manager and embedded); tool scope in `ToolRegistry::specs_for`; per-run `max_iterations` and token budget in `drive_inner`; `[subagents]` config | core: child run with scoped tools; budget stop is replay-stable; inbox step replays without consulting mail; `tern doctor` accepts the config |
| **S1: spawn and wait** | `agent_spawn`, `agent_wait`, `agent_cancel`, `agent_status`; queueing; "no finishing with live children"; cancel cascade; `ensure_driven`; startup reconcile; Manager skips delivery for children; Router walks to the root; `SubagentSpawned` and `RunStarted.parent_run_id`; `runs show` tree | e2e (`crates/tern/tests/s1.rs`): fan-out of 3 with the mock; `TERN_CRASH_AFTER_STEP` during spawn, during wait, and in a child, then daemon restart: no duplicate children, same final answer; `tern resume` of a parent in the CLI resumes its children; cancel parent cancels children |
| **S2: API and TUI** | `api_subagents.rs` endpoints; SSE enrichment; `ApiApproval` fields; session filtering; usage including sub-agents; health flag; TUI run-id filtering, `Entry::Agent`, `/agents`, `/agent`; `tern run` progress | API tests; fixtures for `tern_client`; TUI reducer tests (child events don't touch the parent transcript; approval label) |
| **S3: two-way messaging** | `agent_send`, `agent_ask_parent`, inbox delivery to children, ask timeout, `SubagentMessage`, `POST /v1/runs/{id}/message`, `/agent <label> <msg>` | e2e: a child asks, the parent's `agent_wait` returns the question, the parent answers, the child finishes; crash between ask and answer doesn't ask twice |
| **S4: desktop** | `tern_client` models; store maps; `AgentsItem` and the agents card with inline transcript; NOW section agents list; inspector tree and links; approval chip and notification text; `RunPhase.waiting` | transcript reduction tests (events in, items out) including resync from `/subagents`; widget tests for the card; fixture parsing |
| **S5: prompt** | §6 sections in `prompt.rs`; `AgentRole` in `PromptInputs`; flip `[subagents].enabled` default to true | prompt unit tests (sections present/absent per role and config); a short real-provider eval set (`#[ignore]`d like M1's): a fan-out task, a single quick lookup (must *not* delegate), a review-my-work task, measured on spawn count, duplicate work and verification behaviour |

`enabled` defaults to `false` until S5, so S0–S4 can ship without the model using sub-agents unprompted; the user can
turn it on early to try them.

## 8. Open questions

1. **Depth.** Default `max_depth = 1` (children can't spawn). Do you want grandchildren from the start, for example a
   research child that fans out per source?
2. **Children outliving the parent's turn.** As designed, a parent run can't finish while children run, so the main
   agent can't ask *you* something mid-fan-out without cancelling them. Alternative: "detached" children that keep
   running after the turn ends and report into the session's next run. More useful for long jobs (overnight
   research), but it changes "one run per session" semantics. Worth it now, or later?
3. **What children may change.** Proposed: no `schedule_*` or `connector_activate`; memory saves allowed only if the
   brief says so (prompt-level, not enforced). Should memory saves be blocked outright?
4. **Models.** Is `[subagents].models` with aliases (`fast`, `strong`) the right home for the "per-role models"
   idea, or should that wait for agent profiles (gui-spec §7.2), where a sub-agent could be "spawn agent profile X"?
5. **Budgets.** Are token budgets the right limit, or do you want a cost limit in money (needs a price table per
   model, which tern doesn't have)?
6. **"Claude Duet".** If Duet-style channels turn out to mean agents posting into shared topic streams, option C
   (§2.1) is the bridge. Is that the intent, so the mail table should be designed as a general channel now?
7. **User steering.** Is messaging a running child from the UI (S3) wanted, or should the user only ever talk to the
   main agent?
8. **Telegram.** Child approvals go to the root run's chat with the label. Should Telegram also get progress
   messages ("3 agents working…"), or stay quiet until the final answer?

## Implementation notes (S0–S1)

Where the built code differs from the design above (details in DECISIONS.md "S0–S1"):

- Tools get the call id and mark mail consumed through a task-local (`tool::with_call`, `current_call_id`,
  `consume_mail`), next to `progress_sink`, rather than through `ToolCtx`.
- The `inbox` step's input is `{"wait": bool}`; the mail ids are in its output (`InboxOutput`), so replay hashes
  don't depend on the live mailbox.
- A child counts as done when its `finished:<child>` mail exists. Queued children have run status `queued`.
- Limits are checked after a turn's tool calls (where `max_iterations` was), and a parent's budget counts only its own
  journaled usage, not its children's (not replay-stable otherwise).
- `read_only` is the fixed built-in list; MCP/connector tools aren't added by policy.
- A child's system prompt is the parent's frozen prompt until S5.
- Cancel reasons: `cancelled by the user`, `cancelled by the parent agent[: reason]`, `cancelled: the parent run
  ended`.
- `when_task_contains` is an alias of the mock's existing `conversations[].matches`.
- `tern runs list` hiding children (§3.2) landed in S1. The blocking inbox step emits no progress lines.

## Implementation notes (S2–S3)

Details in DECISIONS.md "S2–S3".

- `ChildRun` lives in `tern_core::subagent` (the TUI's `Backend::children` returns it too) and is built from the store
  by `subagents::child_run`, so it works with sub-agents disabled or for a child another process drives. Its status
  adds `stopped` and `cancelled` to §3.2's list: `cancelled` for a child interrupted on purpose, `interrupted` only for
  one a crash cut off. `started_at`/`finished_at` are Unix seconds (other API times are milliseconds).
- A child's question is pending while its newest journal step is a started `agent_ask_parent` call and no answer to it
  is waiting; that drives `status: "question"`, `pending_question` and `agent_wait` returning early.
- `agent_send` to a child blocked on a question is always the answer: `question_id` is optional, and a wrong one is an
  error. Sent to a child that isn't asking, it is a `message`, or a late `answer` when it names a question.
- The question id is `q_<mail id>`. The ask timeout counts from when the question was first posted, so a re-execution
  after a crash doesn't restart it; the timeout result is JSON with `answer: null` and the §2.3 note.
- Questions delivered by `agent_wait` (or answered by `agent_send` before the inbox saw them) are consumed there, so
  they don't reach the inbox as well.
- `POST /v1/runs/{id}/message` emits no `subagent_message`: the event's `run_id` is the sender, and the user isn't a
  run. Steering text renders as `[message from the main agent]` (from the parent) or `[message from the user to this
  agent]`.
- `agent_ask_parent` is hidden from top-level runs by `tool::in_scope` (a `None` scope means "all but child-only
  tools") and always added to a child's frozen scope.
- `/v1/health` reports `subagents` whether or not `[subagents].enabled` is set: it says the API exists.
- The cancel cascade on `/v1/runs/{id}/cancel` was already in S1 (a parent's drive ending cancels its children).
- The `tern_client` fixture test (gui-spec §6.4) doesn't exist on the Rust side yet, so S2 added none.
