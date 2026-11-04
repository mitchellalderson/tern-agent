# tern personal-agent tooling: routines, channels, connector platform, safety

Status: draft, 2026-11-03. Not built. This is the next set of tooling after S0–S1, D1 and P0. It was derived from a
survey of Claude Duet, Grok Bot, Meta Muse, ChatGPT, Google CC, Poke, OpenClaw and its forks
([research/personal-agents-2026.md](research/personal-agents-2026.md)). Phases are T0–T8 by area (§12.2), built in five stages from quick wins to the hardest integrations (§12.1).

Related:
- [subagents-design.md](subagents-design.md): goals (§6) build on sub-agent runs.
- [deployment-spec.md](deployment-spec.md): webhooks and push (§3.4) need a reachable core.
- [gui-spec.md](gui-spec.md) §3: this resolves the open "Claude Duet channels" question.
- [research/inngest-tie-in.md](research/inngest-tie-in.md): triggers are where V2 plugs in.

## TL;DR

The category has converged. In late 2026 a personal agent is:
- **triggers** (cron plus events) feeding
- **durable background runs**,
- that report into **organized threads** (channels, a Tasks view)
- through **messaging the user already uses**,
- with **approvals for anything that writes**.

The hosted players run all of this on a cloud VM per user, at $20–200 a month. We do the same with one static binary,
SQLite and short Luau scripts. Nine additions get us there; none adds a mandatory dependency:

1. **Routines** (T1). Schedules become routines with more kinds of trigger: cron, connector **watches**, webhooks,
   gateway messages, and system events. A watch is a Luau function that polls with a cursor and returns events.
   **No events means no model call**, which answers OpenClaw's biggest complaint (cost). Each event starts an ordinary
   journaled run.
2. **Quiet delivery** (T1). A run can end with `NO_REPLY`. Each routine has an **initiative level** (silent, digest,
   notify, interrupt) and quiet hours. A cheap model decides whether a result is worth a ping.
3. **Channels** (T2). This is the Duet idea: named topic threads over **one shared memory**. A "home" channel gets
   only "done" and "needs you" posts. Routines and sub-agents post into a target channel. In Telegram a channel maps to
   a forum topic, in the desktop app to the sidebar, in the TUI to `/channel`.
4. **Effects, the outbox and deferred approvals** (T3).
   - Every tool declares an effect: `read`, `write` or `destructive`.
   - Policy is enforced by the host on that effect, never by the model. This is Muse's Sentinel without a second
     agent.
   - A write that needs approval while nobody is around goes into a journaled **outbox**, and the run carries on
     (Duet's "bring the approval back later"). It runs exactly once when approved.
5. **Taint** (T3). A run started by external content (an email, a webhook, a scraped page) is **tainted**. In a
   tainted run, `write` and `destructive` effects always ask. This is our main answer to prompt injection, and the
   hosted agents don't have it.
6. **Connector platform v2** (T4).
   - **Credentials are injected at the host boundary**, so scripts never see secret values.
   - **OAuth first**: when connecting anything, the host checks built-in profiles and `.well-known` metadata (the MCP
     authorization flow included), and the agent researches the docs only if that finds nothing. Sign-in is the
     default when it's usable; API keys through `secret_request` stay as the fallback. Endpoints found by research
     are validated against the service's domain.
   - **Several accounts per service**: Gmail · work and Gmail · personal are separate **connections** of one
     connector or MCP server, each with its own tokens, cache, grants and watches. Writes must name the account.
   - An **OAuth helper** (PKCE on a loopback port, refresh handled by the host).
   - Path-level HTTP allowlists and per-connector rate limits.
   - **Gateway connectors**: a new chat channel such as Slack, Discord, ntfy or WhatsApp is a Luau file, not a Rust
     crate.
   - **Signed bundles** (connector plus skill plus routine), installed from a URL with the hash pinned. No open
     registry: ClawHavoc showed where that leads.
7. **Memory and skills that improve themselves** (T5). A memory flush before compaction, capped USER/MEMORY files,
   and an offer to save a skill after a complex run (Hermes, Grok Bot's "teach a task").
8. **The rest, cheaply** (T6–T7).
   - Briefing, follow-up and meeting-prep **recipes** (skill plus routine; no special feature).
   - **Goals** (a long-lived sub-agent with check-ins).
   - An **activity ledger** (a journal query, hash-chained).
   - **Budgets per routine**.
   - **`tern mcp serve`**.
   - **Browser** through an external MCP first, native CDP later behind a cargo feature. Chromium is never bundled.
9. **A menu-bar tray that outlives the window** (T8). `tern tray` is a tiny native process, a few MB with no
   Flutter. It shows status, "needs you" items and approvals, and posts actionable notifications. The daemon runs
   under launchd or systemd. Closing the desktop app closes only the window; the agent, its routines and approvals
   keep working. **Turning tern off is one click** ("Quit tern" in the tray or the app), with plain-language
   confirmation and automatic escalation if it hangs. No terminal is ever needed, and the service manager never
   resurrects a tern the user quit.

**What we won't do:** a per-user VM, bundled Chromium, OS-level vision clicking, an open skill marketplace,
iMessage (it needs Apple Business approval and pays Apple per user), or human concierges.

## 1. Ethos constraints (they decide every trade-off below)

| Constraint | Concretely |
|---|---|
| **Native** | One static binary. New capability goes into Rust only when it is a primitive (a trigger kind, an effect check, a transport). Integrations go into Luau or MCP. |
| **Lightweight** | Tier 1 install stays as it is: no new listener, no database server, no required daemon features. Release binary < 25 MB, idle RSS < 30 MB (today 14 MB / 16 MB). New crates go behind features unless they are tiny. |
| **Fast** | A watch with nothing to report costs one Luau call and zero tokens. A notification gate uses the cheapest configured model. Nothing resends full history. |
| **Hostable core** | Everything works on a remote core (D1). Webhooks and always-on watches are where the hosted tier earns its money (§3.4). |
| **Connectors are sandboxed Luau** | If an integration can be done over HTTPS, it is a Luau connector. MCP stays first-class for whatever already exists. |

## 2. Wargame

The scenarios that shaped this spec. Each one names the attack or failure, what tern does today, and the defence that
goes in.

| # | Scenario | Today | Defence | Section |
|---|---|---|---|---|
| W1 | **Inbox prompt injection.** An email tells the agent to forward the last 10 invoices to an attacker. The email watcher starts a run, and the agent calls `gmail.send`. | No watchers yet. Once there are, approval would depend on whether `gmail.send` was marked `requires_approval`. | The run is **tainted** because it was started by external content, so `write` asks no matter what the tool's flag or any "always" grant says. Untrusted content is wrapped in delimiters with a provenance note. The approval prompt says the run was triggered by an email from X. | §5.3 |
| W2 | **Malicious bundle.** A shared "Notion recipe" declares `*.notion.so`, but its code posts your secrets to a lookalike host. | Connectors can read secret values with `ctx.secret`. | Credentials are injected by the host (`auth = "NOTION_TOKEN"`) and the script never sees the value. Host and path allowlists. Install shows a capability diff. Bundles are signed and hash-pinned. `ctx.secret` stays for legacy, flagged in `tern connectors list` and in `tern security audit`. | §6.1, §6.5 |
| W3 | **Cost runaway.** A watch fires on every new Slack message, including the agent's own replies: an infinite loop at $X per run. | Not possible yet. | Events from tern's own actions carry `origin = self` and are dropped by default. Each routine has a rate limit, a daily run cap and a **budget**; when exceeded it pauses and posts once to home. A watch with no events costs zero tokens. | §3.2, §8.3 |
| W4 | **Approval when nobody is looking.** The 2 a.m. routine wants to send a reply. | With no client attached, the answer is deny, and the run fails or works around it. | The write goes to the **outbox** and the run continues and finishes. In the morning a "1 action waiting" card appears in home or Telegram; approve, and it runs exactly once. Outbox items expire (default 72 h). | §5.2 |
| W5 | **Laptop asleep.** Events are missed. | Cron collapses missed runs into one. | Watches are cursor-based, so they catch up on wake (bounded by `max_catchup`). Webhooks need a reachable core: free on a self-hosted server core, paid on tern Cloud. The UI says plainly "this routine needs an always-on core". | §3.4 |
| W6 | **Exposed core** (OpenClaw's 80k+ open gateways, CVE-2026-25253). | D1: TLS, device tokens, pairing, scopes. Loopback/Unix by default. | Webhook endpoints use per-routine secrets and never accept device tokens. Any future WebSocket checks `Origin`. `tern security audit` lists listeners, scopes, tainted-capable routines, legacy `ctx.secret` users and unsigned bundles. | §8.4 |
| W7 | **Approval fatigue.** The user says "always" to everything, and taint then means nothing. | `Always (<pattern>)` grants. | Grants are scoped (to this routine, this channel, or everywhere) and **never cover tainted runs** of `write`/`destructive` effects. That one rule is not configurable, except by `autonomy = "yolo"`, which `doctor` shows in red. | §5.4 |
| W8 | **Notification spam**, the way users drop proactive agents (Lenny's "forgot to use it" is the opposite failure). | Every scheduled run is delivered. | Initiative levels, `NO_REPLY`, digests, quiet hours, and a gate model that answers only "worth interrupting? yes/no". Defaults are conservative: routines start at `digest`. | §4 |
| W10 | **Phishing OAuth endpoint.** A poisoned search result or doc page gives the agent a lookalike authorize URL, and the user "signs in" to an attacker. | Not possible yet (no OAuth). | Discovery prefers built-in profiles and `.well-known` metadata. Researched endpoints must share the API's registrable domain or belong to a known identity provider, and are confirmed against issuer metadata. Unverified plans are labelled as such on the card. The browser leg shows the real URL in the user's own browser. | §6.7.2 |
| W11 | **Wrong account.** With work and personal Gmail connected, the agent sends a reply from personal. | One account per connector. | Writes with more than one connection must name it explicitly. Channels and routines pin a default. Approval cards and the ledger name the account. Grants are per connection. | §6.8.2 |
| W9 | **Competitor moves.** Duet adds a desktop app; Grok Bot adds a local mode. | | The moat isn't any single feature. It is **local plus durable plus self-written sandboxed connectors plus small**. Keep the binary small, keep Luau connectors trivial to write, and get to "describe an API, get a working connector in a minute" (§6.6). |  |

## 3. Routines (T1)

### 3.1 Model

Schedules become one kind of **routine**. Migration: every `schedules` row becomes a routine with a `cron` trigger,
and the `schedule_*` tools stay as aliases.

```toml
# Routines are stored in the database and created by the agent or the UI; this TOML form is for bundles (§6.5).
[[routine]]
name = "gh-review-requests"
channel = "work"                     # §4
initiative = "notify"                # silent | digest | notify | interrupt
prompt = "A PR review was requested of me. Summarize the PR and tell me if it's urgent."
trigger = { watch = "github.review_requests", every = "5m" }
budget = { runs_per_day = 40, usd_per_day = 1.00 }
model = "openrouter:cheap-model"     # optional per-routine model (per-role models idea)
```

| Trigger | Source | Notes |
|---|---|---|
| `cron` | existing evaluator | unchanged semantics |
| `watch` | a connector's watch function (§3.2) | polled by the scheduler; no model call when it returns no events |
| `webhook` | `POST /v1/hooks/<routine-id>` with `Authorization: Bearer <routine secret>` | only when a TCP listener is on (§3.4); the body becomes the event payload (capped at 256 KB) |
| `message` | a gateway message matching a pattern in a channel | e.g. "every message in #receipts" |
| `event` | internal events: `run.failed`, `outbox.expired`, `connector.error`, `goal.check` | lets routines react to tern itself |
| `once` | an absolute time | reminders ("tomorrow at 9") |

An event starts a run in the routine's own session (as cron runs do today). The event payload is the run's input,
wrapped as untrusted (§5.3). Deduplication uses `routine_events(routine_id, event_key)` with a unique index, so a
re-delivered webhook or an overlapping poll can't start a second run. The event key is the connector's `id`, or a hash
of the body.

### 3.2 Watches in Luau

A connector can declare watches alongside its tools:

```lua
return {
  name = "github",
  capabilities = { http = { "api.github.com" }, auth = { "GITHUB_TOKEN" } },
  tools = { ... },
  watches = {
    review_requests = {
      description = "PRs where my review is requested",
      default_every = "5m",
      fn = function(ctx, cursor)
        local r = ctx.http.get("https://api.github.com/search/issues?q=review-requested:@me+is:open",
                               { auth = "GITHUB_TOKEN" })
        local seen, events = cursor or {}, {}
        for _, it in ipairs(r.json.items) do
          if not seen[tostring(it.id)] then
            table.insert(events, { id = tostring(it.id), title = it.title, url = it.html_url })
          end
          seen[tostring(it.id)] = true
        end
        return { events = events, cursor = seen }
      end,
    },
  },
}
```

- **The cursor** is opaque JSON stored per (routine, watch). It is saved only after the events are recorded, in one
  transaction. A crash between the two re-polls, and the dedup index absorbs the duplicates.
- **On the first poll**, events are recorded but no runs start (a baseline), unless `backfill = true`. Turning on an
  inbox watch must not fire 400 runs.
- **Limits**: the 5 s Luau limit applies per poll, and the minimum interval is 1 minute. `max_events_per_poll`
  (default 20) is enforced; overflow collapses into one batched run.
- **`origin = self`**: events carry an `origin` field the host fills in when it can tell, because the item's id
  matches an outbox item or a tool result in the journal. Self-originated events are dropped unless the routine sets
  `include_self = true`.
- **The agent can write watches** in the same draft-test-activate flow as tools. Activating a watch shows its interval
  and hosts.

Why polling in Luau rather than push: it works behind NAT, on a laptop, with no listener, and it is the same
sandboxed script model. Push (webhooks) is the upgrade path on a reachable core.

### 3.3 Gate scripts (skip the model)

A cron routine can also have a `gate`, a Luau expression or function that runs first. If it returns false, no run is
started. For example, "only brief me if there are unread emails or meetings today". This is NanoClaw's script gate.
It costs nothing in tokens.

### 3.4 Where triggers need an always-on core

| Trigger | Laptop (tier 1/2) | Server core (tier 3) | tern Cloud (tier 4) |
|---|---|---|---|
| cron, once | while awake; missed slots collapse into one run | ✓ | ✓ |
| watch | while awake; catches up from the cursor | ✓ | ✓ |
| webhook | ✗ (no listener by default; a `tern hook fire <id>` CLI exists for local scripts) | ✓ over the TLS listener | ✓, plus a stable public URL `hooks.tern.cloud/<id>` relayed to the core |
| message | while the gateway runs | ✓ | ✓ |

This table belongs in the UI: a routine that needs an always-on core says so when it's created. **This is the paid
product's clearest value**, and it never takes a feature away from the free tiers.

## 4. Delivery: initiative, digests, channels (T1–T2)

### 4.1 Initiative and quiet results

- **`NO_REPLY`**: if a routine run's final answer is exactly `NO_REPLY` (after trimming), nothing is delivered. The
  run is recorded as `completed (quiet)`. The routine prompt template tells the model this option exists.
- **Initiative levels:**

| Level | Behaviour |
|---|---|
| `silent` | Post to the channel only. No push. |
| `digest` | Post to the channel. Pushes are batched into one message at the digest times (default 08:30 and 17:30). |
| `notify` | Post plus push, through the **gate**: one call to the cheapest configured model with the result and the routine's purpose, answering only `{"interrupt": bool, "headline": str}`. "No" downgrades the push to `digest`. |
| `interrupt` | Post plus push, always. Bypasses quiet hours only if `urgent_ok = true`. |

- **Quiet hours** are a global setting (default 22:00–07:30 local), overridable per channel.
- **A global initiative dial** (Muse) scales every routine one level up or down: "vacation mode" makes everything
  `silent`.

### 4.2 Channels

Channels are **topic threads over one shared memory**, the Duet design. They are not security compartments and not
transports.

Detailed design (store, posts, how agents see each other's posts, desktop views, phases): [channels-design.md](channels-design.md).

| Concept | Representation |
|---|---|
| channel | `channels(id, slug, title, instructions, initiative_override, quiet_hours_override, created_by)`. `home` always exists. |
| a channel's conversation | sessions gain `channel_id`. A channel's timeline is the union of its sessions' messages and its routine posts, newest last. |
| instructions | optional `instructions` text, added to the system prompt for runs in that channel (e.g. "#finances: amounts in EUR, never move money"). |
| memory | shared across channels, unchanged. Memory entries record the channel they came from, for display only. |
| posts from background work | a routine or sub-agent result is a **post**: a message in the channel's timeline linked to the run. Home receives a one-line pointer only when the post needs the user ("needs you") or the routine is `notify`/`interrupt`. |

Creating channels:
- The user can create one: `/channel new finances` in the TUI, a + in the desktop sidebar, `/channel` in Telegram.
- The agent can **propose** one: the tool `channel_suggest(slug, title, why)` produces a card the user accepts. It
  never creates one silently.
- Accepting moves the related routines there.

Mapping to front ends:
- **Desktop**: channels are the sidebar's top section. Conversations within a channel nest under it.
- **TUI**: `/channel <slug>` switches. The status line shows the channel and the count of items needing you.
- **Telegram**: if the paired chat is a forum supergroup (or a private chat with topics enabled), each channel is a
  **topic**, mapped through `message_thread_id`. Otherwise channel posts are prefixed `#slug ·` in the one chat.
  Which Bot API version first allows topics in private bot chats needs checking before T2. Replying in a topic
  continues that channel.
- **Gateway connectors** (§6.4) declare whether they support threads. Slack and Discord threads map naturally.

### 4.3 Reply-to-act

A delivered post carries its run id. Replying to it (a Telegram reply, the desktop's reply button, an email reply
once an email gateway exists) continues **that** routine session with the reply as the next user message. This is
Google CC's "reply to the brief to act", and it falls out of sessions.

## 5. Effects, outbox, taint, autonomy (T3)

### 5.1 Effects

Every tool declares `effect`:

| Effect | Examples | Default policy |
|---|---|---|
| `read` | `read_file`, `http_fetch` GET, `gmail.search` | no approval |
| `write` | `write_file` in the workspace, `gmail.send`, `slack.post`, `calendar.create` | approval unless granted |
| `destructive` | `delete`, `pay`, `sign`, shell outside the allowlist | approval every time; grants allowed only for this routine and never for tainted runs |

How tools get their effect:
- Built-in tools are annotated in Rust.
- Luau tools declare `effect` (the default is `write` when it is missing; this replaces the M4 default of "no
  approval").
- MCP tools use the server's `readOnlyHint` / `destructiveHint` annotations where present, else `write`. The user can
  override per tool in config.

`requires_approval` stays as a way to force approval.

The host enforces all of this in `approval.rs`; the model can't talk its way past it. There is also a hard deny list
nobody can approve, same as Duet: tools tagged `money_movement` or `trade`. That is a connector manifest flag the
host refuses.

### 5.2 Outbox and deferred approvals

When a `write` or `destructive` call needs approval and no approver answers within `approval_wait` (default 2 min,
or at once when no client is attached):
1. The call is recorded as an **outbox item** (`outbox(id, run_id, call_id, tool, args, summary, created, expires,
   state)`), and the tool returns to the model: `queued for approval as outbox #12; continue with anything that
   doesn't depend on it`.
2. The run carries on and can complete. Its final post lists the pending items.
3. Approval (Telegram button, desktop card, `tern outbox approve 12`) executes the call **exactly once**, through a
   journaled `outbox_exec` step on a small system run in the same session, with idempotency keyed by the outbox id.
   The result is posted to the channel. If the model asked for a follow-up ("after sending, archive the thread"), it
   records that as a dependent outbox item, not as a promise in prose.
4. Items expire after 72 h, which posts once to home. Rejecting one can include a note that goes back to the session.

This needs no change to the run loop beyond a new tool-result kind. The sub-agent `inbox` step already proves the
pattern.

### 5.3 Taint

- A run is **tainted** when its input or any tool result so far came from outside the trust boundary: watch or
  webhook payloads, `http_fetch` bodies, MCP or Luau `read` results flagged `untrusted = true` (email bodies, web
  pages, issue comments), or messages from a non-owner in a shared gateway chat.
- Taint is a journaled bit on the run. It is sticky and never cleared within a run.
- External content reaches the model wrapped in `<untrusted source="gmail:msg/123">…</untrusted>`, with one
  system-prompt line saying instructions inside it are data.
- In a tainted run, every `write`/`destructive` call needs approval, grants notwithstanding. With nobody around, it
  goes to the outbox.
- The approval prompt shows why it is asking: "run triggered by email from billing@ex4mple.com".

This is deliberately blunt. It makes injection-driven exfiltration through tool calls need a human click. Read-only
exfiltration (a `read` tool whose URL encodes data) is closed by the per-connector host and path allowlists (§6.1):
`http_fetch` in a tainted run is limited to hosts already contacted in this run, or it asks.

### 5.4 Autonomy profiles

`autonomy = "readonly" | "supervised" (default) | "autonomous" | "yolo"`, globally and per routine:

| Profile | Approvals |
|---|---|
| `readonly` | `read` effects only |
| `supervised` | §5.1 defaults |
| `autonomous` | `write` doesn't ask in untainted runs |
| `yolo` | nothing asks; `doctor` and the status line show it in red |

## 6. Connector platform v2 (T4)

### 6.1 Credentials at the host boundary

- `capabilities.auth = { "GITHUB_TOKEN" }` declares which credentials a connector may *use*. Names resolve against
  the **connection** the call runs under (§6.8), so the same script serves several accounts.
- `ctx.http.*` accepts `auth = "GITHUB_TOKEN"` (Bearer by default) or `auth = { secret = "X", header = "X-Api-Key",
  scheme = "" }`. The host adds the header after the allowlist check. The value never enters the VM and is redacted
  from errors.
- `ctx.secret` keeps working for v1 connectors but is a separate capability, `secrets_raw`. The activation prompt
  shows it as a warning and `tern security audit` lists it. New agent-written connectors use `auth`.
- **Path allowlists**: `http = { "api.github.com/repos/*", "api.github.com/search/*" }`. Host-only entries keep working.
- **Rate limits**: `rate = "60/min"` per connector, enforced by the host. The host also handles `Retry-After`.

### 6.2 OAuth helper

```lua
capabilities = { oauth = { google = { scopes = { "gmail.readonly", "gmail.send" } } } }
-- then: ctx.http.get(url, { auth = "oauth:google" })
```

- Providers are built-in profiles (Google, Microsoft, GitHub, Slack, Notion, Linear, generic) giving authorize and
  token URLs plus PKCE support. A profile for anything else is produced by auth discovery (§6.7) and approved by the
  user.
- The flow runs **from a front end**, never from the agent: `tern auth google`, or a button in the desktop app. It
  opens the browser to the provider and catches the code on a one-shot `127.0.0.1` port, then stores refresh and
  access tokens as secrets of that **connection** (`conn/gmail/work/oauth/...`, §6.8). The host refreshes on 401 or expiry.
- **Client IDs**: the user supplies their own (documented per provider; Google needs a "Desktop app" OAuth client),
  or, on tern Cloud, ours. This is a real onboarding cost. A bundle can ship a setup guide (a skill) that walks
  through it.
- For a remote core, the device-code flow where the provider supports it, or the desktop app runs the browser leg
  and hands the code to the core over the API.
- Scopes are part of the capability diff on activation.

### 6.3 Read/write split and untrusted results

Tools declare `effect` (§5.1) and `untrusted = true` when they return third-party text. Validation rejects a
connector whose `read` tool uses a non-GET method unless it sets `effect = "write"` or explicitly sets
`read_via_post = true` (for GraphQL and search APIs).

### 6.4 Gateway connectors (channels in Luau)

A connector can implement a **chat transport**:

```lua
gateway = {
  kind = "slack",
  threads = true,
  poll = function(ctx, cursor) ... return { messages = {...}, cursor = ... } end,   -- or webhook = true
  send = function(ctx, to, text, opts) ... end,
  buttons = function(ctx, to, text, buttons) ... end,   -- optional; approvals fall back to "reply 1/2/3"
}
```

- The daemon runs `poll` on an interval, or receives webhooks on a reachable core.
- Pairing is shared with Telegram: the first message must be a pairing code, and unpaired senders get one fixed
  reply.
- Messages from a gateway connector are owner messages only after pairing. Anything else is tainted.
- Telegram stays native, since it is the reference implementation and the zero-setup path.
- Candidates, in order: **ntfy** (push-only, trivial, a phone notification with no bot setup), **Slack**,
  **Discord**, **Matrix**, **email via JMAP/Gmail API**, **WhatsApp Cloud API** (webhook only, needs a reachable
  core).
- **IMAP/SMTP** isn't HTTP, so it can't be a Luau connector today. Decision: start with Gmail API and JMAP in Luau.
  Add a native `email` gateway (async-imap plus lettre behind a cargo feature) only if users ask. Signal (signal-cli,
  Java) and iMessage stay out of scope.

### 6.5 Bundles (recipes)

A bundle is a directory or `.tar.zst` with `bundle.toml` (name, version, author key, files) holding connectors,
skills and routine templates.

- `tern bundle install <url|path>` shows the full capability set:
  - hosts and paths;
  - auth, OAuth scopes, effects;
  - routines and their triggers.
  It installs only on approval. Upgrades show a **capability diff** and re-ask when it grows.
- **Signing**: ed25519 (minisign-compatible) over the manifest, which lists the SHA-256 of every file. Unsigned
  bundles install with a warning. A lockfile pins (url, hash, key).
- **No registry in v1.** Sharing is a URL or git repo. A curated index (a static JSON of url, hash and key, reviewed
  by us) can come later. ClawHavoc is the reason.
- `tern bundle export` packages your own connectors, skills and routines to share. This is Poke's "Recipes" and Grok
  Bot's team skills.
- Import from OpenClaw/Hermes (`tern import openclaw <dir>`): copies USER.md, MEMORY.md and SOUL.md into tern's
  persona, user and memory files, and SKILL.md skills as drafts. Their plugins are not converted. Cheap, and it gives
  migrating users a route in.

### 6.6 Faster connector authoring

This is the moat: "connectors are just Luau", so make writing one take a minute.
- `connector_write` accepts an **OpenAPI or GraphQL schema URL**. The host fetches the schema (it can be large) and
  hands the model a trimmed operation list, so it can generate tools for the operations the user wants.
- `connector_test` gains **recorded fixtures**: the first live call is saved (redacted) and later tests replay it
  offline.
- Grok Bot's "teach a task": for sites without an API, the browser path (§9) records a flow and drafts a skill. That
  waits for T7.

### 6.7 Choosing how to authenticate: OAuth first

When the agent connects tern to something new, by writing a connector or adding an MCP server, its first job is to
find out **how that service wants to be signed into**. OAuth is preferred whenever it is usable:
- the user clicks "Sign in" instead of hunting for an API key;
- scopes are narrower than a personal access token;
- tokens refresh and can be revoked from the service's own settings;
- no long-lived key ever passes through the chat.

The current flows stay as the fallback:
- `secret_request` for keys and tokens ([secret-requests-spec.md](secret-requests-spec.md));
- `bearer_token_secret` / `header_secrets` for MCP servers ([mcp-management-spec.md](mcp-management-spec.md)).

**The flow** (written into the connector guide in `tools.rs` and the MCP-adding guide, and enforced in part by the
host):

```
 1. identify   what service, which API or MCP URL?
 2. discover   auth_discover(service, url?)                  ← host-side, deterministic, no model guessing
 3. research   only if discover says "unknown": web search + read the service's developer docs
 4. propose    auth_propose(plan)                            ← a card the user answers
 5. build      connector with capabilities.oauth (or auth = secret), or MCP server config
 6. connect    connection_add(target, method)                ← "Sign in to Notion" card, or secret_request
 7. test       connector_test / MCP verify, under that connection
```

#### 6.7.1 `auth_discover` (host-side)

A read-only tool. The host runs checks in a fixed order, so the common cases need no model reasoning and no web
search:

| Order | Check | Finds |
|---|---|---|
| 1 | **Built-in profiles** (Google, Microsoft, GitHub, Slack, Notion, Linear, Atlassian, Dropbox, …), matched by service name or API host | full profile, including whether tern Cloud has a client ID for it |
| 2 | **MCP servers**: the MCP authorization flow. Send an unauthenticated request; on 401 read `WWW-Authenticate` → `resource_metadata`, or fetch `/.well-known/oauth-protected-resource` (RFC 9728), then the authorization server's metadata | OAuth 2.1 with PKCE; whether the server allows **dynamic client registration** (RFC 7591) or **client ID metadata documents**, meaning no client ID setup at all |
| 3 | **Any HTTPS API**: `/.well-known/oauth-authorization-server` (RFC 8414) and `/.well-known/openid-configuration` on the API host and its registrable domain | authorize, token, device and registration endpoints; scopes; PKCE methods |
| 4 | Nothing found | `{"method": "unknown"}`: go to research |

The result is structured, never prose:

```json
{ "method": "oauth2", "source": "well_known", "issuer": "https://auth.notion.com",
  "authorize_url": "...", "token_url": "...", "device_url": null, "pkce": ["S256"],
  "client": "dynamic_registration",        // | "cimd" | "builtin" | "tern_cloud" | "user_supplied"
  "scopes_supported": ["read", "write"], "revocation_url": "...", "userinfo_url": "..." }
```

#### 6.7.2 Research when discovery comes up empty

The agent searches for "<service> API OAuth" and reads the developer docs, then produces the same structured plan
with an `evidence` list of doc URLs. The search goes through:
- the model's native web search when the provider has one (Anthropic's server-side web search, for example); or
- a configured search connector (Brave, Kagi or Tavily as a Luau connector); or
- `http_fetch` on the docs URL the user gives.

tern doesn't bundle a search engine. `doctor` notes when there is no search source, and the agent then asks the user
for the docs link.

Search results and docs are **untrusted** (§5.3), and an OAuth endpoint is an ideal phishing target, so the host
validates any plan that didn't come from a built-in profile or discovery steps 2–3:
- `https` only.
- The authorize and token hosts share a **registrable domain** with the API hosts the connector declares (`api.notion.com`
  ↔ `auth.notion.com`), or are a known identity provider (Google, Microsoft, Okta, Auth0 tenant patterns).
- Fetching the issuer's metadata must confirm the endpoints. If it can't (some services publish no metadata), the plan
  is marked **unverified** and the card says so in plain words.

#### 6.7.3 Deciding: when OAuth wins, and when it doesn't

The prompt states the rule; the host enforces the "user decides" part through `auth_propose`.

| Situation | Default |
|---|---|
| OAuth with PKCE (or device code), and the client ID comes from dynamic registration, CIMD, a built-in profile or tern Cloud | **OAuth**, and the user is told no setup is needed |
| OAuth that needs the user to register their own app (most "bring your own client ID" APIs) | **OAuth recommended**, with the setup steps from the docs summarised. API key offered as the quicker alternative. |
| OAuth only for "apps" that need review or business verification, or tokens that expire inconveniently (for example Google projects in *Testing* status issue refresh tokens that expire after 7 days) | Show both, with the caveat stated. Personal access token or API key first, if the service has one. |
| OAuth only with a confidential client (needs a client secret that can't live on a desktop) | API key, unless tern Cloud brokers it |
| No OAuth | the current flow: `secret_request` / MCP header secrets |

`auth_propose(plan, options)` shows a card in the desktop app and TUI, with the same pointer-only behaviour on
Telegram as secret requests. For example:

> **Connect Notion**
> **Sign in with Notion** (recommended): no keys to copy; you choose which pages tern can see.
> Use an integration token instead
> Cancel
> *Found via auth.notion.com metadata (verified).* · *Scopes: read, write*

The result is the user's choice. The agent never picks OAuth or a key on the user's behalf when more than one option
is shown.

#### 6.7.4 Connecting

- `connection_add(target, method, label?)` opens a **"Sign in to X" card** on a front end. That is the only place the
  browser leg runs (§6.2). The result is `connected {account}`, `skipped` or `timed_out`, the same shape as
  `secret_request`. The agent never sees a token.
- For a remote core without a desktop attached: the device-code flow if the service supports it (the card shows the
  code and URL). Otherwise "Finish signing in from the tern app".
- The **account label** comes from the userinfo or OIDC `email` claim, or from a connector's optional
  `whoami(ctx)` function. It is what tells several accounts apart (§6.8).
- A key-based method calls `secret_request` with the connection-scoped secret name. The prompt shows the target and
  connection, for example "for GitHub · personal".

#### 6.7.5 MCP servers

Adding a remote MCP server runs the same steps. Discovery step 2 is the MCP spec's own authorization flow, so for
compliant servers the user just clicks Sign in. This replaces the "OAuth for MCP servers: later" item in
mcp-management-spec §9. Stdio servers keep using environment secrets, but per connection (§6.8).

### 6.8 Connections: several accounts per connector or MCP server

A **connector** (or MCP server definition) is code and capabilities. A **connection** is one authenticated account
using it: *Gmail · work (mitch@inngest.com)* and *Gmail · personal (mitch@gmail.com)* are two connections of one
`gmail` connector. Users can add as many as they like. Each is stored and authorised separately.

#### 6.8.1 Model

| | Belongs to the connector or server | Belongs to each connection |
|---|---|---|
| code, tools, declared hosts, scopes requested | ✓ | |
| credentials (OAuth tokens, keys) | | ✓, as secrets `conn/<target>/<slug>/<NAME>` |
| account label, status, last error | | ✓ |
| `ctx.kv` namespace | | ✓ `lua:<connector>:<slug>` (cache, cursors) |
| watch cursors, routines' triggers | | ✓ (`watch = "gmail@work.new_mail"`) |
| approval grants ("always allow send") | | ✓: a grant for *work* doesn't cover *personal* |
| for stdio MCP servers: the process | | ✓, one process per connection, started lazily and stopped after `idle_timeout` (default 10 min) |

Store: `connections(id, kind, target, slug, label, account, auth_method, is_default, status, created_at,
last_used_at)`. Slugs are `[a-z0-9_-]{1,16}` and unique per target.

Config for MCP servers keeps the server-level fields as the definition. Each connection holds only its own credential
references:

```toml
[mcp.servers.linear]
transport = "http"
url = "https://mcp.linear.app/mcp"

[mcp.servers.linear.connections.work]      # OAuth: tokens live in secrets under conn/linear/work/
auth = "oauth"

[mcp.servers.linear.connections.side]
bearer_token_secret = "LINEAR_SIDE_KEY"
```

Migration:
- Every existing MCP server and connector becomes one connection named `default`, with its current secrets.
- Nothing is renamed, and v1 configs load unchanged.

#### 6.8.2 How the model chooses an account

Tool count must not multiply with accounts, so tools are not duplicated:
- With one connection, nothing changes.
- With two or more, the host adds a `connection` parameter (an enum of slugs, each described with its account label)
  to every tool of that connector or server. It routes the call accordingly: `ctx.connection` (slug, label) in Luau,
  or the matching MCP client session. A server tool that already has a `connection` parameter gets `tern_connection`
  instead.
- The system prompt's Integrations section lists them: `gmail: work (mitch@inngest.com, default), personal
  (mitch@gmail.com)`.

Defaults and ambiguity:
- **Read** calls without `connection` use the target's default, or the channel's default (a channel can pin one:
  `#work` → `gmail@work`, §4.2).
- **Write and destructive** calls without an explicit `connection`, when there is more than one, are **rejected by
  the host** with the list of options, so the model has to choose. A routine pins its connection when created.
- The approval card and the ledger always name the account: **"Send email from mitch@gmail.com (Gmail · personal)"**.
  That makes a wrong-account send visible before it happens.

#### 6.8.3 Managing connections

- **Desktop**: a Connections view grouped by service. Each service lists its accounts, with
  - **Add another account**, which reruns §6.7.4 with the same method;
  - rename, set default, **Reconnect** (when a token is revoked or expired; status `needs_reauth` shows in Tasks and
    the tray);
  - **Remove**, which revokes at the provider when a `revocation_url` is known, deletes the connection's secrets, kv
    and grants, and pauses routines that pinned it, with a notice.
- **TUI**: `/connections`. **CLI**: `tern connections list|add|remove|default`.
- **Same account twice**: connecting an account label that already exists for that target offers Reconnect instead of
  creating a duplicate.
- Limit: 10 connections per target (configurable). Each stdio MCP connection is a process, so the idle stop matters.

## 7. Memory and skills (T5)

M2 already has memory tools, compaction and FTS5 `session_search`. Additions:

- **Flush before compaction**: before a compaction step, one silent model call with the to-be-summarized span asks
  for durable facts. It writes through the existing memory tool. The call is journaled, so replay is deterministic.
  OpenClaw and Hermes both do this.
- **Capped profile files**: `user.md` and a new `memory.md` (core facts) are injected with a token budget each
  (default 1.5k). When over the budget, a consolidation run is queued (a `system` routine) instead of silently
  truncating. Long-tail memory stays searchable, not injected.
- **Editable everywhere**: memory is already files and rows. The desktop app adds a Memory view (gui-spec §4.3) with
  edit and delete, and every entry shows its source channel and run.
- **Skill capture**: after a run with ≥ N tool calls (default 8) that the user marked good or didn't correct, the
  agent may offer once: "Save this as a skill?" Accepting writes `skills/<slug>/SKILL.md` as a draft for review. It is
  never automatic, so there is no quiet drift. Skills use the agentskills.io format already in use.

## 8. Ledger, budgets, models, hardening (T6)

### 8.1 Activity ledger

- A view over the journal: every `write`/`destructive` effect executed, every outbox decision, every capability
  grant or revoke, every connector activation, and every bundle install.
- Filterable by channel, routine and connector. Exposed at `GET /v1/ledger`, through `tern ledger`, and in a desktop
  view. This is Muse's activity log.
- **Hash chain**: each ledger row stores `sha256(prev_hash || row)`, so tampering with the database is detectable
  (`tern ledger verify`). That gives ZeroClaw-style tool receipts with no extra machinery.

### 8.2 Tasks view

Duet's Tasks tab is a query, not a new model:
- running runs (with sub-agents nested);
- outbox items;
- goals (§10);
- routines in error.

"Needs you" is the count on the home channel badge. Only the user dismisses a goal or marks it done.

### 8.3 Budgets and usage

- Per-routine and global budgets: `runs_per_day`, `usd_per_day`, `tokens_per_run`. Prices come from the provider
  model list (P0) where known.
- When exceeded, the routine pauses and posts to home once.
- `/usage` gains a per-routine and per-channel breakdown.
- Per-role models: `models.gate`, `models.routine_default`, `models.subagent` and `models.main`. The gate and
  watches-to-runs default to the cheapest configured model.
- **Provider fallback chain**: `default_model = ["anthropic:…", "openrouter:…"]` tries the next on 5xx, overload or
  auth failure. The journal records which one answered.

### 8.4 `tern security audit`

A read-only report that exits non-zero on high findings:
- listeners and bind addresses;
- device tokens and their scopes;
- the autonomy profile;
- routines that can be tainted and hold `write` grants;
- connectors using `secrets_raw`;
- unsigned bundles;
- stale OAuth scopes;
- `secrets.toml` permissions.

`doctor` links to it.

### 8.5 `tern mcp serve`

Exposes tern's **connectors, memory search and channels** as an MCP server over stdio, so Claude Code or another
agent can use your Luau connectors and post into a channel. The effect policy and taint still apply. Cheap: rmcp is
already a dependency (it needs the `server` feature), and it spreads the Luau connector story outside tern.

## 9. Browser (T7, opt-in)

Ranked by cost:
1. **External MCP**: Chrome DevTools MCP or Playwright MCP, set up through the existing MCP management flow. Zero
   engine work, and we document it as a recipe. **This is the v1 answer.**
2. **Native CDP to the user's installed Chrome or Chromium**, behind a `browser` cargo feature (chromiumoxide or a
   minimal CDP client on tokio-tungstenite):
   - Tools: `browser_open`, `browser_snapshot` (an accessibility-tree text snapshot with refs, not screenshots by
     default), `browser_act(ref, action)`, `browser_screenshot`.
   - It uses a **dedicated profile** by default. Attaching to your real signed-in profile needs an explicit config
     flag.
   - Every page's content is untrusted (§5.3). Form submits and purchases are `destructive`.
   - We never bundle Chromium.
3. A cloud browser, possibly as a tern Cloud add-on. Not now.

OS-level computer use (vision clicking) is out: it is per platform, heavy, and the highest injection risk.

## 10. Recipes on top (T6, mostly content)

These ship as a built-in bundle. They are skills plus routine templates, not engine features:

| Recipe | Trigger | Uses |
|---|---|---|
| **Morning brief** | cron 07:30 workdays, gate "anything today?" | calendar plus email connectors; posts to home at `digest`; reply-to-act |
| **Quiet-thread follow-up** | daily | email search for threads awaiting reply > 3 days; drafts go to the outbox |
| **Meeting prep** | watch on calendar: event starts in 60 min with external attendees | web and memory search; posts to the event's channel |
| **Watch a page** | watch: `http_fetch` plus a diff of the extracted text | for prices, docs, status pages; `notify` on change |
| **GitHub / Linear triage** | watch | `#work` channel |
| **Weekly review** | cron Friday 16:00 | ledger plus channel activity, summarized |

**Goals** (Muse, Gemini): a goal is `goals(id, channel, statement, check_every, state_json, status)` plus a
`goal.check` event routine. It is a sub-agent run in the goal's channel that reads its prior state, does one step,
and updates state. It shows in Tasks. Only the user closes a goal.

## 11. Menu-bar tray: the daemon without the window (T8)

### 11.1 Problem

Everything above assumes an agent that is **always there**: routines fire at 07:30, watches poll, and the outbox
waits for a click. Today the desktop app is the only GUI presence, and it is also the approval route. Close it, and
the daemon is still running (if it was started separately) but invisible:
- Approvals from API clients are denied (gui-spec §6.3).
- Nothing tells you a routine posted.
- There's no one-click way to see whether tern is up.

The gui-spec planned a menu bar extra *inside* the Flutter app (§5, `tray_manager`). That would mean keeping a Flutter
engine (100+ MB RSS) alive just to show an icon, and it ties the agent's presence to the UI process. Duet, Grok Bot
and Muse all stay reachable with no app open. We should too, at a few megabytes.

### 11.2 Shape: three processes, each optional

```
 launchd / systemd --user
   ├── tern daemon        (headless engine: runs, routines, gateways, API; KeepAlive)
   └── tern tray          (menu-bar icon + notifications; API client of the daemon; LSUIElement, no Dock icon)
 tern.app (Flutter)       (the window; API client; started on demand, quits when its window closes)
```

- **The daemon owns all state and never depends on a UI.** That is already true. This section only makes it the
  default way to run.
- **The tray is a client**, like the TUI and the desktop app. It talks `/v1` over the Unix socket, or over TLS to a
  remote core (D1), using the same connection profiles. A tray crash can't hurt a run.
- **The desktop app is a client that may come and go.** Closing its last window quits it (Cmd-W or Cmd-Q). The Dock
  icon appears only while a window is open. There is no hidden-window mode: that is exactly the heavy process this
  avoids.

Why a separate process rather than putting the tray inside the daemon:
- AppKit wants the main thread and an app bundle; the daemon is a headless tokio process that also runs on servers.
- Keeping them apart means `tern daemon` stays identical on a VPS, and a tray bug can't take down the engine.

Cost: one extra process of a few MB, and one SSE connection.

### 11.3 `tern tray`

A subcommand of the same binary, compiled behind a `tray` cargo feature. It is on for macOS release builds and the
desktop bundle, and off for musl server builds, so server binaries don't grow.

| Platform | Icon and menu | Notifications | Notes |
|---|---|---|---|
| macOS | `tray-icon` + `muda` (pure Rust over objc2/AppKit; the crates Tauri uses) | `UNUserNotificationCenter` through objc2, with actions | Needs an **app bundle identity** for notifications and login items. Ships as `tern.app/Contents/Library/LoginItems/tern Tray.app`, a minimal bundle (`LSUIElement = true`) whose executable is a symlink or copy of `tern` started with `tray`. |
| Linux | `ksni` (StatusNotifierItem over D-Bus, pure Rust via zbus; no GTK, so it works in static builds) | `notify-rust` (D-Bus, zbus backend) with actions where the server supports them | KDE and most desktops show SNI icons. GNOME needs the AppIndicator extension; `doctor` says so. |
| Windows | out of scope (as everywhere else) | | |

Budget: **< 15 MB RSS idle** and no busy loop. It sleeps on the SSE stream, and the icon only changes on events.
`tern tray` without a GUI session (SSH, a server) exits with a clear error.

### 11.4 The menu

```
 ● tern — running · 2 runs · 3 routines active        (○ stopped / ◐ reconnecting / ● remote: home-server)
 ───────────────────────────────
 Needs you (2)                     ▸  outbox #12 "Reply to Dana re: invoice"   Approve · Reject · Open
                                      approval: shell `git push` (run 8f2c)     Allow once · Deny · Open
 Recent posts                      ▸  #work  GitHub triage: 2 PRs need review   (opens in app)
                                      #home  Morning brief                        (opens in app)
 ───────────────────────────────
 New message…                      ⌘N   (opens the app's composer; see §11.6)
 Open tern                         ⌘O
 Initiative                        ▸  Normal · Quiet (digest only) · Vacation (silent) · Paused (no routines)
 ───────────────────────────────
 Start at login                    ✓
 ───────────────────────────────
 Quit tern                         ⌘Q   (stops everything: the background agent, the tray and the app; §11.5a)
```

Stopped state (the icon stays until the user quits it; one click brings tern back):

```
 ○ tern is stopped. Routines and Telegram are paused.
 ───────────────────────────────
 Start tern
 Start at login                    ✓
 Close this icon
```

- **The icon reflects state**: idle, working (a run in progress), needs you (a dot badge), stopped, or
  disconnected. That's the at-a-glance "is tern alive?".
- **Approve and reject happen from the menu or the notification without opening the app.** The tray calls the
  existing approval endpoints and the T3 outbox endpoints. Destructive effects and tainted-run approvals can't be
  approved from a notification banner. They open the app, or show the full summary in the menu, so a one-tap approve
  never lands on something consequential (W7).
- **Initiative** sets the global dial from §4.1. "Paused" stops routines from firing, but chat and gateways still
  work.
- **Quit tern** is the one way to stop, and it's the obvious one (§11.5a). There is deliberately no separate
  "quit tray but keep the daemon": to a non-technical user an icon that disappears while something keeps running
  looks like malware. Keeping the agent running without the icon is a Settings option for people who want it.

### 11.5 Lifecycle and installation

- **Start at login** installs two per-user services:
  - **macOS**: `SMAppService.agent(plistName:)` from the app bundle (macOS 13+) for `ai.tern.daemon` (`tern daemon`,
    `KeepAlive = { SuccessfulExit = false }`, logs to `~/.tern/logs/`), and `SMAppService.loginItem` for
    `tern Tray.app`. `SuccessfulExit = false` means launchd restarts a *crash* but not a clean exit: that one key is
    what makes Quit work (§11.5a). This replaces the
    hand-written LaunchAgent in gui-spec §6.2. They show up in System Settings › Login Items, where users expect to
    manage them.
  - **Linux**: `~/.config/systemd/user/tern-daemon.service` (`Restart=on-failure`) and an XDG autostart entry for
    `tern tray`.
  - **CLI-only users** get the same through `tern service install [--tray] | uninstall | status`. It writes a plain
    LaunchAgent plist on macOS when there's no app bundle. `doctor` reports which method is in use.
- **The tray supervises nothing.** The service manager keeps the daemon up. If the daemon is down and not managed,
  the tray offers **Start tern** (spawns `tern daemon` detached, exactly as gui-spec §6.2 does).
- **The single-daemon lock stays as it is (M5).** A second `tern daemon` exits with the first's pid, so an
  app-started daemon and a launchd one can't both run. Starting the app with a daemon already up just attaches.
- **Upgrades**: the desktop bundle carries the `tern` binary. On app update, the tray notices `version` changed in
  `/v1/health` and offers **Restart tern to update** (nothing is lost, since runs resume). The app never restarts
  the daemon silently mid-run.
- **Remote cores**: the tray can point at any connection profile. With a remote core (tier 3 or 4) there's no local
  daemon. The tray shows the remote's status, and Start/Stop and service items are hidden.

### 11.5a Stopping tern, for people who never open a terminal

Goal: anyone can turn tern off completely in one click, see that it's off, and turn it back on, without knowing what
a daemon, a process or launchd is. The UI never says "daemon": it says **tern** and, where needed, "running in the
background".

**The trap this avoids.** A service manager set to keep a process alive restarts it the moment it's killed. A naive
Stop button "works", and then tern is back two seconds later. That's how background apps earn a reputation for being
impossible to quit. So stopping always goes through tern, never through a kill:
1. The daemon receives `POST /v1/shutdown`. It stops polling, leaves runs `running` (they resume on the next start,
   per M5), flushes, and **exits with code 0**.
2. Because the service is `KeepAlive = { SuccessfulExit = false }` on macOS and `Restart=on-failure` on Linux, a clean
   exit is not restarted. A crash still is.
3. The daemon stays registered for login, unless the user also unticks Start at login. "Quit" means "off until I
   start it or log in again", which matches how every Mac app's Quit behaves.

**Where Quit lives**, all reachable with a mouse:

| Place | Control | What it does |
|---|---|---|
| Tray menu | **Quit tern** (⌘Q while the menu is open) | Stops the background agent, closes the desktop app if open, removes the tray icon. |
| Tray menu, stopped state | **Start tern** · **Close this icon** | Start relaunches through the service manager (or spawns `tern daemon` if unmanaged). Close removes the icon. |
| Desktop app menu | **tern › Quit tern completely** (⌥⌘Q), beside the normal Quit (⌘Q, which closes only the window) | Same as the tray's Quit. |
| Desktop app, Settings › General | "**When I close the window:** keep tern running in the background (recommended) / quit tern completely" | For people who want closing the window to mean off. |
| Desktop app, Settings › General | **Start tern when I log in** ☑ | Registers or unregisters both login services (§11.5). |
| Desktop app, Settings › General | **Remove tern from this Mac…** | Stops tern, unregisters all services, asks whether to keep or delete `~/.tern` (default: keep), then tells the user to drag tern.app to the Bin. |
| macOS System Settings › Login Items | the two tern entries | Works as for any app, because we use `SMAppService`; turning them off there is respected (the tray shows "Start at login" unticked). |

**The first close explains itself.** The first time the user closes the window while tern keeps running, a single
notification (not a modal) says: *"tern is still running in the menu bar, so routines and Telegram keep working.
Quit it any time from the ● icon."* It has a **Quit tern now** action. It is shown once.

**Confirmation, in plain words.** Quit asks only when something would be interrupted, and says what:

> **Quit tern?**
> 1 task is in progress (*"Summarize the Q3 report"*). It will pick up where it left off next time tern starts.
> Routines and Telegram will be paused until then.
> [Quit tern] [Cancel]

There is no "are you sure" when nothing is running. Approvals waiting in the outbox survive and are listed again on
the next start.

**When it doesn't stop.** If the daemon doesn't exit within 10 s of `/v1/shutdown` (wedged, or an old version
without the endpoint), the tray and app escalate by themselves, still with no terminal:
1. They read the pid from the daemon's pid file (`~/.tern/tern.pid`, written under the existing M5 lock) and verify
   it is a `tern` process owned by this user.
2. They send SIGTERM and wait 5 s, then SIGKILL.
3. When the service manager would restart it anyway (an old `KeepAlive = true` plist), they unregister the service
   first (`SMAppService.unregister` or `launchctl bootout`; `systemctl --user stop` on Linux) and re-register it with
   Start at login still ticked, so the next login works.
4. The UI says "tern was stopped" and, if the kill was needed, offers **Send a report** (`tern report`).

The user only ever sees "Quitting tern…" and then the stopped icon.

**Visible state.** The tray icon is the source of truth: filled ● running, ○ stopped, ◐ starting or stopping. When
tern is stopped and the user has closed the icon, opening tern.app shows the existing calm "tern isn't running" state
with one **Start tern** button. Off is always visible as off.

**CLI parity.** For technical users and scripts there's `tern stop` (same graceful path and escalation) and
`tern start` (through the service if installed, otherwise detached). They need no `launchctl` knowledge.

### 11.6 Interaction with the rest of this spec

- **Approval routing**: an attached tray counts as an API approval client. Approvals no longer fall to "deny" just
  because the window is closed. The rule becomes: if no client answers within `approval_wait`, the request goes to
  the outbox (T3). The tray shows outbox items, so nothing is lost, and the gui-spec §6.3 caveat goes away once T3
  lands.
- **Notifications move to the tray.** The desktop app stops posting its own native notifications whenever a tray is
  connected. A new `GET /v1/clients` shows who's attached, and the app checks it. That prevents double banners.
  Clicking a notification opens the app at that channel or run (macOS URL scheme `tern://channel/work`,
  `tern://run/<id>`, `tern://outbox/12`; the app registers it, and the tray launches the app with it if the app isn't
  running).
- **Digests and quiet hours (§4.1)** are decided by the daemon. The tray just shows whatever arrives, so behaviour is
  the same on Telegram, the tray and mobile push.
- **Quick input**: a native text field in a menu is awkward on both platforms. "New message…" opens the app's
  composer focused on the home channel. A global hotkey that summons a small composer panel without the full window
  is a possible G6+ follow-up, done in the app, not in the tray.

### 11.7 API additions

| Endpoint | Purpose |
|---|---|
| `GET /v1/status` | One cheap summary for the tray: daemon version, uptime, counts (running runs, needs-you items, active routines, gateway health), and the initiative level. Kept small so the tray doesn't fetch lists on every event. |
| `POST /v1/shutdown` | Graceful stop with exit code 0, so the service manager doesn't restart it (local socket, or the `admin` scope remotely). Returns the in-progress runs first when called with `?dry_run=1`, which the confirmation dialog uses. |
| `PUT /v1/initiative` | Set the global dial. |
| `GET /v1/clients` | Attached clients and their kinds (`tray`, `desktop`, `tui`), for notification de-duplication. Clients send `X-Tern-Client: tray/0.x` on the SSE request. |

### 11.8 Acceptance

- Closing the desktop window quits the Flutter process. The daemon and tray keep running, and a cron routine fires
  and its notification appears.
- An approval raised with only the tray attached can be answered from the notification. After T3, one raised with
  nothing attached lands in the outbox and shows in the tray menu.
- Tray idle RSS < 15 MB. `tern` musl builds without the `tray` feature are unchanged in size.
- **Quit**: from the tray, with a run in progress and the daemon under launchd/systemd: the confirmation names the
  run; after Quit there is no `tern` process left 15 s later (checked for 60 s, so no launchd respawn); the next Start
  resumes the run. The same from the app's "Quit tern completely" and from `tern stop`.
- **Wedged daemon**: a test build that ignores `/v1/shutdown` is still gone within 20 s of Quit, and Start at login
  is still registered afterwards.
- **No terminal anywhere** in the start, stop, login-item or uninstall flows (a checklist item for the T8 UI review).
- Killing the tray doesn't affect a run in progress. Killing the daemon under launchd/systemd restarts it, the tray
  shows `◐` and then `●`, and the run resumes.

## 12. Phases

The phases below (T0–T8) group the work **by area**. §12.1 orders the same items into **stages by complexity and
dependency**. Build stage by stage: each stage is shippable alone, and nothing in a stage needs a later one. Where a
phase mixes easy and hard parts, it is split across stages (T1a/T1b, T8a/T8b and so on).

### 12.1 Stages

| Stage | Theme | Items | Complexity | Needs |
|---|---|---|---|---|
| **1. Quick wins** | Small changes to what exists. No new data model, no new protocols. Each item is days, not weeks. | **T0** effects on every tool, money-movement deny · **T1a** on today's schedules: `NO_REPLY`, quiet hours, per-schedule run caps and budget · **T5a** memory flush before compaction, capped `user.md`/`memory.md` · **T6a** provider fallback chains · **T8a** `/v1/status`, `/v1/shutdown`, `tern start`/`stop`, `tern service install` with `SuccessfulExit = false`, "Quit tern completely" in the app (one-click stop for non-technical users, before the tray exists) · **T7a** browser via an MCP recipe (docs only) | S each | nothing |
| **2. Foundations** | The new core concepts everything later builds on. Migrations land here. | **T1b** routines (schedules migrated), Luau watches with cursors and dedup, gate scripts, initiative levels, the gate model, digests · **T3** taint, untrusted wrapping, outbox and deferred approvals, autonomy profiles · **T4b-i** host-injected credentials (`auth =`), path allowlists, rate limits | L / M / S–M | Stage 1 (effects, budgets) |
| **3. Organize and surface** | Make background work visible and tidy. Mostly UI plus straightforward store work. | **T2** channels, home, posts, reply-to-act, Telegram topics · **T6b** activity ledger with hash chain, Tasks view, `tern security audit` · **T8b** `tern tray` on macOS (helper bundle, notifications with actions, login items, quit escalation, `tern://` links) · **T5b** skill capture | M each | Stage 2 (routines, outbox) |
| **4. Connections** | External protocols and the most security-sensitive code. The largest stage. | **T4a** `auth_discover`, research validation, `auth_propose`, the OAuth helper (PKCE, device code, refresh, revoke), connections and multiple accounts for connectors and MCP servers · **T1c** webhook triggers (reachable core) · **T4b-ii** gateway connectors (ntfy, then Slack) | L | Stage 2 (taint before any external input); secret-requests spec built |
| **5. Ecosystem and bigger bets** | Things that compound once the platform exists, and optional heavy features. | **T6c** recipe bundle (morning brief, follow-ups, meeting prep), goals · **T4b-iii** bundles with signing, `tern bundle`, `import openclaw` · **T6d** `tern mcp serve` · **T4c** OpenAPI-to-connector and recorded test fixtures · **T8c** tray on Linux (ksni) · **T7b** native CDP browser behind a feature | M each, T7b L | Stage 4 (recipes need Google OAuth; bundles carry connections) |
| **6. Multiplayer** (later) | Several people with one agent: group rooms first, then members on one core, then hosted workspaces (§15) | **M0** Telegram, Slack and Discord group rooms with guests · **M1** members, per-member connections, approvals and memory · **M2** tern Cloud workspaces, invites, seats | M0 S–M, M1 L, M2 M | Stages 2–3 (taint, channels); M2 needs tern Cloud |

Why this order:
- **Stage 1** delivers visible quality of life immediately: quiet routines, cost caps, easy stop. It costs almost
  nothing and de-risks the effect model before anything depends on it.
- **Stage 2** is where the hard *design* lives (routines, taint, outbox). Doing it before connections means every
  external input that arrives later is already tainted and every write already goes through the outbox. Rule from
  W1: no email or webhook watch is on by default until T3 ships.
- **Stage 3** is mostly presentation of data Stage 2 creates. The tray waits until here because before routines and
  the outbox it has little to show (T8a already covers start and stop).
- **Stage 4** is the hardest *engineering*: OAuth edge cases, per-provider quirks, account routing, migrations of
  secrets. Connections go in with OAuth, not later, to avoid a second migration of secrets and grants.
- **Stage 5** is content and optional features that reuse everything above.
- **Stage 6** is multiplayer. Its schema hooks (`author_id`, connection owner, ledger actor) go into stages 2–4 so
  the stage needs no migration later. See §15.4.

### 12.2 Phases by area

| Phase | Scope | Engine | Clients | Size |
|---|---|---|---|---|
| **T0** | `effect` on all tools; Luau `effect`/`untrusted`; MCP hint mapping; money-movement deny | approval.rs, tool.rs, tern-lua spec | approval prompts show effect | S |
| **T1** | Routines (migrate schedules), watch triggers, gates, `NO_REPLY`, initiative and gate model, quiet hours, digests, per-routine budgets | tern-core scheduler, store migrations, tern-lua watches | routine list and editor (desktop), `/routines` (TUI) | L |
| **T2** | Channels, posts, home, reply-to-act, Telegram topics | store (sessions.channel_id), gateway | desktop sidebar, TUI `/channel` | M |
| **T3** | Taint, untrusted wrapping, outbox and deferred approvals, autonomy profiles | agent loop (new tool-result kind), approval.rs | outbox cards in Telegram and desktop; Tasks view | M |
| **T4a** | Auth and connections: `auth_discover` (profiles, RFC 9728/8414, OIDC, dynamic registration), research guidance and endpoint validation, `auth_propose`, `connection_add`, OAuth helper (PKCE, device code, refresh, revoke), connections for connectors and MCP servers (migration to `default`, `connection` parameter, per-connection secrets, kv, grants, stdio process per connection) | tern-lua, tern-mcp, store, prompts | Sign-in and choice cards, Connections view, `/connections` | L |
| **T4b** | Connector v2: host-injected auth, path allowlists, rate limits, gateway connectors (ntfy and Slack first), bundles with signing, `import openclaw` | tern-lua, `tern bundle` | bundle install diff | M |
| **T5** | Memory flush, capped profiles with consolidation, skill capture | compaction, memory | Memory view | S–M |
| **T6** | Ledger with hash chain, Tasks, `security audit`, fallback chains, `mcp serve`, recipe bundle, goals | store, rmcp server | Ledger and Tasks views | M |
| **T7** | Browser: MCP recipe now, native CDP behind a feature later | optional crate | | S / M |
| **T8** | `tern tray` (macOS and Linux), one-click Quit with escalation, `tern start`/`stop`, `tern service install`, login items, `/v1/status`, `/v1/shutdown`, `/v1/initiative`, `/v1/clients`, `tern://` deep links | `tray` cargo feature (tray-icon, muda, ksni, notify-rust) | app quits with its window, stops posting notifications when a tray is attached, handles `tern://` | M |

Order rationale by phase (the stages in §12.1 apply it):
- T0 is small and everything else leans on effects.
- T1 is the largest user-visible win.
- T3 must land **before** watches on email or webhooks are enabled by default. Until then, `watch` and `webhook`
  routines force `autonomy = "readonly"` unless the user overrides it.
- T4a's OAuth unblocks Google, which is table stakes for briefs. Connections land in T4a, not later, because adding
  them after people have single-account secrets and grants means a second migration. T4a builds on the
  secret-requests spec (its `secret_request` is the fallback path) and should follow it.
- T8 is independent of T1–T7 and can go first, or in parallel. It makes the existing scheduler and Telegram
  approvals visible today. It pays off fully with T1 (routines worth being notified about) and T3 (the outbox
  replaces "deny when nobody is attached").

Acceptance highlights (each phase also passes clippy `-D warnings` and `cargo test-all`):
- **T1**: a watch returning no events makes zero provider calls (mock provider asserts). A crash between event record
  and cursor save starts each run exactly once. A `NO_REPLY` run delivers nothing.
- **T3**: an injected email that asks to send mail produces an outbox item, not a send, even with an "always" grant.
  Approving executes once across a daemon restart.
- **T4a**:
  - Discovery against a fixture MCP server with RFC 9728 metadata and dynamic registration yields an OAuth plan with
    no model call.
  - A researched plan whose token URL is on another domain is rejected.
  - Two Gmail-shaped connections with a fake provider:
    - reads default correctly;
    - a send without `connection` is rejected with the options;
    - each connection's tokens, kv and grants are separate;
    - removing one leaves the other working.
- **T4b**: a connector using `auth =` never sees the secret: `print` of every ctx field and a deliberate echo endpoint
  in the test server show only redacted values.
- Binary stays < 25 MB without the `browser` feature. Idle RSS stays < 30 MB with 10 watches registered.

## 13. Decisions taken here (revisit if wrong)

1. **Polling watches in Luau before push.** Push needs a reachable core; polling works everywhere.
2. **Channels share memory.** Isolation is the job of the autonomy and effect policy, not of channels.
3. **Taint is sticky per run and coarse.** Fine-grained information-flow tracking isn't worth its complexity yet.
4. **Effect default for unannotated tools is `write`.** This changes M4's "no approval" default for Luau tools; v1
   connectors keep their old behaviour until re-activated.
5. **No public registry.** Signed URLs and a curated index later.
6. **Email via HTTP APIs first.** Native IMAP only on demand.
7. **Browser via MCP first.** Native CDP behind a feature; never bundled.
8. **The tray is a separate native process, not the Flutter app hidden.** It keeps a few-MB presence instead of a
   100+ MB one, and leaves `tern daemon` headless and identical on servers. The service manager, not the tray, keeps
   the daemon alive.
9. **OAuth first, keys as fallback, and the user chooses** whenever there's more than one real option.
   Discovery is host code, not model guesswork. Research is only for services with no metadata.
10. **Accounts are connections, not copies of connectors.** One tool set with a host-injected `connection`
    parameter, rather than duplicating tools per account. That keeps the tool list and prompt size flat.

## 14. Open questions

- Telegram topics in private bot chats: which Bot API version, and does it need the user to enable anything? Fallback
  is the `#slug ·` prefix.
- OAuth client IDs for Google: is "bring your own client" acceptable friction for tier 1, or do we run a
  token-exchange broker (which is a hosted dependency, and then belongs to tern Cloud)?
- Should the gate model also judge **channel routing** ("this post belongs in #finances"), as Duet appears to do, or
  is that `channel_suggest` only?
- Taint and sub-agents: should a tainted parent's children start tainted? Proposed: yes, since the brief may carry
  the injected text.
- Tray on macOS: is `tray-icon`/`muda` plus hand-written objc2 calls for `UNUserNotificationCenter` and
  `SMAppService` enough, or is a ~200-line Swift helper bundle simpler to maintain? Swift adds a build step but no
  runtime weight. Decide in a T8 spike.
- **Search source for auth research**: is provider-native web search plus "ask the user for the docs link" enough, or
  should tern ship a default search connector (which needs an API key for Brave, Kagi or Tavily)?
- **Our own OAuth client IDs on the local tier**: shipping tern's Google desktop client would make Gmail one click
  for everyone. But Gmail's restricted scopes need Google verification plus a yearly security assessment. Is that cost
  worth it, or do we keep "bring your own client" locally and give tern Cloud verified clients?
- Should a channel be able to restrict which connections it may use at all (not just its default), as a light form of
  separating work and personal?
- Pricing line: is "webhooks plus always-on watches" the free/paid boundary on Cloud, or does tier 3 self-hosting make
  that moot, so we sell convenience only (deployment-spec §10.1)?

## 15. Multiplayer: several people, one agent (future; hosted first)

Status: research and sizing only. Not in stages 1–5. Raised 2026-11-03 from creator feedback on Grok Bot ("7 Grok
Bot agents I use every day", https://www.youtube.com/watch?v=QBmgF1kJSK4): the creator wants **several people in the
same chat with the same agent**, and finds Grok Bot's lack of that a real downside. The transcript wasn't available;
this section works from that summary plus the vendors' documentation
([research/personal-agents-2026.md](research/personal-agents-2026.md) §4).

### 15.1 What others do (November 2026)

| Product | Several humans, one conversation? | How |
|---|---|---|
| **Grok Bot** | Only on Cursor **Team/Enterprise** plans, and only in group chats or Slack | A "shared Bot" has owner, Manager and Collaborator roles. Each person's 1:1 chat with it stays private, even from the owner. In group chats and Slack threads the Bot **acts for whoever asked**: their usage, their connected accounts (it asks before using them), and only they can answer its approvals. The Bot saves team memory but keeps what it learns about you private. An individual user can't invite a partner into a chat, which is likely the gap the creator hit. |
| **ChatGPT group chats** | Yes, up to 20 people, consumer plans | Personal memory and custom instructions are **switched off** in groups. The group has its own instructions. Agent mode, voice and Deep Research aren't available in groups. It replies when mentioned or when it judges it should. |
| **Claude Duet** | No (reviewers call this out) | Single-user. |
| **Claude Tag / Slackbot / Dust** | Yes, inside Slack | The agent lives in the team's existing channels, and anyone can tag it. |

What the category teaches:
- **Shared room, per-person authority.** The conversation is shared. Accounts, approvals and billing belong to the
  person who asked.
- **Private memory stays private.** Either it is disabled in groups (ChatGPT) or split into team and personal (Grok
  Bot).
- Cross-user contamination is the known failure mode: one member's data or injected instructions leaking into
  another's results.

### 15.2 What it would take in tern

tern assumes **one owner per core** throughout. deployment-spec §1 lists multi-user as a non-goal, and devices have
scopes (owner, chat, read) but all act as that one person. These parts would change:

| Area | Today | Multiplayer needs |
|---|---|---|
| Identity | devices and Telegram pairings belong to "the owner" | a `members(id, name, role: owner/member/guest)` table. Devices, gateway pairings and Cloud logins point at a member. API auth resolves to (member, device scope). |
| Conversations | one author per session; **a new message cancels the running run** (M5) | messages carry `author_id`, and the model sees `[Dana] …`. In shared channels a new message **queues or steers** (through the inbox step that sub-agents already use) instead of cancelling someone else's run. |
| Channels (§4.2) | single-user | channel membership (who can read and post), per-channel instructions, and group mention rules (reply when @-mentioned or asked a question) |
| Connections (§6.8) | the owner's | each connection has an **owner member**. A run uses the requester's connections. Using someone else's needs that person's grant, on the Grok Bot model. Shared connections (a team Linear) are explicit. |
| Approvals (§5) | routed to the owner | routed to the **requester**. A shared connection's writes go to its owner. The outbox records who approved. |
| Memory (§7) | one store | scopes: `personal(member)`, `channel`, `core`. Personal memory is never injected into a run another member started. Default for group channels: channel memory only, as ChatGPT does. |
| Taint (§5.3) | messages from non-paired senders are tainted | every other member's message is **untrusted with respect to me**. A run started by Dana that touches my connection is tainted for me. This is the hardest rule to get right, and it's why the taint machinery must land first. |
| Budgets (§8.3), ledger (§8.1) | per routine, global | per member as well, and every ledger row names the acting member and the requester |
| Secrets | one `secrets.toml` | per member (`member/<id>/…`) plus core-wide. With connections (§6.8) this is mostly a namespace change. |
| Sandbox / shell | one workspace | a per-member workspace directory at least. On a shared core, shell stays off for non-owners by default. |

### 15.3 Levels, cheapest first

**M0: Group gateway rooms.** S–M, any tier, no accounts.
- The owner adds tern to a **Telegram group** (or a Slack or Discord channel through a gateway connector, §6.4) and
  pairs that room to a channel.
- The other people in it are **guests**. They can talk to tern there, and their messages are tainted. tern uses only
  the connections the owner has allowed for that channel, and only the owner can approve.
- Telegram's bot privacy mode already limits a bot in a group to @-mentions, replies and commands.
- This gets the "several of us in one chat with the agent" experience with almost no identity work. It is a good test
  of demand.
- Needs: T2 channels, T3 taint, the queue-instead-of-cancel rule, `author` on messages, and a per-channel connection
  allowlist.

**M1: Members on one core.** L. Self-hosted or Cloud.
- Everything in §15.2: members, invites (a pairing code issued to a person rather than a device), per-member
  connections, approvals, memory scopes, budgets, and a membership UI.
- It fits **households and small teams** sharing one always-on core.
- It reuses the Cloud architecture as it stands: deployment-spec §10.2 already recommends **one core per tenant**. A
  multiplayer workspace is just a tenant whose core has several members. No shared multi-tenant cores (§10.7), no
  `tenant_id` everywhere.

**M2: Hosted workspaces (the paid feature).** M on top of M1, Cloud only.
- Sign-in with tern Cloud accounts, plus email invites to a workspace.
- Roles: owner, admin, member, guest.
- Per-seat billing and metering: each reply is charged to the person it answers, as Grok Bot does.
- Push to every member's phone.
- Admin policy: which connectors are allowed, whether shell is on, the autonomy ceiling.
- An audit export.
- SSO and SCIM later, for Enterprise.

### 15.4 How hard, honestly

| | Effort (rough) | Main risk |
|---|---|---|
| M0 | 1–2 weeks after T2 and T3 | concurrent posters in one session; noisy groups (mention rules handle it) |
| M1 | 5–8 weeks engine + 2–3 weeks UI | **cross-member leakage** through memory, connections or injected instructions. Every store query that today means "the owner's" has to be scoped. Migration of single-owner data to an owner member. |
| M2 | 3–4 weeks once tern Cloud exists | identity and billing plumbing. It depends on deployment-spec D-phases (accounts, edge auth, control plane), which aren't built. |

What makes it tractable:
- **Connections (§6.8)** already separate an account from its connector, so "whose Gmail" is one more foreign key.
- **Taint (§5.3)** already treats non-owner messages as untrusted.
- **Channels (§4.2)** are the natural shared room.
- **One core per tenant** means no new isolation layer in the cloud.

What makes it expensive:
- Memory scoping is threaded through prompts, tools and compaction.
- Approvals have to route to people instead of devices.
- The UI needs membership and "acting as" everywhere.

**Recommendation:**
- Keep it after stage 5, but **design T2, T3 and T4a with a member id in the schema from the start**: `author_id` on
  messages, an `owner_member` on connections, and `actor` / `requester` in the ledger, all defaulting to the owner.
  That costs almost nothing now and avoids a painful migration later.
- Ship **M0** soon after T2 and T3 to test demand cheaply.
- Sell **M2** as the multiplayer tier of tern Cloud.
- Keep **M1** in the open-source engine, so self-hosted households get it too. The paid part is the hosting,
  invites, seats and admin, not the capability. That keeps faith with "export and move back to local are features"
  (deployment-spec §1).

Open questions:
- Can a guest in an M0 group trigger any `write` effect at all? Proposed: no; writes from guest-started runs always
  go to the owner's outbox.
- In shared channels, do members see each other's tool calls, or only the answers? Proposed: tool calls are visible
  to the channel, but their inputs and outputs are redacted when they touch a connection the viewer doesn't own.
- Is the creator's need "my partner and I share one assistant" (M1, household) or "my team uses one bot" (M2)? That
  decides which level to build first. Worth asking her audience.
