# Roadmap

What tern does today, what comes next, and what it won't do. Each line links to the spec that describes it. Nothing
below "Built since v0.1" is built; order can change, and items may be cut. The tooling spec's stages
([§12.1](docs/personal-agent-tooling-spec.md#121-stages)) are the build order.

## Built (v0.1)

- One static binary for macOS and Linux; terminal chat, `tern run`, a daemon with a scheduler and a local API.
- Step journal: crash-safe runs that resume without repeating completed tool calls (`tern resume`, automatic in the
  daemon).
- Anthropic and any OpenAI-compatible endpoint; guided setup and model picker
  ([provider-setup-spec.md](docs/provider-setup-spec.md)).
- Tools: files, shell (with approvals, an OS sandbox, and a Docker option), `http_fetch`, memory, conversation search, skills, MCP
  servers, schedules from natural language.
- Luau connectors the agent writes, tests and activates only with your approval of their hosts and secrets.
- The agent can ask you for a secret without seeing it ([secret-requests-spec.md](docs/secret-requests-spec.md)).
- Telegram with pairing and approval buttons.
- Sub-agents: delegation, fan-out and coordination ([subagents-design.md](docs/subagents-design.md)).
- Remote core over TCP with TLS, QR device pairing and scoped tokens (D1 in
  [deployment-spec.md](docs/deployment-spec.md)).
- A macOS desktop app (download, daemon included): conversations, approvals, routines, run inspector, provider and secret
  settings (G0–G2 in [gui-spec.md](docs/gui-spec.md)).

## Built since v0.1 (unreleased)

From [personal-agent-tooling-spec.md](docs/personal-agent-tooling-spec.md) §12.1 unless noted.

**Safety and control**
- **Effects on every tool** (`read`, `write`, `destructive`) enforced by the host, and a money-movement deny (T0).
- **Untrusted content is tracked** (T3a): a run that reads outside content is marked, its output is wrapped as data, and
  it can't act on its own. Shell has no network by default.
- **Outbox and autonomy** (T3): writes from unattended runs wait for your approval as cards in Home, on Telegram and in
  the TUI (`/outbox`); autonomy profiles set how much a run may do alone. Approving `http_fetch` for a run, or for a
  site for good, is one click.
- **Credentials stay out of connectors** (T4b-i): the host injects them, and connectors get path allowlists and rate
  limits.
- Approvals land where the work is: cards in the run's conversation, with a pointer in Home.

**Routines and schedules**
- **Quieter schedules**: `NO_REPLY`, quiet hours, per-schedule run caps and budgets (T1a).
- **Routines** (T1b): schedules become routines with cron and one-off triggers, Luau watches that remember where they
  left off, gate scripts that skip a run, initiative levels, and digests.
- **Webhook triggers** (T1c): an outside event can start a routine.

**Memory and models**
- **Memory flush before compaction**, with capped `user.md` and `memory.md` (T5a).
- **Skill capture** (T5b): after a long, clean run tern offers once to save it as a skill; accepting writes a draft you
  review and publish (`tern skills`).
- **Provider fallback chains**: if one model fails, the next takes over (T6a).
- **A browser through an existing MCP server**, documented as a recipe (T7a).

**Start, stop and tray**
- **Start and stop without a terminal**: `tern service install`, `tern start` / `stop`, "Quit tern completely" (T8a).
- **Menu-bar icon on macOS** (`tern tray`): status, Open, Start/Quit, Start at login through System Settings › Login
  Items (T8b). **Tray icon on Linux** (StatusNotifierItem via `ksni`, XDG autostart; T8c). The desktop app's first launch asks where tern runs: this Mac, or a tern server you pair with.

**Channels and agents**
- **Channels** ([channels-design.md](docs/channels-design.md) C0–C4): Home as one stream, topic channels in the desktop
  app, agents that share a channel and read each other's posts, sub-agents that post, "needs you" pointers into Home,
  and moving conversations into channels. Conversations are flat everywhere: no reply threads or side chats. Every run
  is told where it is and where its reply goes.
- **Agent profiles** ([agent-profiles-design.md](docs/agent-profiles-design.md) AP0–AP2): several configurable agents,
  each with its own DM, persona, model list, tools, memory and routines, in the desktop app and Telegram; agents in topic
  channels that wake on `@mention`; and delegating work to another agent.

**Connections**
- **Sign in with OAuth** (T4a): the agent finds how a service signs in and proposes it; you approve. Several accounts
  per service, for connectors and MCP servers, in a Connections view and `tern connections`.
- **Chat gateways as connectors** (T4b-ii): ntfy and Slack, written in Luau, built in.
- **MCP management** from the TUI (`/mcp`), the CLI (`tern mcp`), the desktop app and chat, with no restart and no
  `config.toml` editing ([mcp-management-spec.md](docs/mcp-management-spec.md) M0–M2).

**Compute and settings**
- **Sandboxed compute**: a `python` tool on Monty, a Python subset in Rust, whose scripts call tern's read-only tools
  as functions, each call checked and journaled, so loops over a connector don't go through `shell`. Scripts run in a
  sandboxed helper process ([python-tool-spec.md](docs/python-tool-spec.md) PY0, PY1; writes from scripts are PY2).
- **A config editor** in the desktop app, backed by a settings API, so most of `config.toml` can be changed without
  opening it.
- `tern reset` clears all local data.

## In progress

Being built on other branches right now; not done until merged.

- Activity ledger with a hash chain, Tasks view, `tern security audit` (T6b).
- Signed bundles, `tern bundle`, `import openclaw` (T4b-iii).
- OpenAPI-to-connector and recorded test fixtures, for faster connector authoring (T4c).
- Agents suggest channels; `/channel` in the TUI; Telegram topics (C5).
- `tern mcp serve`: tern's tools for other agents over MCP (T6d).

## Next

| Stage | What | Spec |
|---|---|---|
| 5. Ecosystem | Recipes (morning brief, follow-ups, meeting prep) and goals (T6c). Native browser behind a feature flag (T7b). Tray on Linux (T8c, done). | tooling §6.5, §9, §10 |
| Agent profiles | AP3 is built (API, CLI, daemon): "Let agents decide" routing, a Telegram bot per agent, a per-agent autonomy ceiling, templates as signed bundles. Left: the desktop controls for them. | [agent-profiles-design.md](docs/agent-profiles-design.md) |
| 6. Multiplayer | Group rooms with guests (M0), then several members on one core (M1), then hosted workspaces (M2). | tooling §15 |

**Running it elsewhere** ([deployment-spec.md](docs/deployment-spec.md) §12): event replay and an async store (D0),
container image and server guides (D2), Postgres (D3), a mobile app with push (D4), a hosted tier with export and
leave (D5), and a relay so a home core gets phone reach without port forwarding (D6).

**Desktop app** ([gui-spec.md](docs/gui-spec.md) §10): packaging, notarization and a Linux build (G6).

## Won't do

From the tooling spec's TL;DR: a per-user VM, bundled Chromium, OS-level vision clicking, an open skill or connector
marketplace, iMessage (it needs Apple Business approval and per-user fees), or human concierges. No telemetry.
