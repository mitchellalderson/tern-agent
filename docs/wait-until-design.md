# Wait until

An agent mid-task can say "watch this and continue this conversation when it's done": a GitHub Actions run, a
deploy, any check. tern polls it with no model call and, when it matches (or time runs out), gives the agent one new
turn in the same conversation.

## Why not a watch

A [watch](reference.md#routines-triggers-watches-gates-initiative) is a routine trigger: recurring, many events, each
starting a run in the routine's own session, until someone turns it off. A wait is the opposite on every point: tied
to the conversation that made it, one event, ends by itself. So it is its own small thing (`waits` table, three tools)
and reuses what is already there: connector sandboxes, the checked public fetch, taint, the ledger, the scheduler's tick.

## The tools

`wait_until` · `wait_list` · `wait_cancel`. Not available to sub-agents.

```json
{
  "note": "tell Mitch whether CI passed",
  "check": { "connector": "github", "tool": "run_status", "args": { "owner": "o", "repo": "r", "run_id": 123 } },
  "until": { "path": "status", "in": ["completed"] },
  "every": "1m",
  "timeout": "1h"
}
```

| Field | |
|---|---|
| `check` | One of `{connector, tool, args}` (a **read** tool of an active connector), `{url}` (a public JSON address, no credentials), or `{in: "20m"}` (check back after a delay; no `until`). |
| `until` | `{path, equals}` or `{path, in: [...]}`. `path` is dotted into the JSON result (`workflow_run.status`, `jobs.0.status`); empty means the whole result. Numbers, strings and booleans compare as written (`42` matches `"42"`). A missing field never matches. |
| `every` | Gap between checks. At least 30s, default 1m. Never more than the timeout. |
| `timeout` | How long to wait. Default 1h, at most 24h. |
| `note` | A line to the agent's future self: what this is and what to do next. Included in the continuation. |

## What the daemon does

On the scheduler tick it checks each due wait:

1. **Check.** A connector read runs in the connector's sandbox: its hosts, secrets and limits, GET only, active and
   approved connectors only. A URL goes through the same public-address check as `http_fetch` (loopback, private and
   link-local refused, every hop pinned), no credentials, 256 KB, JSON or text. **No model is called.**
2. **Decide.** The condition matches: the wait ends. Past the timeout: it ends as timed out (after one last check). A
   failed check is recorded ("check failed: …") and tried again at the next interval.
3. **Continue.** One run starts in the same session, same agent, trigger `wait`. Its input is the note, how it
   ended, and the result: framed as `<untrusted source="wait:github.run_status">` if it came from outside, in which
   case the run is tainted. The answer is delivered where the conversation is (a topic channel, a DM, Home, or the
   chat it was made in). The wait row is deleted.

A busy conversation is never interrupted: the continuation starts on the first tick after its run ends. Waits live in
the database, so a restart loses nothing; one that expired while the daemon was down ends on the first tick.

## Seen and stopped

- **Tasks** (`GET /v1/tasks`, Library → Tasks) lists each as "Waiting: <note>", under Scheduled, with what it waits
  for, its last check and when it gives up, and a Cancel button.
- **In the conversation** a quiet line under the messages says "Waiting: <note>" with a Cancel link while one is active.
- **API**: `GET /v1/waits[?session=][&agent=]`, `DELETE /v1/waits/{id}`. A chat device may cancel.
- **Agent**: `wait_list`, `wait_cancel`.

## Limits and approval

- `[schedules] max_waits` (default 5) active waits per agent.
- `[schedules] runs_per_day` also caps an agent's continuations per day; one past it is dropped with a notice.
  `tokens_per_run` applies to a continuation's run.
- The ledger records `wait.create`, `wait.fire` and `wait.cancel`.
- Creating a wait doesn't ask in an untainted run and asks in a tainted one. See DECISIONS.md "Wait until".

## GitHub Actions

See [recipes/github-actions.md](recipes/github-actions.md): a small connector with a `run_status` read tool, and the
`wait_until` call that uses it.
