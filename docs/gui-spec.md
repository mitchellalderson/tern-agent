# tern desktop: Flutter GUI spec

Status: draft, 2026-11-03. G0–G2 are built in `apps/desktop/` (see its README for what's in and out).

A desktop app for tern that attaches to the daemon (`tern daemon`) over its local socket API. It's a front end, like the TUI and the Telegram gateway: the engine owns all state, runs, approvals and scheduling. The app can be closed at any time without losing anything.

The layout takes after [Rakazo](https://rakazo.com/): a sidebar of agents, a conversation in the middle, and a side panel showing what the agent is doing and what it runs on a schedule. The content differs because tern's strengths differ: Luau connectors the agent writes itself, MCP servers, skills, channels (Telegram for now), a crash-safe step journal, and approvals.

## 1. Principles

"Light and native" applies to the app in two ways.

**Light (footprint and look).**
- Idle RSS under 120 MB, cold start to first frame under 400 ms, app bundle under 40 MB (Flutter's floor is around 60–80 MB RSS, so this is about not adding to it).
- No idle CPU: the event stream is push (SSE). The app never polls more often than once a minute.
- Few dependencies: no state-management framework, no codegen, no Electron-style web views. Every package must justify itself (see §9).
- A light visual theme by default: white and near-white surfaces, one accent colour, hairline dividers, plenty of space. Dark mode follows the system.

**Native.**
- macOS first: transparent title bar with traffic lights inset into the sidebar, vibrancy on the sidebar, SF Pro via the system font, native menu bar (`PlatformMenuBar`), standard shortcuts, native notifications with action buttons, a menu bar extra.
- Linux second (GTK window, libnotify). Windows is out until the daemon has a non-Unix transport.
- Behaves like a Mac app: ⌘N new conversation, ⌘K quick switcher, ⌘, settings, ⌘W closes the window but the daemon keeps working, window size and selection restored on launch.

**Engine-first.** The app never reads `~/.tern` files or the SQLite database directly. Anything it needs that the API lacks is added to the API (§7), so the TUI and future clients benefit too.

## 2. Window layout

Three columns, as in the reference. The right panel is collapsible (⌘⌥I) and remembers its state per window.

```
┌──────────────┬──────────────────────────────────┬──────────────────────┐
│ ● ● ●      + │ (◕) Research          ⌄ sonnet ▢ │ Activity        ⚙  ✕ │
│ ⌕ Search  ⌘K │                                  │ ▸ run 3f2a · 00:42   │
│              │  you: find seed-stage fintechs…  │   ✓ http_get  …      │
│ AGENTS       │                                  │   ✓ memory_save      │
│ (◕) Research │  ┌ Worked for 42s ──────────────┐│   ◌ shell  running   │
│ (◔) Inbox  • │  │ ✓ Searched  → 40 accounts    ││                      │
│ (◑) Ops      │  │ ✓ Drafted   → 18 emails      ││ Routines             │
│              │  │ ✓ Skipped   → 4 in pipeline  ││ ◷ Overnight  1:00 am │
│ CHANNELS     │  └──────────────────────────────┘│ ◷ Reply sweep  8 am  │
│ ✈ Telegram ● │  ◷ Created routine Overnight      │ + New routine        │
│              │                                  │                      │
│ ──────────── │  two of them raised last week…   │ Connectors           │
│ Connectors   │                                  │ ⬡ hubspot   active   │
│ Skills       │  ┌ Approval ─────────────────────┐│ ⬡ crm_sync  draft    │
│ Memory       │  │ shell: git push origin main   ││                      │
│              │  │ [Allow once] [Always] [Deny]  ││ Memory (3 new)       │
│ ● daemon ok  │  └──────────────────────────────┘│                      │
│              │ ┌ + Message Research          ↑ ┐│                      │
└──────────────┴──────────────────────────────────┴──────────────────────┘
```

### 2.1 Sidebar

- **Search** (⌘K): a quick switcher over agents, conversations, connectors and skills, backed by session search (FTS) for message text.
- **Agents**: avatar (generated glyph plus colour), name, relative time, and the last line of the latest assistant message or tool summary. A dot marks unread; an amber dot marks a pending approval. Selecting an agent opens its most recent conversation. Agents need an engine change (§7.2); until it lands there is one agent, "tern", and this section lists its conversations instead.
- **Conversations** (under the selected agent, expandable): sessions grouped Today / This week / Older, filterable by source (`api`, `telegram`, `cron`). Scheduled-run sessions show the ◷ icon.
- **Channels**: Telegram, with paired-chat count and gateway health. Room for more gateways later.
- **Library**: Connectors, Skills, Memory open full-width views in the centre column (§4).
- **Footer**: daemon status (connected, version, model) instead of a user profile. Click for details and Restart daemon.

### 2.2 Conversation (centre)

Header: agent avatar and name, a model picker (`PUT /v1/sessions/{id}/model`, validated with `/v1/models/check`), and a toggle for the right panel.

Transcript items, in order of importance:

1. **User and assistant messages.** Assistant text renders Markdown (headings, lists, code with copy button, tables, links). Text streams in from `text_delta`; the final `assistant_message` replaces the streamed buffer.
2. **Work card.** All tool calls between two assistant messages fold into one card ("Worked for 42s", one `✓ name → summary` line per tool, `✗` for errors, a spinner for the running one). Click a line to expand its output (the 32 KB `output` from `tool_finished`). This is the reference's checklist card, built from real tool events instead of model prose.
3. **Event pills.** Centred, quiet, one line: `◷ Created routine Overnight sourcing`, `⬡ Activated connector hubspot`, `✎ Saved to memory`, `⇣ Compacted earlier messages`, `↻ Retrying (2/5) in 4s`. Derived from tool names (`schedule_create`, `connector_activate`, `memory_save`) and from `compacted` / `retrying` events. Each links to the thing it made.
4. **Approval card.** Tool, summary, the full input (collapsed JSON), the allowlist pattern that "Always" would add, and Allow once / Always / Deny. Keyboard: ⏎ once, ⌥⏎ always, ⎋ deny. After answering it collapses to one line showing the decision.
5. **Run footer.** Failed or interrupted runs show the error and a Resume button (`POST /v1/runs/{id}/resume`).

Composer: multiline (⏎ send, ⇧⏎ newline), a Stop button while a run is active (`/cancel`), and a `+` menu for attaching a file (copied into the workspace and referenced by path) and inserting a skill. Sending while a run is active follows the engine rule: the current run is cancelled and the new message starts a new run. The composer says so ("Sending will stop the current run").

### 2.3 Side panel (right)

Rakazo shows the bot's remote computer here. tern has no remote desktop, so the panel shows what tern actually does:

- **Activity**: the active run as a live step list (tool, summary, duration, status), with token usage from `usage` events. Clicking opens the Run inspector (§4.4). When idle it shows the last run's outcome.
- **Routines**: this agent's schedules (cron rendered in words: "Weekdays 8 am"), next run time, last result, and a toggle. "+ New routine" opens a sheet with a prompt, a schedule (presets plus a cron field with a live "next 3 runs" preview) and a delivery target (`none`, this app, a Telegram chat).
- **Connectors and tools**: what this agent can use, with status (active, draft, MCP server up/down).
- **Memory**: recent memories this agent saved, with edit and delete.

In M6 or later, if the sandbox exposes a workspace, the panel can gain a **Workspace** tab: a file tree of the agent's working directory with Quick Look previews. That's the nearest honest analogue to "the bot's computer". No screen streaming.

## 3. Channels

Channels are where an agent can be reached other than this app. Today that's Telegram.

- **Telegram view**: bot status (polling, last update, errors), paired chats with their names and the agent each routes to, and Pair a chat, which shows the 8-character code, a countdown to expiry, and a QR code for `https://t.me/<bot>?start=<CODE>`. Unpairing asks first.
- **Routing** (needs §7.2): each paired chat maps to one agent. Scheduled-run delivery picks from the same list.
- **Setup**: if no bot token is configured, the view walks through BotFather, takes the token into a secure field and stores it with `PUT /v1/secrets/{name}` (write-only; the app never reads secrets back).

Open: the follow-up notes mention "channels for organizing information streams" in the style of Claude Duet. That's a different idea (topic streams rather than transports), designed in [channels-design.md](channels-design.md).

## 4. Library views

These open in the centre column and replace the conversation until you pick a conversation again.

### 4.1 Connectors

The distinctive part of tern, so it gets real care.

- **List**: active connectors and drafts, each with tools, declared HTTP hosts, secrets (present or missing) and validation errors, matching `tern connectors list`.
- **Detail**: the Luau source in a read-only code view with syntax highlighting, its capabilities as chips (`api.hubspot.com`, `*.slack.com`, `secret: HUBSPOT_TOKEN`), its tools with their JSON Schemas rendered as forms, and a **Try it** panel: fill the form, run the tool (`connector_test` semantics), see the result. Running a draft that declares hosts or secrets goes through the usual approval.
- **Activate** shows the same capability summary as the approval prompt, then activates.
- **Ask the agent**: a button that opens a conversation pre-filled with "Write a connector for …". Agent-authored drafts appear here live.
- **Editing** source in-app is later (P2). For now Reveal in Finder / Open in editor.

### 4.2 MCP servers

A list of configured servers: transport (stdio or HTTP), state (not started, running, unavailable with retry countdown), the tools they expose (`mcp__server__tool`), and missing `env_secrets`. Adding a server is a form that writes config through the API (§7.1); Restart forces a reconnect.

### 4.3 Skills and memory

- **Skills**: name, description, files, rendered `SKILL.md`. Reveal in Finder, and "Use in conversation" inserts a mention.
- **Memory**: searchable list (FTS) with source session links, edit, delete, and add. Scope chips once agents exist (shared or per-agent).

### 4.4 Run inspector

The GUI version of `tern runs show`: a timeline of journal steps (llm, tool, approval, compaction) with inputs, outputs, durations, token usage, retries and replay markers. Opened from a work card or the Activity panel. A sheet, not a new window.

### 4.5 Settings (⌘,)

Tabs: **Models** (providers, API key secrets, default model, context window), **Approvals** (allowlist patterns, delete), **Daemon** (status, logs folder, run at login, restart), **About** (versions, doctor results from §7.1).

## 5. Notifications and the menu bar

- Native notification when a run finishes while the app isn't focused on that conversation, and always for approvals. Approval notifications carry Allow once / Deny actions that call the API directly.
- Menu bar extra: superseded by `tern tray`, a separate native process that outlives the app window ([personal-agent-tooling-spec.md](personal-agent-tooling-spec.md) §11, phase T8). The app quits when its window closes and doesn't post notifications while a tray is attached.
- Dock badge = pending approvals.

## 6. Connecting to the engine

### 6.1 Transport

The daemon serves JSON over HTTP on `~/.tern/tern.sock` (mode 0600), possibly a symlink into `/tmp/tern-<uid>/`. Dart's `HttpClient.connectionFactory` can return `Socket.startConnect(InternetAddress(path, type: InternetAddressType.unix), 0)`, so plain `dart:io` covers both requests and the SSE stream with no plugin. Resolve the symlink before connecting.

`TERN_HOME` is honoured the same way the CLI does, and the socket path can be overridden in Settings for development.

### 6.2 Daemon lifecycle

1. On launch, `GET /v1/health`. If it answers, check `version` against the app's supported range (§7.3).
2. If nothing answers, show a calm empty state: "tern isn't running" with **Start** (spawns `tern daemon` detached) and **Start at login** (installs a LaunchAgent / systemd user unit). The `tern` binary is found on `PATH`, at a configured path, or bundled in the app (`Contents/Resources/tern`).
3. On disconnect, the UI keeps showing the last state greyed out, reconnects with backoff (0.5 s doubling to 10 s), and resyncs (§6.3).

### 6.3 State and events

- One SSE connection (`/v1/events`) per app. Events are reduced into an in-memory store keyed by session and run. `run_started` maps `run_id` to `session_id`; later events only carry `run_id`.
- On connect and after every reconnect: refetch the session list, the open session, active runs and pending approvals. SSE has no replay, so this resync is how missed events are recovered. §7.1 adds the endpoints that make it complete.
- `replayed: true` messages and tool results (after a daemon crash and resume) are already in the transcript from the session fetch, so they're deduplicated by tool `id`, not appended.
- The app is the API approval route while attached. If the app is closed, the engine denies (current behaviour), so the app should be explicit: Settings shows "With tern closed, approvals from the app are denied. Telegram approvals still work."

### 6.4 Client package

`apps/desktop/packages/tern_client`: a pure Dart package with the transport, typed models for every API type, and the SSE parser. No Flutter dependency, so it's unit-testable and reusable (a future mobile or CLI client). Models are hand-written. A Rust test writes a JSON example of every API type to `apps/desktop/packages/tern_client/test/fixtures/`, and a Dart test parses each one, so drift between the two shows up in CI.

## 7. Engine changes the GUI needs

### 7.1 API additions (P0 unless noted)

| Method | Path | Purpose |
|---|---|---|
| GET | `/v1/runs/active` | running runs with their session ids (resync) |
| GET | `/v1/approvals` | pending approvals (resync; today a late-attaching client never sees them) |
| GET | `/v1/runs/{id}/steps` | journal steps for the inspector (built, with `GET /v1/sessions/{id}/runs`) |
| GET | `/v1/sessions?source=&q=` | filter by source; full-text search |
| PATCH / DELETE | `/v1/sessions/{id}` | rename, archive |
| GET / POST / PATCH / DELETE | `/v1/schedules[/{id}]` | routines CRUD, enable toggle, last result (built, plus `POST /v1/schedules/preview` and `/{id}/run`) |
| GET | `/v1/connectors` | active and drafts, capabilities, missing secrets, errors |
| GET | `/v1/connectors/{name}/source` | Luau source |
| POST | `/v1/connectors/{name}/test` | run a tool (routes through approvals) |
| POST | `/v1/connectors/{name}/activate` | activate (routes through approvals) |
| GET | `/v1/mcp` / POST `/v1/mcp/{name}/restart` | server status, restart |
| GET | `/v1/skills` | skills list and bodies |
| GET / POST / PATCH / DELETE | `/v1/memories[/{id}]` | memory CRUD and search |
| GET / DELETE | `/v1/approvals/allowlist[/{id}]` | allowlist management |
| GET | `/v1/gateway/telegram` | status, paired chats |
| POST / DELETE | `/v1/gateway/telegram/pairings[/{id}]` | create a code, unpair |
| PUT | `/v1/secrets/{name}` | write-only |
| GET / PATCH | `/v1/config` (P1) | read and edit config through the same validation as `tern config set` |
| GET | `/v1/doctor` (P1) | doctor results as JSON |
| GET | `/v1/tools` | full tool specs `[{name, description, input_schema}]` (built) |
| GET | `/v1/runs/{id}/children[?recursive=1]`, `/v1/sessions/{id}/subagents` | sub-agent runs as `ChildRun` (built, S2; [subagents-design.md](subagents-design.md) §3.2) |
| POST | `/v1/runs/{id}/message` | `{text}` from the user to a running sub-agent (built, S3) |
| GET | `/v1/sessions/{id}/runs?include=subagents`, `/v1/sessions/{id}/usage?include=subagents` | run trees and their usage; `/v1/sessions` hides sub-agent sessions unless `?source=subagent` (built, S2) |

Event changes:
- Add `session_id` to every `AgentEvent`, so a client that attaches mid-run can place events without a lookup. (Built: the SSE layer adds it, so `AgentEvent` itself is unchanged.)
- New events: `session_updated` (title, model), `schedule_changed`, `connector_changed`, `memory_changed`. `approval_resolved` turned out to be unneeded: the existing `approval` agent event fires whoever answers, and clients close the card on it.
- Add an `id` to each SSE event, ready for `Last-Event-ID` replay later.
- Built: `tool_output` (live output chunks of a running tool, matched to `tool_started` by `id`; never replayed), and for sub-agents ([subagents-design.md](subagents-design.md) §3.1) `subagent_spawned` (on the parent's run), `subagent_message` (`question`, `answer` or `message` between runs of a tree), `parent_run_id` and `root_session_id` on every event of a sub-agent run, and `parent_run_id`, `agent_label`, `root_session_id` on a sub-agent's `approval_requested` and in `/v1/approvals`. `/v1/health` reports the `subagents` feature.

### 7.2 Agents (P1)

Detailed design: [agent-profiles-design.md](agent-profiles-design.md) (profiles, per-agent DMs as channels, agents owning routines, agents in topic channels, phases AP0–AP3). The sketch below is kept for context.

The reference's sidebar is a list of agents with different jobs. tern has one agent with many sessions. Proposal: an **agent profile** is a name, avatar, persona (Markdown), default model, and a tool scope (which connectors, MCP servers and skills it can use). Sessions, schedules and channel routes gain an `agent_id`; the existing setup becomes the default agent, so nothing breaks. This is also where the planned per-role models (a fast main agent handing harder work to heavier sub-agents) would live.

The GUI ships before this (phase G1–G3 list conversations under the single default agent) and gains the Agents section when the engine supports it.

### 7.3 Versioning

`/v1/health` adds `api: 1` plus a list of capability flags (`schedules`, `agents`, …). The app hides features the daemon doesn't report rather than failing.

### 7.4 Remote access (later)

The socket is local only, which is right for now. A phone or second machine needs a TCP listener with a token, or a relay. Designed in [deployment-spec.md](deployment-spec.md) §6–§8 (listeners, device pairing, connection profiles, push), which supersedes this section and §6.1. Noted here so the client package doesn't assume Unix sockets everywhere.

## 8. Visual design

- **Palette (light)**: background `#FFFFFF`, sidebar `#F6F6F4` with vibrancy, cards `#F7F7F5`, hairlines `#E7E7E3`, text `#1C1C1A` / secondary `#6B6B66`, one accent (warm orange `#F07A2B`, from the tern mark) for primary buttons and the send button, green `#2F9E5B` for ✓, amber `#D9902B` for approvals, red `#D2453B` for errors. Dark mode is a separate token set, not an inversion.
- **Agent avatars**: a rounded glyph (circle, drop, heart, flower, in the reference's spirit) in one of eight colours with two eye dots. Generated from the agent id and editable.
- **Type**: system font. 15 pt body, 13 pt secondary, 12 pt monospace for tool output and code. Assistant text is not bubbled; user messages sit in a light grey bubble on the right; work cards and approvals are rounded 12 px cards.
- **Motion**: short and functional only: streaming caret, spinner on the running tool, 150 ms card expand. Respect Reduce Motion.
- **Density**: 12 px sidebar rows padding, 720 px max transcript width, centred.
- Before building, make a clickable mock of the main window (light and dark) and agree on it.

## 9. Implementation

### 9.0 Why Flutter (and what it costs)

Flutter compiles Dart ahead of time to native code and draws its own widgets, so one codebase covers macOS, Linux, iOS and Android at native speed. The costs: the engine puts a floor of roughly 35–45 MB on the app bundle and 60–100 MB on idle memory, and "native" is imitated (custom-drawn widgets plus window plugins), not AppKit. Alternatives considered:

- **SwiftUI**: the most native and smallest (a few MB), shares code with an iOS app. No Linux or Android.
- **Tauri 2**: Rust like the engine, small bundle (~10 MB) using the system web view. Web UI rather than native controls; mobile support is younger.
- **Rust-native toolkits (gpui, Slint, egui)**: small and fast, but weak or missing mobile support, and egui doesn't look native.

Flutter fits "desktop first, mobile later, one codebase". If mobile ends up meaning iOS only and the native feel matters most, SwiftUI is the stronger choice. Either way the contract is the socket API, so the client can be swapped without engine changes.

- **Location**: `apps/desktop/` (Flutter app) and `apps/desktop/packages/tern_client/` (Dart). Outside the Cargo workspace; its own CI job (format, analyze, test, macOS build).
- **State**: `ChangeNotifier` stores plus `ListenableBuilder`, one store per domain (connection, sessions, runs, library). No Riverpod, Bloc or codegen; revisit only if this gets painful.
- **Packages** (each one justified; check maintenance status when starting, since some Markdown packages have been discontinued):
  - `macos_window_utils` (title bar, vibrancy) and `window_manager` (size and position on Linux). The MVP needs neither: `MainFlutterWindow.swift` sets the transparent title bar, and a `tern/window` method channel handles drag and zoom.
  - a maintained Markdown renderer (MVP: `flutter_markdown_plus`; `flutter_markdown` is discontinued) plus a syntax highlighter for code and Luau
  - `tray_manager` (menu bar extra), `flutter_local_notifications` (actionable notifications)
  - `qr` for the pairing code
- **Testing**: `tern_client` unit tests against the fixtures (§6.4) and against a real daemon running the mock provider (the same scripted conversations the Rust e2e tests use). Widget tests for transcript reduction (events in, items out), the approval card and the work card. One integration test on macOS that starts a daemon with `TERN_HOME` in a temp dir, sends a message, answers an approval and sees the reply.
- **Packaging**: signed and notarized `.app` in a DMG, optionally bundling the `tern` binary. Linux: AppImage or Flatpak later.

## 10. Phases

Each phase ends usable, like the engine milestones.

| Phase | Scope | Engine work |
|---|---|---|
| G0 | Shell: window chrome, sidebar, connect to the socket, daemon status, start daemon | `/v1/health` capability flags |
| G1 | Chat: sessions list, transcript, streaming, work cards, approvals, cancel, resume, model picker, notifications | `runs/active`, `approvals` list, `session_id` on events, `approval_resolved` |
| G2 | Side panel: activity, routines CRUD, run inspector | schedules API, `runs/{id}/steps` |
| G3 | Library: connectors (try and activate), MCP, skills, memory, settings | connectors, mcp, skills, memories, allowlist, secrets, config APIs |
| G4 | Channels: Telegram status, pairing with QR, routing | gateway API |
| G5 | Agents: sidebar of agent profiles, per-agent routines, memory scope, channel routing | §7.2; [agent-profiles-design.md](agent-profiles-design.md) AP1 |
| G6 | Polish: menu bar extra, quick switcher, packaging and notarization, Linux build | — |

## 11. Open questions

1. **Agents**: is a first-class agent profile (§7.2) the direction, or should the sidebar stay a list of conversations? It's the biggest change this spec implies for the engine. Proposed answer: yes, designed in [agent-profiles-design.md](agent-profiles-design.md).
2. **"Channels"**: Telegram-style transports (as specced in §3), Claude Duet-style information streams, or both? If the latter, what does a stream contain and who writes to it?
3. **Bundling**: should the app ship the `tern` binary and manage the daemon itself, or assume a separate install?
4. **Platforms**: macOS-only for v1, or is Linux a requirement from the start?
5. **Approvals while closed**: keep "deny when no client", or let approvals wait (with a timeout) so a notification can bring the user back?
