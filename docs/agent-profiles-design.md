# tern agent profiles: several agents, each with its own job

Status: AP0 and AP1 built (2026-11-04; DECISIONS.md "Agent profiles AP0" and "AP1"). AP2–AP3 not built. Open questions (§9) were built with their recommended answers, still to be confirmed. This is the detailed design for agent profiles, which were only sketched in
[gui-spec.md](gui-spec.md) §7.2 (phase G5, open question 1) and in the "Later" row of
[channels-design.md](channels-design.md) §6. It builds on channels C0–C2 (Home's stream, topic channels, posts) and
T3a (taint), and on the sub-agent machinery in [subagents-design.md](subagents-design.md). Related:
[personal-agent-tooling-spec.md](personal-agent-tooling-spec.md) §3 (routines), §5.3 (taint), §8.3 (per-role models),
§15 (multiplayer).

The target experience is Grok Bot's (channels-design §8): several agents listed in the sidebar like contacts, each
with its own DM, its own job and its own memory, sharing a few facts about you. They can also join topic channels,
much like Grok Bot's group chats.

## TL;DR

- **An agent profile is a configurable bot**: a name, an avatar, a persona prompt, a default model (a T6a fallback
  chain), a tool scope (built-in tools, connectors, MCP servers, skills), a memory scope, and its own schedules. You
  can edit everything you see. The **system layer** stays fixed underneath every persona: safety, untrusted content,
  secrets, how to work with other agents, channels and sub-agents. A persona can change who the agent is, but it
  can't remove those rules.
- **The setup you have today becomes the default agent, `tern`.** Its persona is still `~/.tern/persona.md`, its
  memory is still `memory.md` and the shared `memories` table, and Home is still its stream. There is one additive
  migration, and nothing breaks.
- **Each agent has its own DM, and the DM is a channel.** It is a `main` channel like Home, so it gets C1's segments,
  carry-over, History, side chats and queued sends without new machinery. Opening "Research" in the sidebar is
  opening Research's channel.
- **Agents replace standalone routines as the thing you set up.** A routine becomes a job an agent does on a schedule
  ("every weekday at 8, sweep my inbox"). Its result goes to that agent's DM by default, or to a topic channel such as
  `#marketing`. The schedule engine (cron, T1a's quiet runs, caps and budgets, T1b's watches later) stays as it is:
  only who owns a schedule changes. You can also just chat with any agent.
- **Agents join topic channels as members.** Each channel has a **lead** agent that answers by default. Others answer
  when `@mentioned`, which wakes them, within a per-channel budget and a hop limit so agents can't loop.
- **Approvals still land in Home**, whichever agent asks. The asking agent's DM also shows the card when the run
  started there (open question 1).
- **Taint carries across agents** as T3a set out: a run that reads another agent's tainted post, or is woken by one,
  is tainted.

## 1. What a profile is

| Field | Stored as | Notes |
|---|---|---|
| Name | `agents.name` | Display name ("Research"). Can be renamed. |
| Slug | `agents.slug` | `[a-z0-9][a-z0-9-]{0,31}`, unique. Used for `@research`, `/agent research` and paths. Can be renamed; the id can't. |
| Avatar | `agents.avatar` (JSON `{glyph, color}`) | gui-spec §8's glyph and eight colours. NULL means generated from the id, as `AgentAvatar` does today. |
| Persona | `~/.tern/agents/<id>/persona.md` | Markdown: who the agent is, its job, its tone. A file, so it can be edited in an editor and shipped in bundles (spec §6.5). The default agent keeps `~/.tern/persona.md`. |
| Default model | `agents.model` | A `ModelChain` (a string, or a list for T6a fallbacks). NULL means `default_model`. A session's `/model` still overrides it for that session. |
| Sub-agent model | `agents.subagent_model` | Optional. The model this agent's sub-agents get when `agent_spawn` doesn't name one. NULL means `subagents.default_model`. |
| Tool scope | `agents.tools` (JSON) | §1.2. NULL means everything, which is the default agent's scope. |
| Memory scope | `agents.memory` | `shared` or `private` (§1.3). |
| Schedules | `schedules.agent_id` | Its jobs (§4). |
| Main channel | `channels.agent_id` on a `main` channel | Its DM (§3). |
| Telegram | `agents.telegram` (JSON, AP3) | An optional bot of its own (§6.4). |
| Archived | `agents.archived_at` | Archiving hides the agent and pauses its schedules. Its sessions stay in History, and its posts keep their author. Nothing is deleted. |

### 1.1 The prompt: persona on top of a fixed system layer

`prompt::system_prompt` today starts with `persona.md` and then appends fixed sections (role, memory, time,
workspace, secrets, untrusted content, sub-agents). With profiles, the persona slot reads the agent's persona file,
and everything after it stays engine-owned:

```text
<agent persona.md>                       ← user-editable, per agent
## Agents                                ← new, engine-owned (§1.4)
## Channel #slug                         ← channel instructions, as today
## About the user / Memory / What you remember   ← per memory scope (§1.3)
## Current time · Workspace · Secrets · Untrusted content · Sub-agents   ← unchanged
```

The persona editor shows the engine-owned sections read-only beneath the text box ("tern adds these to every
agent"), so it's clear what a persona can't turn off. Creating an agent starts from a template persona (§5.3), never
a blank page.

### 1.2 Tool scope

```json
{
  "builtins": "all" | "read_only" | ["read_file", "http_fetch", "memory_save", …],
  "connectors": "all" | ["hubspot", …],
  "mcp": "all" | ["browser", …],
  "skills": "all" | ["weekly-report", …]
}
```

- The host enforces it as sub-agent tool scopes are enforced today (`ToolRegistry::specs_for`): tools out of scope
  are left out of the specs and refused if called anyway. A missing key means `all`.
- Some tools are part of the system layer and always in scope: `channel_read`, `channel_post`, `history_search`,
  `secret_request`, and the agent's own memory tools.
- **A new agent gets a narrow default**: built-in tools except `shell`, no connectors, no MCP servers, all skills.
  You tick what its job needs. A template (§5.3) fills this in.
- Connections (spec §6.8, T4a) become another key, `"connections": ["gmail·work"]`, so "Inbox can use my work Gmail
  but not my personal one" is part of the scope.
- When T3's autonomy profiles land (spec §5.4), each agent gets an optional `autonomy` ceiling.

### 1.3 Memory scope

What exists today: `user.md` (about the user, capped), `memory.md` (core facts, capped), and the `memories` table
(long tail, FTS).

| | `shared` agent | `private` agent (default for new agents) |
|---|---|---|
| `user.md` | read, edit | read, edit: **always shared**. Who you are is the "few shared facts" Grok Bot keeps across bots. |
| `memory.md` | the global `~/.tern/memory.md` | its own `~/.tern/agents/<id>/memory.md` |
| `memories` rows | reads and writes `agent_id IS NULL` rows | reads its own rows plus shared rows; `memory_save` writes its own by default, `{"shared": true}` writes a shared one |
| `session_search`, `history_search` | its own channels' sessions | the same: never another agent's DM |

The default agent is `shared`, so its behaviour doesn't change. The Memory view (gui-spec §4.3) gets the scope chips it
already anticipates: Shared and one chip per private agent. T5a's memory flush writes to the agent's own scope.

### 1.4 The engine-owned "Agents" section

It's only added when there is more than one agent. It lists the other active agents in the channel the run is in, or
every agent in a DM, one line each: `@slug · Name · first line of its persona`. It says that `@slug` in a channel post
asks that agent (§5.2), and that approvals go to Home. That way an agent knows who else is around and can hand work
on without the user wiring it up.

## 2. Data model

One migration, the next free number (0010 at the time of writing; T2 C3 needs none). It's all additive, and every new
column defaults to the default agent.

```sql
CREATE TABLE agents (
  id TEXT PRIMARY KEY,              -- 'tern' (default) | 'tern:<slug at creation>'
  slug TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  avatar TEXT,                      -- JSON {glyph, color}; NULL = generated from id
  model TEXT,                       -- ModelChain (string or JSON list); NULL = default_model
  subagent_model TEXT,
  tools TEXT,                       -- JSON scope (§1.2); NULL = all
  memory TEXT NOT NULL DEFAULT 'private',   -- shared | private
  telegram TEXT,                    -- JSON {bot_token_secret} (AP3)
  archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
INSERT INTO agents (id, slug, name, memory, created_at, updated_at)
  VALUES ('tern', 'tern', 'tern', 'shared', <now>, <now>);

ALTER TABLE sessions  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- NULL read as 'tern'
ALTER TABLE schedules ADD COLUMN agent_id TEXT REFERENCES agents(id);
ALTER TABLE channels  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- main: whose DM; topic: the lead
ALTER TABLE memories  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- NULL = shared

CREATE TABLE channel_agents (       -- topic channel membership (§5)
  channel_id TEXT NOT NULL REFERENCES channels(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  added_at INTEGER NOT NULL,
  PRIMARY KEY (channel_id, agent_id)
);

-- Loop guards (§5.3): which post woke a run, and how far down a wake chain it is.
ALTER TABLE runs  ADD COLUMN woken_by INTEGER REFERENCES posts(id);
ALTER TABLE posts ADD COLUMN wake_depth INTEGER NOT NULL DEFAULT 0;
CREATE INDEX sessions_agent ON sessions(agent_id, updated_at);
CREATE INDEX posts_author ON posts(channel_id, author_id);
```

Notes:

- **Ids.** `tern` stays the default agent's id, which matches what C0 already writes into `posts.author_id` and what
  `tern_core::channels::author::AGENT` names. Other agents are `tern:<slug>`, fixed at creation: a renamed agent keeps
  its id, and old posts keep pointing at it. A sub-agent's author id becomes `<agent id>/<label>` (`tern/research-1`
  today, `tern:inbox/triage-1` under Inbox). This is the "`tern…` ids reserved for profiles" that DECISIONS.md "T2 C0"
  set aside. Member ids from multiplayer (spec §15, `owner`, a member uuid) live alongside them in the same author
  namespace without clashing.
- **Posts need no new column.** `posts.author_id` already is the agent id. A routine's posts keep
  `routine:<schedule id>`; the desktop gets the agent through the schedule, so they show as "Inbox · ◷ Reply sweep".
- **No backfill writes.** Existing rows keep `agent_id = NULL`, which the code reads as `tern`, the same pattern C0
  used for channel ids. New rows are always written explicitly. Existing topic channels get `agent_id = NULL` (lead:
  tern) and no `channel_agents` rows. The default agent is an implicit member of every channel, so today's behaviour
  holds.
- **Runs have no `agent_id`.** A run's agent is its session's agent, and every view that needs it (`RunItem`, events,
  approvals) joins once. Sub-agent sessions take their parent session's `agent_id` in `create_run`, as they already
  take its channel.

`Runtime::start_run` reads the session's agent once and freezes four things into the run: persona path, model chain,
tool scope and memory scope. These sit next to the channel instructions it already freezes. As with instructions,
editing an agent affects the next run, not one already working.

## 3. Each agent's DM

**Every agent has one `main` channel.** `home` stays the default agent's, with `agent_id = 'tern'`. Creating an agent
creates `dm-<slug>` (kind `main`, `agent_id` set, not archivable while the agent exists). Everything C1 built for Home
works per main channel because it was written against `main_session_id`:

- **Segments and carry**: cut at the same `[home] idle_hours` / `day_starts` rules, so the agent's DM reads as one
  continuous chat with dividers. The setting is renamed `[main]` with `[home]` kept as an alias.
- **Queue instead of cancel**: sending while the agent works becomes mail at its next step.
- **History and side chats**: per DM (`/v1/sessions?channel=dm-research`).
- **Stream**: `GET /v1/channels/{id}/stream` already serves any main channel.
- **Routine results**: posted into the DM by default, interleaved like routine posts in Home today (§4).

**What stays Home-only:**

- **Approvals and secret requests**: every one appears in Home, with the Home badge counting them, as C2 built. If the
  asking run belongs to an agent's DM (its main thread, a side chat there, a routine posting there, or their
  sub-agents), the same card also appears inline in that DM. It is one request, so answering in either place closes
  both. Topic channels still only show "Waiting for you in Home". This mirror is open question 1; without it, a DM
  shows a waiting line like topic channels.
- **Pointers** (`needs_you` from a topic channel, C4) go to Home, plus to the posting agent's DM when that isn't
  `tern`.

**Telegram** (§6.4) maps a DM to either a forum topic per agent or a bot per agent.

## 4. Agents and routines

Today a routine is a prompt plus a cron. It runs as "the" agent in a private session and posts to a channel. With
profiles, **a routine is a job owned by an agent**:

- `schedules.agent_id` decides persona, model, tool scope and memory for the routine's runs. A routine's session is
  still private (`source = cron`, `agent_id` set).
- Its default delivery is the owning agent's DM; it can target a topic channel instead (`schedules.channel_id`, as
  C2 built), e.g. Marketing's 9am "draft today's posts" going to `#marketing`. Posting to a topic channel adds the
  agent as a member there (§5.1).
- **You can set it up by talking to the agent.** "Every weekday at 8, check my inbox and tell me what needs a reply"
  in Inbox's DM makes Inbox call `schedule_create`, which creates the schedule owned by Inbox, delivering to Inbox's
  DM. `schedule_*` tools act on the calling agent's own schedules only, so one agent can't edit another's jobs.
- **Is this "agents replace routines"?** For the user, yes. The Routines editor moves into each agent's page (§5.3),
  and you think about "what does Inbox do every morning", not about a list of floating cron jobs. Under the hood the
  schedule engine stays: T1a's `NO_REPLY`, quiet hours, caps and budgets, and T1b's watches, gate scripts and
  initiative levels all attach to a schedule, and a schedule now always has an agent. A plain reminder with no agent
  set belongs to `tern`.
- **"Tell it to loop"** is a schedule, not a run that never ends. A run that's told to "keep checking every hour" calls
  `schedule_create` rather than sleeping: each check is a fresh, budgeted, journaled run, and a crash or restart loses
  nothing. The system layer says so.
- **T1b** migrates schedules into routines. If AP0 lands first, `agent_id` travels with the migration and T1b needs no
  separate agent work. That is the main ordering constraint (§8).

## 5. Agents in topic channels

### 5.1 Membership and the lead

- A topic channel has a **lead** (`channels.agent_id`, NULL = `tern`) and **members** (`channel_agents`). The lead
  answers your posts that mention no one: it owns the main thread, exactly as `tern` does today.
- Each member keeps **its own sessions** in the channel (channels-design §1: agents never share a session), with its
  own main thread created on first use, and gets the C3 `channel` digest of everyone else's posts. Per-agent digests
  come for free because the digest cursor (`sessions.posts_seen`) is already per session.
- **Joining**: you add an agent from the channel header (+ Agent). An agent that posts there via a routine or
  `channel_post {channel}` is added automatically. An agent can't `channel_post` into a channel where it isn't a
  member, unless you sent it there ("post this in #finances" in its DM): that case adds it as a member too, with a
  pill in the timeline ("Marketing joined").
- An agent reads (`channel_read`) only channels it is a member of, plus its own DM. It never reads other agents' DMs.

### 5.2 Who answers: `@mentions`

| Post | Who runs |
|---|---|
| Yours, no mention | the lead |
| Yours, `@research` | Research (in its thread in this channel), not the lead |
| Yours, `@research @ops` | both, in parallel, each in its own thread |
| An agent's, `@ops` | Ops, if the channel allows agent wakes and the guards below pass |
| A routine's | nobody (as in C0–C2), unless it mentions an agent, which counts as an agent's post |

- Mentions are parsed by the host from the post text (`@slug` matching a member, or any active agent for your own
  posts, which also adds that agent as a member). They're stored on the post as `mentions`, so clients highlight them
  and the digest shows them.
- A mentioned agent that's busy in the channel gets the post as queued mail (C1).
- **No "every bot decides whether to speak."** Grok Bot lets bots in a group decide who answers. Asking every member
  costs N model calls per post and makes channels noisy. The lead-plus-mentions rule is deterministic and cheap. An
  opt-in channel setting, **Let agents decide** (AP3), adds one call on the gate model (spec §3.3) that picks
  responders from the member list and their persona lines. That's one call, not N.

### 5.3 Budgets and loop guards (channels-design open question 2)

Your own posts always wake whoever they mention. Agent-started wakes are limited by:

- **Off by default.** The channel setting `agent_wakes` (off / on) decides whether agents' `@mentions` wake anyone. When
  it's off, the mention is shown and included in the digest but starts nothing.
- **Hop limit.** Your post has `wake_depth = 0`. A post written by a run that was woken by a post at depth *d* has
  depth *d + 1*. Wakes from posts at depth ≥ `max_hops` (default 2) are refused. This is what stops A→B→A ping-pong.
- **Per-channel budget.** At most `wakes_per_hour` (default 6) agent-started runs per channel, counted from
  `runs.woken_by`, plus an optional `usd_per_day` using T1a's budget code. When it's hit, one quiet pill ("@ops not
  woken: #launch's hourly wake budget is used up") and no run.
- **One wake per agent per post**, deduped by `wake:<post>:<agent>`, so a replayed or re-posted step never wakes
  twice.

### 5.4 Group mechanics reused

Posts, the digest, `channel_post`, queued sends and member live lines (C4) all work per agent unchanged. What
profiles add is: authors become real agents with avatars, the lead and member rules, and mention wakes.

## 6. Interactions

### 6.1 Sub-agents and delegating to another profile

- **Ordinary sub-agents are unchanged**: `agent_spawn` gives an anonymous helper under the parent agent's persona, a scope no wider than the parent's, its `subagent_model`, and the parent's memory scope.
- **Delegating to a profile** (AP3): `agent_spawn {"agent": "research", …}` runs the child **as Research**, with
  Research's persona, model, tool scope and memory scope, as a sub-agent run of the caller (same journal tree, inbox,
  budget, depth limit, and report back to the parent). This is what makes a profile useful as a specialist ("Inbox,
  ask Research to look up this sender").
  - Allowed only to agents listed in the caller's `delegates` (a profile field, empty by default), so a narrow agent
    can't route around its scope by default. The user decides which handoffs exist.
  - Taint is inherited (T3a: spawned by a tainted run → tainted), so a tainted Inbox asking Ops to send an email still
    gets an approval for the write.
  - The child's posts have author `tern:research` with a "for Inbox" chip.
- This settles subagents-design open question 4: the `[subagents].models` aliases stay for model choice, and "spawn
  profile X" is the per-role-agent form of it.

### 6.2 Taint

T3a's rules apply per run, and that's enough across agents:

- A digest step (C3) that includes a tainted post taints the reading run, whichever agent wrote the post.
- **A run woken by a tainted post starts tainted**: `NewRun.taint = "woken by a tainted post from Inbox"`. This hook
  exists already. So an injected "@ops wire $500" in Inbox's email summary reaches Ops tainted, and Ops's writes ask.
- A delegated child of a tainted run is tainted (§6.1).
- Private memory doesn't leak through the digest: agents see each other's posts, never each other's memory or
  sessions.
- **One gap to note**: with a shared workspace (open question 8), a tainted agent can write a file that a clean agent
  reads later through `read_file`, which is not untrusted. That gap already exists between runs of one agent and isn't
  made worse here, but per-agent workspaces would close it.

### 6.3 Per-role models

Spec §8.3's `models.main` / `models.subagent` become per agent: `agents.model` for the agent's runs (and routines),
and `agents.subagent_model` for its helpers. `models.gate` stays global. T6a's chains work unchanged
(`Config::fallbacks_for` keys on the chain's first model). A fast, cheap agent for triage and a heavy agent for
research is then two profiles, not a config trick.

### 6.4 Telegram

| | One bot (AP1) | A bot per agent (AP3) |
|---|---|---|
| Setup | none beyond today | one BotFather token per agent (`agents.telegram.bot_token_secret`) |
| Feel | `/agent research` switches the chat to Research's DM (shown as `Research ·` prefixes); `/agents` lists them. With topics enabled, each agent's DM is a forum topic. | each agent is its own contact in Telegram, the closest thing to Grok Bot's sidebar on a phone |
| Engine | one poller; the chat maps to a main channel instead of always `home` | one poller per token; pairing stays per Telegram user id, so pairing once trusts you on every bot |

Recommendation: AP1 ships one bot with `/agent` (and topics where available). Per-agent bots are an AP3 option.
Either way, approvals go to the chat that started the run, as today.

### 6.5 Multiplayer (spec §15)

Agents and members are orthogonal: a member talks to agents, and an agent acts for whoever asked. Author ids already
separate them (`owner` / member ids vs `tern…`). M1's memory scopes (`personal(member)`, `channel`, `core`) compose with
the agent scope: a private agent's memory is personal to the owner. Nothing in this design blocks M0 or M1.

## 7. Front ends

### 7.1 API

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/agents` | `[{id, slug, name, avatar, model, subagent_model, tools, memory, delegates, main_channel_id, last_activity, needs_you, active_runs}]` |
| POST | `/v1/agents` | `{name, slug?, avatar?, persona?, model?, tools?, memory?, template?}`; creates the DM channel |
| PATCH | `/v1/agents/{id}` | any field; `archived: true` archives |
| GET / PUT | `/v1/agents/{id}/persona` | the Markdown; GET also returns the engine-owned sections as `system_sections` for the read-only preview |
| GET / POST / DELETE | `/v1/channels/{id}/agents[/{agent}]` | members; PATCH `/v1/channels/{id}` gains `lead`, `agent_wakes`, `max_hops`, `wakes_per_hour` |
| GET | `/v1/schedules?agent=` | an agent's jobs; create and update take `agent` |
| GET | `/v1/memories?agent=` | scope filter |

Events: `agents_changed {}` as a new `Change` variant; run events, `approval_requested` and `RunItem` gain `agent_id`.
`/v1/health` reports the `agents` feature; clients without it see one agent, as now.

### 7.2 Desktop

Sidebar, top to bottom (today: the lone `AgentAvatar('tern')` row, then ⌂ Home, then CHANNELS):

```text
 ⌂ Home  (tern's avatar)       2      ← tern's DM + everything that needs you
 AGENTS                        +
 (◕) Research                  •      ← unread dot; spinner while it works
 (◔) Inbox                     ◌
 (◑) Marketing
 CHANNELS                      +
 # marketing                          ← Marketing and tern are members
 # finances
 LIBRARY …
```

- **The top avatar row goes away** and Home carries tern's avatar. Home is the default agent's DM, so listing tern
  again under AGENTS would show the same stream twice. Right-click on Home (or its header's ⋯) → **Edit tern**.
- **An agent row** opens its DM: the same view as Home (stream, dividers, New topic, History, side chats), with the
  agent's avatar and name in the header, its model picker, and **Edit**.
- **The side panel** (gui-spec §2.3) becomes per agent, as it was always drawn: Activity, **Routines** (this agent's
  jobs, with "+ New routine" prefilled with the agent), Tools (its scope, with status), and Memory (its private
  memories, plus shared memories behind a chip).
- **Create / edit agent** is one sheet: name, avatar (glyph and colour picker, preview), template, persona editor with
  the read-only system sections below it, model (chain) picker, tool scope (grouped checkboxes: built-in tools,
  connectors, MCP servers, skills, each showing status and missing secrets), memory (Private / Shared, with one line
  saying what each means), delegates (AP3), and Archive. **Templates**: Research (web and files, read-only, private
  memory), Inbox (mail connectors, a morning routine), Marketing (drafting, a `#marketing` routine), Blank. A template
  just fills the sheet.
- **Topic channel header** gains the member avatars (lead first, marked "lead"), + Agent, and
  the wake settings in the channel dialog. The composer autocompletes `@` from members (and all agents, marked "will
  join").
- **The quick switcher** (⌘K) lists agents first.

### 7.3 TUI

`/agent <slug>` switches the composer to that agent's DM, `/agents` lists agents with unread and needs-you counts, and
the status line shows the current agent. Creating and editing stays in the desktop app or `tern agents` (`list`,
`new <name>`, `edit <slug>` which opens the persona in `$EDITOR`, `archive`).

## 8. Phases

Each phase ships on its own, sized like spec §12.1 (S = days, M = one to two weeks, L = more). They're numbered AP0–AP3
because P0 already names provider setup.

| Phase | Scope | Size | Needs | Ships as |
|---|---|---|---|---|
| **AP0** (built) | Migration (§2); default agent; `/v1/agents` CRUD and persona files; `start_run` freezing persona, model chain, tool scope and memory scope; memory scopes in prompt and memory tools; `schedules.agent_id` and the `schedule_*` tools acting on the caller's own; `agents` health flag; `tern agents`; TUI `/agent`. DM channels created per agent. | M | C1 (main channels) | API, CLI and TUI: several configurable agents, each with its own DM stream, and their own routines |
| **AP1** (built) | Desktop: AGENTS section, agent DM view (reusing the Home view), create/edit sheet with templates, per-agent side panel (routines, tools, memory), memory scope chips, approval mirror in the DM; Telegram `/agent` with forum topics where available. | M | AP0 | **The Grok Bot experience**: agents in the sidebar, each with a DM and its own jobs |
| **AP2** | Agents in topic channels: membership, lead, `@mention` wakes, `agent_wakes`, hop limit and budgets, taint on wake; desktop member avatars, + Agent, `@` autocomplete; routines posting to topic channels auto-join. | M | C3 (digest), C4 (member live lines) | Marketing posting into `#marketing`; asking `@research` in `#launch` |
| **AP3** | Delegating to profiles (`agent_spawn {agent}`, `delegates`); "Let agents decide" on the gate model; a Telegram bot per agent; per-agent autonomy ceiling; templates as signed bundles. | M (each part S) | AP2; T3's outbox for delegated writes at night; T1b's gate model for the router | agent-to-agent handoffs |

**Where this sits relative to T2 and stage 2** (spec §12.1, stage 3 next to T2):

1. **C3** (in progress) as planned.
2. **AP0, then AP1** next. They don't need the digest, they're the user's top priority, and AP0 has to land before T1b
   migrates schedules so routines are born agent-owned.
3. **C4**, then **AP2**, which needs both C3's digest and C4's member lines.
4. **C5** and **T1b** after that, in either order. T1b's routines carry `agent_id` from the start.
5. **AP3** after T3's outbox, so delegated writes with nobody around queue rather than fail.

AP0 and C3 touch different code (start_run and prompt vs the agent loop's digest step), so they can be built in
parallel if wanted. The one shared resource is the migration number.

## 9. Open questions

1. **Approvals in an agent's DM.** Recommended: Home stays the one place every approval lands and is counted, and the
   asking agent's DM also shows the card when the run started there (one request, answered in either). The strict
   alternative is Home only, with a waiting line in the DM like topic channels. Which do you want?
2. **Memory default for new agents.** Recommended: private, with `user.md` always shared (Grok Bot's model). The
   alternative is shared by default, which is simpler, but agents' notes mix.
3. **Telegram.** One bot with `/agent` and topics (AP1), with per-agent bots as an option later (AP3)? Or is a bot per
   agent essential from the start?
4. **Agent-to-agent wakes.** Off by default per channel, with a hop limit of 2 and 6 wakes an hour when on. Are those
   the right defaults, or should agents never wake each other until you've seen how it behaves?
5. **Should routines remain visible on their own?** This design moves them under agents (§4). Is a global "all
   routines" list still wanted (e.g. in Settings or the Tasks view, spec §8.2) for seeing everything that runs at
   night?
6. **Profiles as files or rows.** Recommended: a DB row plus `persona.md` on disk (editable in an editor, shippable in
   bundles). The alternative is a full `agents/<id>/agent.toml` for git-friendly configs; that makes the API a file
   writer and needs file watching.
7. **The default agent's identity.** Can `tern` be renamed and re-avatared like any agent (recommended: yes, the id
   stays `tern`), and should Home's label stay "Home" or become the agent's name?
8. **Workspace per agent.** v1 shares `~/.tern/workspace`. Per-agent workspace directories would close the file gap in
   §6.2 and keep agents' scratch files apart, at the cost of agents not seeing each other's files. Wanted in AP0, or
   later?
9. **How many agents.** No hard limit is planned. Grok Bot groups hold 2–6 bots. Should the app nudge at some number,
   given that every agent's routines cost money?
