# Roadmap

What tern does today, what comes next, and what it won't do. Each line links to the spec that describes it. Nothing
below the first section is built; order can change, and items may be cut. The tooling spec's stages
([§12.1](docs/personal-agent-tooling-spec.md#121-stages)) are the build order.

## Built (v0.1)

- One static binary for macOS and Linux; terminal chat, `tern run`, a daemon with a scheduler and a local API.
- Step journal: crash-safe runs that resume without repeating completed tool calls (`tern resume`, automatic in the
  daemon).
- Anthropic and any OpenAI-compatible endpoint; guided setup and model picker
  ([provider-setup-spec.md](docs/provider-setup-spec.md)).
- Tools: files, shell (with approvals and a Docker option), `http_fetch`, memory, conversation search, skills, MCP
  servers, schedules from natural language.
- Luau connectors the agent writes, tests and activates only with your approval of their hosts and secrets.
- The agent can ask you for a secret without seeing it ([secret-requests-spec.md](docs/secret-requests-spec.md)).
- Telegram with pairing and approval buttons.
- Sub-agents: delegation, fan-out and coordination ([subagents-design.md](docs/subagents-design.md)).
- Remote core over TCP with TLS, QR device pairing and scoped tokens (D1 in
  [deployment-spec.md](docs/deployment-spec.md)).
- A Flutter desktop app (build from source): conversations, approvals, routines, run inspector, provider and secret
  settings (G0–G2 in [gui-spec.md](docs/gui-spec.md)).

## Next: quick wins (Stage 1)

From [personal-agent-tooling-spec.md](docs/personal-agent-tooling-spec.md) §12.1.

- **Effects on every tool** (`read`, `write`, `destructive`) enforced by the host, and a money-movement deny (T0).
- **Quieter schedules**: `NO_REPLY`, quiet hours, per-schedule run caps and budgets (T1a).
- **Start and stop without a terminal**: `tern service install`, `tern start` / `stop`, "Quit tern completely" (T8a).
- Memory flush before compaction, capped profile files (T5a); provider fallback chains (T6a).
- A browser through an existing MCP server, documented as a recipe (T7a).

Also specced, not yet scheduled against the stages: **MCP management** from the TUI, CLI and desktop app instead of
editing `config.toml` ([mcp-management-spec.md](docs/mcp-management-spec.md)).

## Then

| Stage | What | Spec |
|---|---|---|
| 2. Foundations | Routines with Luau watches, gate scripts and zero-token idle; initiative levels and digests (T1b). Taint for runs started by external content, an outbox with deferred approvals, autonomy profiles (T3). Credentials injected by the host so connectors never see them, path allowlists, rate limits (T4b-i). | tooling §3, §5, §6.1 |
| 3. Organize | Channels over one shared memory, reply-to-act, Telegram topics (T2). Activity ledger, Tasks view, `tern security audit` (T6b). Menu-bar tray on macOS (T8b). Skill capture (T5b). | tooling §4, §8, §11 |
| 4. Connections | OAuth-first sign-in, several accounts per service (T4a). Webhook triggers (T1c). Chat channels as Luau gateway connectors: ntfy, then Slack (T4b-ii). | tooling §6 |
| 5. Ecosystem | Recipes (morning brief, follow-ups, meeting prep) and goals (T6c). Signed bundles, `import openclaw` (T4b-iii). `tern mcp serve` (T6d). Faster connector authoring (T4c). Tray on Linux (T8c). Native browser behind a feature flag (T7b). | tooling §6.5, §9, §10 |
| 6. Multiplayer | Group rooms, then several members on one core, then hosted workspaces. | tooling §15 |

**Running it elsewhere** ([deployment-spec.md](docs/deployment-spec.md) §12): event replay and an async store (D0),
container image and server guides (D2), Postgres (D3), a mobile app with push (D4), a hosted tier with export and
leave (D5), and a relay so a home core gets phone reach without port forwarding (D6).

**Desktop app** ([gui-spec.md](docs/gui-spec.md) §10): library of connectors, MCP servers, skills and memory (G3),
channel settings (G4), agent profiles (G5), packaging, notarization and a Linux build (G6).

## Won't do

From the tooling spec's TL;DR: a per-user VM, bundled Chromium, OS-level vision clicking, an open skill or connector
marketplace, iMessage (it needs Apple Business approval and per-user fees), or human concierges. No telemetry.
