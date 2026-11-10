# Teams design: several people, one core, shared channels and agents

Status: draft for owner review, 2026-11-10. Design only; no feature code. Builds on:

- [deployment-spec.md](deployment-spec.md) §10.4a (Clerk accounts, built in v0.1.0-alpha.4) and §10.2 (one core per tenant).
- [personal-agent-tooling-spec.md](personal-agent-tooling-spec.md) §15 (multiplayer research and levels M0 to M2). This doc is **M1 plus the invite and admin half of M2**.
- [channels-design.md](channels-design.md) and [agent-profiles-design.md](agent-profiles-design.md) (channels, agents, `@mentions`, memory scopes).
- Earlier drafts that live only on old branches: `docs/accounts-design.md` (branch `worktree-agent-a4bb32e18df039cf7`) and `docs/transport-design.md` (`worktree-agent-abae3bf82f3ad48ca`). Both already say "multiplayer adds `members` on the tenant; handlers read identity only from `Auth`". This design keeps that promise.

Vocabulary: a **core** is one tern daemon with one SQLite store. A **user** is a person (one Clerk identity). A **team** is the group of users on a core. A **principal** is who a request is for: a user, plus the device or channel it came through. The **requester** of a run is the human whose message or action started it.

## 0. TL;DR

1. **One core hosts one team in v1.** The tables carry `team_id` so more teams per core stay possible, but v1 enforces exactly one. The team switcher is the existing server switcher, labelled with the team name. This keeps the isolation boundary where it already is (a core's store, secrets, workspaces, memory) and avoids scoping every query by tenant.
2. **Teams, roles and invites live in tern's own tables**, not in Clerk Organizations. Clerk stays the identity provider only (`sub`, verified `email`). Reasons in §5.1.
3. **Two roles in v1: admin and member.** The creator is the first admin; "owner" (billing, cannot be removed) and "guest" (read or post-only) are designed in but deferred (§3).
4. **One policy layer**: `tern_core::policy::decide(principal, action, resource)`, a pure function with a table-driven test, called from the daemon API, the scheduler, the gateways and the run loop. UIs hide things; the core enforces. Repos take a `Viewer`, so an unscoped list is a compile error (§2.1).
5. **Per-person authority, shared room.** A shared agent in a channel acts **for whoever asked**: the requester's connections and secrets, the requester's approvals, the requester's budget. Personal memory never enters a run another member started. Team memory is separate (§2.3).
6. **Local mode is untouched.** No `[account]`, or no `[teams]` flag: the principal is `LocalOwner`, which the policy allows everything. Teams are off until the owner sets `[account] teams = true` (§1.5).
7. **Smallest alpha slice** (§8.2, "alpha-5 Teams A"): teams, invites, admin/member, share toggle on agents and channels, per-requester credentials, memory isolation, approvals to the requester, attribution in posts and ledger. Presence, human mentions, Telegram for non-owners, per-user budgets and the TUI follow.

## 1. Data model and migrations

The store is SQLite through `rusqlite`, with forward-only migrations in `crates/tern-store/migrations/` (latest today: `0030_waits.sql`). Teams add **0031 to 0034**, all additive. Every existing row reads as "owned by the owner, private", which is exactly how a local install behaves today.

### 1.1 Identity and team tables (migration 0031)

```sql
CREATE TABLE users (
  id TEXT PRIMARY KEY,            -- 'owner' for the existing owner (the id already used in posts.author_id and
                                  -- connections.owner_member, so no data rewrite), else 'u_<uuid7>'
  handle TEXT NOT NULL UNIQUE,    -- [a-z0-9][a-z0-9-]{0,31}; the @mention name. Shares a namespace with agent slugs (§4.4)
  display_name TEXT NOT NULL,
  email TEXT,                     -- mutable, refreshed at sign-in, never a lookup key
  created_at INTEGER NOT NULL,
  disabled_at INTEGER,            -- removed or suspended; the row stays so attribution never dangles
  deleted_at INTEGER              -- tombstone after retention purge (§7.5); name/email scrubbed
);

CREATE TABLE identities (         -- how a login maps to a user
  provider TEXT NOT NULL,         -- 'clerk'
  subject TEXT NOT NULL,          -- Clerk user id (user_...)
  user_id TEXT NOT NULL REFERENCES users(id),
  linked_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject)
);

CREATE TABLE teams (
  id TEXT PRIMARY KEY,            -- 'team' in v1 (the one team); 'tm_<uuid7>' later
  name TEXT NOT NULL,
  autonomy_ceiling TEXT,          -- NULL = no team ceiling (§2.4)
  settings TEXT,                  -- JSON: member_tools, personal_in_team, member_default_visibility, retention_days
  created_by TEXT NOT NULL REFERENCES users(id),
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE memberships (
  team_id TEXT NOT NULL REFERENCES teams(id),
  user_id TEXT NOT NULL REFERENCES users(id),
  role TEXT NOT NULL,             -- 'admin' | 'member' (validated in code, not CHECK, so 'owner'/'guest' are additive)
  status TEXT NOT NULL DEFAULT 'active',   -- active | removed
  added_by TEXT REFERENCES users(id),
  joined_at INTEGER NOT NULL,
  removed_at INTEGER, removed_by TEXT REFERENCES users(id),
  PRIMARY KEY (team_id, user_id)
);
-- Last-admin backstop (§7.6): refuses a change that leaves a team with no active admin.
CREATE TRIGGER memberships_last_admin BEFORE UPDATE OF role, status ON memberships
WHEN OLD.role = 'admin' AND OLD.status = 'active' AND (NEW.role <> 'admin' OR NEW.status <> 'active')
 AND (SELECT COUNT(*) FROM memberships
      WHERE team_id = OLD.team_id AND role = 'admin' AND status = 'active' AND user_id <> OLD.user_id) = 0
BEGIN SELECT RAISE(ABORT, 'a team needs at least one admin'); END;

CREATE TABLE invites (
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  email TEXT NOT NULL,            -- the address the invite is for (lowercased)
  role TEXT NOT NULL,             -- the role granted on accept; 'admin' needs an admin inviter
  token_hash TEXT NOT NULL UNIQUE,-- blake3 of the secret in the link; shown once
  invited_by TEXT NOT NULL REFERENCES users(id),
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,    -- 7 days
  accepted_by TEXT REFERENCES users(id), accepted_at INTEGER,
  revoked_at INTEGER
);
```

`users.id = 'owner'` for the first user is the central migration trick. Today `posts.author_id`, `messages.author_id` and `connections.owner_member` already say `'owner'` (channels-design and tooling spec §15.4 put that in the schema on purpose). So the migration **creates** the owner user instead of rewriting rows.

### 1.2 Ownership and visibility (migration 0032)

Visibility has two values in v1: `private` (the owner, plus anyone added to a channel by name; admins manage it but do not read it, §2.2) and `team` (every active member).

```sql
-- Agents: who owns, who can use.
ALTER TABLE agents ADD COLUMN owner_user_id TEXT REFERENCES users(id);       -- NULL read as 'owner'
ALTER TABLE agents ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';    -- private | team
ALTER TABLE agents ADD COLUMN team_id TEXT REFERENCES teams(id);

-- Channels: same, plus explicit members for private channels.
ALTER TABLE channels ADD COLUMN owner_user_id TEXT REFERENCES users(id);
ALTER TABLE channels ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';
ALTER TABLE channels ADD COLUMN team_id TEXT REFERENCES teams(id);
ALTER TABLE channels ADD COLUMN agent_replies TEXT NOT NULL DEFAULT 'auto';  -- auto | mentioned (§4.3)
CREATE TABLE channel_members (    -- humans in a private channel. Team channels need no rows.
  channel_id TEXT NOT NULL REFERENCES channels(id),
  user_id TEXT NOT NULL REFERENCES users(id),
  added_by TEXT NOT NULL REFERENCES users(id),
  added_at INTEGER NOT NULL,
  PRIMARY KEY (channel_id, user_id)
);
-- A user's own DM with an agent: at most one per (agent, user).
CREATE UNIQUE INDEX channels_dm_owner ON channels(agent_id, owner_user_id) WHERE kind = 'main' AND archived_at IS NULL;

-- Credentials and execution surfaces get the same two columns.
ALTER TABLE connections ADD COLUMN owner_user_id TEXT REFERENCES users(id);  -- replaces owner_member's meaning (copied from it)
ALTER TABLE connections ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';   -- private | team (a team-owned connection: owner_user_id = NULL)
ALTER TABLE workspaces  ADD COLUMN owner_user_id TEXT REFERENCES users(id);
ALTER TABLE workspaces  ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';
ALTER TABLE schedules   ADD COLUMN owner_user_id TEXT REFERENCES users(id);   -- a routine's "runs as"
ALTER TABLE memories    ADD COLUMN user_id TEXT REFERENCES users(id);          -- personal memory; NULL keeps agent/shared meaning
ALTER TABLE sessions    ADD COLUMN owner_user_id TEXT REFERENCES users(id);    -- who a private conversation belongs to
CREATE INDEX agents_owner   ON agents(owner_user_id);
CREATE INDEX channels_owner ON channels(owner_user_id, visibility);
```

Secrets: `secrets.toml` and the keychain are file-level, one namespace today (`secrets.rs`). Teams use name prefixes, enforced by the policy and the resolver, not by file layout: `user/<user_id>/<name>` (personal) and `team/<name>` (team-owned, admin-managed). The existing unprefixed names stay valid and mean `team/<name>` in teams mode and "the owner's" in local mode. `check_scoped` in `secrets.rs` already validates scoped names; it learns the two prefixes.

### 1.3 Attribution (migration 0033)

```sql
ALTER TABLE runs  ADD COLUMN requester_user_id TEXT;      -- the human who caused it; NULL = owner (legacy) or system
ALTER TABLE runs  ADD COLUMN usd REAL;                    -- only if not already derivable; see §2.5
ALTER TABLE posts ADD COLUMN author_user_id TEXT;         -- author_id stays the stable string; this is the FK form for humans
ALTER TABLE devices ADD COLUMN user_id TEXT REFERENCES users(id);   -- a device belongs to a person
ALTER TABLE ledger ADD COLUMN actor_user_id TEXT;
ALTER TABLE ledger ADD COLUMN requester_user_id TEXT;
ALTER TABLE telegram_pairings ADD COLUMN user_id TEXT REFERENCES users(id);
CREATE INDEX runs_requester ON runs(requester_user_id, created_at);
CREATE INDEX ledger_actor ON ledger(actor_user_id, seq);
```

**Ledger hash compatibility** (a real risk, flagged for T4a). `entry_hash` in `ledger_repo.rs` hashes a fixed positional JSON array of 12 fields. Adding the two new columns must **not** change the hash of any existing row, or `tern ledger verify` breaks on upgrade. Rule: the two new values are appended to the array **only when non-null**, so old rows hash exactly as before; a migration test verifies a pre-migration fixture chain. Cheaper alternative with the same effect: the existing `actor` string already says `owner`, `agent:<id>`, `tern`; teams add `user:<id>` and put the requester in `detail`, with no schema change. T4a picks one; the test is mandatory either way.

### 1.4 Team settings and spend (migration 0034)

Per-user budgets use a small table, so per-user limits ship without touching `schedules.usd_per_day`:

```sql
CREATE TABLE user_budgets (
  team_id TEXT NOT NULL, user_id TEXT NOT NULL,
  usd_per_day REAL, runs_per_day INTEGER,
  paused_until INTEGER, paused_reason TEXT,
  PRIMARY KEY (team_id, user_id)       -- user_id '*' = the team default
);
```

### 1.5 Mapping from today's single-owner `[account]` and migrating existing data

Today: `[account] issuer, audience, owner = "user_2abc…"`; a valid token for anyone else gets 403; the owner's token is `Auth::Account` with owner scope.

Config: add `[account] teams = false` (default). With `teams = false`, behaviour is **exactly alpha-4**.

On first start after migration 0031-0034 (whether or not teams are on), the store runs one idempotent bootstrap in a transaction:

1. Insert user `owner` (`handle` from the OS user or `owner`, `display_name` "Owner").
2. Insert team `team` named after the owner's display name ("Personal team" if unknown), `created_by = 'owner'`.
3. Insert membership (`team`, `owner`, `admin`).
4. Backfill: every agent, channel, connection, workspace, schedule and session gets `owner_user_id = 'owner'`, `team_id = 'team'`. **Visibility stays `private`.** Nothing becomes visible to anyone who joins later until the owner shares it, so enabling teams cannot leak an existing install's data.
5. If `[account] owner` is set, insert identity (`clerk`, owner sub) for user `owner`. After that the database is the source of truth; `owner = ` is only a bootstrap and a startup warning appears if it disagrees with the identity table.

Two existing rows need care:

- **Home (`channels.id = 'home'`, the default agent's DM) and the default agent `tern`.** Home is the owner's DM and stays private to them. The default agent `tern` is owned by the owner, `visibility = private` after migration; the owner may share it. A new member gets their own DM with each agent they can use, created lazily (`channels.kind = 'main'`, `owner_user_id = member`, slug `dm-<agent>-<handle>`). This is why `channels_dm_owner` is unique on `(agent_id, owner_user_id)`.
- **The current `[account]` gate** (`Auth::Account { user_id }` equals owner or 403) becomes: identity lookup, then membership. With `teams = false` the old code path is kept verbatim.

## 2. Authorization

### 2.1 One policy layer

New module `tern_core::policy` (no I/O, no SQL, no HTTP types):

```rust
pub enum Principal {
    LocalOwner,                        // Unix socket / local token; local mode. Allows everything.
    User { user: UserId, role: Role, team: TeamId, cap: Cap, via: Via },   // via: Api{device}|Telegram|Scheduler{routine}|Agent{run}
    System,                            // internal maintenance only (migrations, retention); explicit, never default
}
pub enum Cap { Read, Chat, Full }      // today's device scopes, now a CAP on what the user may do (never a grant)
pub enum Resource<'a> { Agent(&'a AgentFacts), Channel(&'a ChannelFacts), Routine(..), Secret(..), Connection(..),
    Memory(..), Workspace(..), LedgerEntry(..), Run(..), Approval(..), Team(..), Member(..), Invite(..), TeamSettings, Provider, Config, Bundle }
pub enum Action { View, Use /*run, post*/, Edit, Delete, Share, Transfer, Approve, Manage }
pub fn decide(p: &Principal, a: Action, r: &Resource) -> Decision;   // Allow | Deny(Reason)
```

`*Facts` structs carry only what the decision needs (owner, visibility, membership flags), loaded by the repo. `decide` is table-driven; the §2.2 matrix **is** its test.

How it is enforced, so no UI is ever the gate:

- **Repos take a `Viewer`.** Every list/get on agents, channels, sessions, posts, memories, connections, schedules, outbox, ledger, waits and workspaces takes `&Viewer` (`Viewer::of(principal)` or `Viewer::system()`), and the SQL filters in the query: `owner_user_id = ? OR visibility = 'team' OR EXISTS(channel_members …)`. An unscoped call does not compile. Listing never leaks by forgetting a check on the handler.
- **Handlers resolve a `Principal` once.** One middleware turns the request's `Auth` into a `Principal` (identity, membership, role, cap, from the DB per request; no role in the token) and puts it in the request extensions. Handlers call `policy::decide` with loaded facts for single-object actions (the IDOR guard).
- **Every route is classified.** A test enumerates the router (the pattern the older designs already propose for `PREAUTH`): each route carries a `ResourceKind` annotation or sits in an explicit `UNSCOPED` list. A new route fails CI until classified. This replaces today's path-prefix lists `OWNER_ONLY` and `CHAT_WRITES` in `daemon/auth.rs`, which stay only as the `Cap` mapping (a `Read` device can never write; `Chat` cannot touch config).
- **Non-HTTP callers get principals too.** The scheduler runs a routine as `Via::Scheduler` with the routine's `owner_user_id`; wakes inherit the original human requester (§2.3); the Telegram gateway maps a Telegram id to a user (§5.4); delegations carry the requester. Nothing internal calls a repo with `Viewer::system()` except migrations and retention.
- **SSE events are filtered per principal.** The event bus carries `channel_id`/`agent_id`; each stream subscription applies the viewer's visibility, and a stream ends on removal or demotion (the existing "stream ends when the token expires" hook).

### 2.2 Role matrix (v1)

A = allowed. "own" = rows whose `owner_user_id` is the user. "vis" = visible to the user (own, team-shared, or named in `channel_members`). Admin sees and controls all team-shared things and **manages** (not reads) other people's private ones (open question 1).

| Resource | Action | Member | Admin |
|---|---|---|---|
| **Team**, members, invites, roles | view member list | A (names, handles, roles) | A |
| | add, remove, change role, invite, revoke invite | no | A (last-admin rule, §7.6) |
| | team settings (ceiling, member tools, retention) | no | A |
| **Agent** | create | A (own, private by default) | A |
| | view / use (run, DM, add to a channel they can post in) | vis | all team-shared; others' private: no |
| | edit (persona, model, tools, delegates, autonomy) | own | own and any team-shared |
| | share / unshare | own | own and any team-shared |
| | delete / archive | own | own and any team-shared; others' private: archive or transfer only |
| **Channel** | create | A (own) | A |
| | view and post | team channels, and private ones they own or were added to | team channels, own, added to; others' private: **no content** |
| | edit title, instructions, members, agents, share | own | own and any team channel; others' private: members/ownership only |
| | delete / archive / transfer | own | any team channel; others' private: archive, transfer |
| **Routine** (schedules) | create, edit, pause, delete | own; routines run **as** their owner | own and any team-shared routine |
| **Connection / secret** | add, edit, delete a personal one | own | own |
| | team-owned (`visibility = team`, no owner) | use (if the agent's tool scope allows) | add, edit, delete, use |
| | read a secret value | **never through the API for anyone**; only the resolver, for the principal it belongs to | same |
| **Provider keys, `config`, webhooks, bundles, MCP stdio servers, shutdown** | any | no | A (code and host-level surfaces stay admin-only; §2.6) |
| **Memory** | view / edit personal (`user_id = me`) | own | own |
| | view / edit agent-team memory | agents they can use | agents they can use, plus edit/delete for team-shared agents |
| **Workspace** | use | the workspace of a team-shared agent they can use, within the tool ceiling | same; manage any team-shared |
| **Run** | start | in channels/agents they can use | same |
| | cancel | their own run | any run in the team |
| **Approval** | decide | those addressed to them (§2.3) | those addressed to them, plus **override** any team-shared run's (audited) |
| **Ledger** | view | entries where they are actor or requester, or in channels they can view | all team entries |
| | verify chain | A | A |
| **Audit of admin actions** | view | no | A (`team.*` ledger kinds, §2.5) |

The same table in code: `policy/matrix_tests.rs`, one row per cell, named so a failing test reads like the table above.

### 2.3 A shared agent used by several members

The rule is **per-person authority in a shared room** (the model Grok Bot ships and the tooling spec §15.1 recommends). Every run has exactly one requester, frozen at start in `runs.requester_user_id`.

| Concern | Rule |
|---|---|
| **Secrets and connections** | A run resolves a connection for its target in this order: the requester's own connection, then a team-owned one if the agent's tool scope allows it. **Never another member's.** If the requester has none, the run asks them (the existing secret/auth card flow, addressed to the requester) or the tool fails with "connect your account". The agent owner's credentials are not the agent's credentials: sharing an agent shares its persona, tools, memory and workspace, not its owner's accounts. |
| **Model provider keys** | Team-level, admin-managed (`team/…`). Spend is attributed to the requester (§2.5). A member bringing their own provider key is a later option. |
| **Personal credentials in a team channel** | Reads **and** writes through a personal connection in a team-visible channel always ask the connection's owner (setting `personal_in_team`, default `ask`; an admin may set `allow` or `deny`). The result of a tool call lands in the shared transcript, so using a personal account in a room is a sharing act. The card says so ("This will be visible in #design"). In the user's own DM nothing changes. |
| **Routines and scheduled runs** | Run as the routine's `owner_user_id`, with that user's connections. They have no live requester, so the approval goes to the owner. If the owner is removed or disabled the routine pauses until an admin reassigns it. |
| **Memory** | Three scopes: **personal** (`memories.user_id = me`), **agent-team** (the agent's memory, shared by everyone who may use that agent), and **channel** (instructions, existing). **Personal memory is injected only when the run's requester is that user and the channel has exactly one human** (their DM or a private channel they alone are in), mirroring ChatGPT's "personal memory is off in groups". A multi-human channel gets agent-team and channel memory only. Anything the agent saves in a team context goes to agent-team memory with `source` (the existing provenance column) and the requester id; a tainted run's notes stay `unverified`. Compaction summaries inherit the scope of their conversation. |
| **Workspace and tools** | The agent's workspace is its own (a shared agent works in the team workspace, not in a member's private folder). The tools a member's run may use are `agent.tools ∩ team member_tools`; `member_tools` defaults to **no shell, no python/code, no new MCP stdio servers** for members (§2.6). Admin may widen it per team. |
| **Approvals** | Routed to the **requester**, in the channel and as a notification to their devices. For team-owned connections the approver is the connection's managers (admins and the agent owner). Another member cannot approve for someone else. The first valid decision wins; the ledger records `actor_user_id` (who decided) and `requester_user_id`. Admins may override on team-shared runs; the override is its own ledger kind (`approval.override`). An approval with no answer stays pending; it never falls through to someone else. |
| **Taint** | The existing taint flag stays. One new reason, `peer`: a run whose digest included posts by a human other than its requester. A `peer` run may not use the requester's **personal** credentials for writes without an approval, whatever the autonomy level. Posts by authenticated members are *trusted for content, not for authority*: another member's text can ask the agent something but cannot spend my approvals. Non-members (guests, later) stay fully tainted as today. |
| **Autonomy** | `effective = min(agent.autonomy, team.autonomy_ceiling, user ceiling if set, run's configured level)`, frozen when the run starts (the same freezing the code already does for agent vs configured). A member cannot raise it above the team ceiling; the agent owner cannot either. |
| **Delegation and wakes** | A wake or delegation inherits the **original human requester**, not the agent that woke it, so authority cannot grow along a chain (the hop guards already cap depth). A delegate must be visible to the requester at the time. Sharing an agent whose `delegates` list names a private agent is refused until the list is fixed. |
| **Adding agents to channels** | Only a `team` agent can join a `team` channel (adding a private agent would expose its memory and owner's tool reach). Private channels may hold private agents of any of their members. |
| **Context** | One conversation per (channel, agent) stays. In a multi-human channel each user message is prefixed with the author (`[Dana] …`, the shape tooling spec §15.2 proposes), and a message from user B no longer cancels user A's running run: it **queues** (or steers through the inbox the sub-agents use). Only the requester or an admin cancels a run. |

### 2.4 Admin-set ceilings

`teams.autonomy_ceiling` (same values as `agents.autonomy` today) caps everyone. `teams.settings.member_tools` caps what members' runs may call. Both apply to **members**; an admin's own private agents obey the team ceiling too unless the setting `admins_exempt` is true (default false: the ceiling is a team promise, not a role perk). Changing either is a ledger event.

### 2.5 Cost and ledger attribution, and audit

- **Spend**: each run records `requester_user_id`; spend per user per day is a query over runs (no new write path). `user_budgets` pauses a user's runs when they exceed `usd_per_day`/`runs_per_day`, using the same pause machinery as `schedules.usd_per_day`. Routine runs charge their owner. The team sees a per-user spend table (admin) and members see their own.
- **Ledger**: every row carries who acted (`actor_user_id` or `actor = 'user:<id>'`) and for whom (`requester_user_id`). Existing rows keep `actor = 'owner'`, which still resolves because user `owner` exists.
- **Admin audit**: new kinds, all `actor = user:<admin>`: `team.create`, `member.invite`, `invite.revoke`, `member.accept`, `member.role`, `member.remove`, `member.transfer`, `agent.share` / `agent.unshare`, `channel.share` / `channel.unshare`, `channel.owner_change`, `settings.change`, `approval.override`, `breakglass.read` (if adopted). The viewer is the existing ledger screen filtered to `team.*`; it is append-only and hash-chained like everything else.

### 2.6 What members cannot do, on purpose

Some surfaces run code on the host or let the internet start runs. They stay admin-only in a team core: installing bundles and skills with code, MCP stdio servers (arbitrary commands), webhooks (they let outsiders start tainted runs), provider keys, `config`, shutdown, devices of other users. Members get safe variants: personal connections via OAuth, remote MCP servers as connections, their own routines. This is the main way a team stays safe when members can create agents.

## 3. Roles beyond admin and member

| Role | Recommendation | Why |
|---|---|---|
| **admin** | v1 | Required. Several admins allowed. |
| **member** | v1 | Required. |
| **owner** (billing, cannot be removed, can delete the team) | **Later (L1)**, with billing. | There is no billing in tern yet, so owner would equal "first admin". v1 gets 90% of it from the last-admin guard plus `teams.created_by`. Schema allows it (`role` is free text, validated in code; `#[non_exhaustive]`). |
| **guest / viewer** | **Later (L2).** | The useful guest is "reads or posts in named channels only", and it is also how M0 group-room guests from Telegram and Slack should work (tooling spec §15.3). That needs per-channel roles: add a `role` column to `channel_members` when guests land (an additive `ALTER TABLE`). |
| **per-channel roles** (channel admin) | Not planned. | Channel owner already covers "my channel". Admin covers "everything". |

## 4. Channels with several humans

### 4.1 Membership

- `visibility = team`: every active member can see and post. No `channel_members` rows.
- `visibility = private`: the owner and named members (`channel_members`). Others do not see it exist in lists, search or SSE; the 404 is the same as "no such channel".
- A member may move their own channel between `private` and `team` at any time. Unsharing a team channel keeps the posts but removes access for people who are not owner or named; if the history should disappear for them it already does, because the channel is simply no longer visible to them.
- Admins can see the **existence, title, owner, size and activity** of private channels so they can govern them, but not read content (§2.2).

### 4.2 Posting and attribution

`posts.author_kind = 'user'`, `author_id` becomes the user id (`owner` for legacy rows), `author_user_id` is the FK, and `author` is the display name **snapshotted at post time** so renames and removals do not rewrite history. After removal the UI shows "Dana (removed)". Messages sent to the model carry the author name for multi-human channels.

### 4.3 Who answers

Today a post that mentions no one is answered by the channel's lead. With several humans that would make the agent answer every human-to-human message. `channels.agent_replies`:

- `auto` (default for channels with one human; today's behaviour);
- `mentioned` (default when a second human joins): agents answer only an `@agent`, a reply to an agent's post, or a post in a thread an agent is in. "Let agents decide" routing still works and may pick members.

The wake limits (`agent_wakes`, `max_hops`, `wakes_per_hour`) are unchanged and per channel.

### 4.4 Mentions, human-to-human, notifications

- A user's `handle` is a mention slug in the same namespace as agent slugs; creation rejects a handle that equals any agent slug in the team and vice versa. `mention_slugs()` in `members_repo.rs` already parses `@name`; resolution looks at agents then users.
- Mentioning a human **never wakes a run**; it produces a notification and an unread mark for that user, and is delivered to their devices (and Telegram if linked). Human-to-human posts are ordinary `kind = message` posts with no run.
- `@all` and `@channel` are out of v1.

### 4.5 Presence

Ephemeral and in memory, never persisted or ledgered: the SSE stream emits `presence` events (`viewing`, `typing`) per channel for users who may see it; a channel header shows who is here. Typing indicators for agents already exist. Presence is **phase 2** of the slice (§8.2) because nothing depends on it.

### 4.6 Each member's own DM

A member opens a DM with any agent they can use: a private `main` channel owned by them, one per (agent, user). Nobody else, admins included, reads it by default. The shared agent's team memory is available in it; personal memory is available too (one human).

## 5. API and transport

### 5.1 Clerk Organizations or tern-owned tables

| | Clerk Organizations | tern-owned tables (**recommended**) |
|---|---|---|
| Source of truth for who is in a team | Clerk | the core's database |
| Works offline or with Clerk down | no: needs claims or an API call | yes: the core decides alone, like it already does with JWKS only for sign-in |
| Self-hosted or non-Clerk identity later | locked to Clerk | identity stays pluggable (`identities.provider`) |
| Removal latency | until the token expires or an API call | **immediate** (role read from the DB per request; streams closed on removal) |
| Telegram, CLI, devices, routines | not Clerk concepts; need a second mapping anyway | one model for all |
| Roles | admin/member plus custom roles with plan limits; org role comes in the session token, which tern does not use (it sends the **id token**) | exactly the roles tern needs, plus ceilings and per-resource ownership, which Clerk cannot express |
| Invites | Clerk emails and accept flow | tern link or code; email sending is the missing piece (below) |
| Cost and plan dependence | Organizations are a paid, plan-gated Clerk feature (check current terms) | none |
| Data model fit | still need tern tables for ownership, visibility, memory scope | one place |

**Decision**: tern tables are the source of truth. Clerk gives `sub` and a verified `email`. If a customer later wants Clerk Organizations as the invite UI or for SSO/SCIM, an optional adapter can create tern invites from Clerk org membership; that is L5 and must not change the model.

### 5.2 Token claims and request flow

No new claims. The core reads `sub`, `email`, `email_verified` from the id token it already verifies (today only `sub` and `email` are read; **`email_verified` must be added and required for invite acceptance**).

```
JWT valid ─► identities(clerk, sub) ─► user ─► memberships(team) ─► Principal{role, cap}
   │ no identity                │ identity, no active membership
   ▼                            ▼
 pre-member allowlist       403 {"error":"not_a_member"}   (and /v1/invites/accept, /v1/me still reachable)
```

Pre-member allowlist: `GET /v1/me`, `POST /v1/invites/accept`. Everything else is classified by the route test. Device tokens (`tern_dev_`) gain `user_id`; a device's `scope` is now a cap on the user it belongs to. **A member can pair devices only up to their own cap and only for themselves**; owner-scope device tokens can be minted only by admins. The wire values `read | chat | owner` stay (`owner` is documented as "full"); old devices backfill to `user_id = 'owner'`.

Cores keep refusing every account in `teams = false` mode other than `[account] owner`, as in alpha-4.

### 5.3 Endpoints (new or changed)

| Method, path | Who | Notes |
|---|---|---|
| `GET /v1/me` | any signed-in | `{user, team, role, cap, capabilities[]}`; UIs gate on `capabilities` (e.g. `team.manage`, `agent.share`), the core still enforces |
| `GET /v1/team` | member | name, settings visible to members, member count |
| `PATCH /v1/team` | admin | name, `autonomy_ceiling`, `member_tools`, `personal_in_team`, retention |
| `GET /v1/team/members` | member | handle, display name, role, status (no email for members; admins see email) |
| `PATCH /v1/team/members/{id}` | admin | `role`; last-admin guarded |
| `DELETE /v1/team/members/{id}?then=transfer:{user}\|archive\|delete` | admin | the removal flow (§7.5) |
| `POST /v1/team/invites`, `GET`, `DELETE /{id}` | admin | `{email, role}` returns `{link}` once |
| `POST /v1/invites/accept` | any valid token | `{token}`; email match plus `email_verified` (§5.5) |
| `POST /v1/agents/{id}/visibility`, `POST /v1/channels/{id}/visibility` | owner or admin | `{visibility: private\|team}`; validation rules in §2.3 |
| `GET/POST/DELETE /v1/channels/{id}/members` | channel owner or admin | private channel humans |
| `POST /v1/agents/{id}/transfer`, `POST /v1/channels/{id}/transfer` | owner or admin | `{to}`; the new owner must be an active member |
| `GET /v1/team/audit` | admin | `team.*` ledger entries |
| `GET /v1/team/spend` | admin; members get own | by user and day |
| existing `agents`, `channels`, `sessions`, `connections`, `secrets`, `ledger`, `outbox`, `schedules`, `memories`, `workspaces` | per §2.2 | responses gain `owner`, `visibility`, `can: {edit, share, delete}` |

### 5.4 Clients

- **Desktop** (`apps/desktop`, `packages/tern_client`): sign-in exists (`state/account.dart`). New: `/v1/me` after connect, `NotAMember` state with "Enter invite code", team name in the server switcher, the UI work in §6. A server with `teams = false` shows none of it.
- **TUI** (`crates/tern-tui`): connects as a device. v1: show the user and team in the status line, `@handle` completion, no admin screens (use `tern team ...` on the CLI). Local mode unchanged.
- **CLI**: `tern team status | members | invite | remove | role`, working over the API with an owner-capable device or the local socket.
- **Telegram gateway**: one bot token = one agent (`agents.telegram`). Today `telegram_pairings(user_id INTEGER)` pairs "the owner". With teams: a pairing code is minted by a signed-in user and binds that Telegram id to **that user**; inbound messages carry the principal `User{via: Telegram}`; outbound approvals and notifications go to the requester's linked chat. An unpaired Telegram sender is refused in v1 (guest rooms are L3). A bot whose agent is private to its owner answers only its owner; a team agent's bot answers any paired member. Telegram group chats stay out of scope.
- **MCP serve** (`mcp_serve.rs`): acts as the device's user; exposes only what the user can see.

### 5.5 Invite flow (email invite, accept through Clerk identity)

The core has no mail sender. v1: the admin enters an email; the core returns a **link** (`tern://invite?server=…&token=…` plus a code form) that the admin copies and sends, and the UI also offers "Email it" through the user's mail client (`mailto:`). Automatic email sending is L5 (control plane or Clerk invitation email). Accept:

1. Invitee opens the link in the desktop app, signs in with Clerk (system browser, as today).
2. App calls `POST /v1/invites/accept {token}` with the id token.
3. Core checks: token hash matches an unexpired, unrevoked, unaccepted invite; token's `email_verified` is true **and** the id token's `email` equals `invites.email` (case-insensitive). On success it creates or links the user (identity), creates the membership with the invited role, marks the invite accepted, ledgers `member.accept`.
4. Mismatched verified email: refused with a message naming the invited address (never revealing other invites). Open question 3 asks whether to also allow code-only accept.

Rate limit 5 per minute per source (the pairing limiter); tokens are 160 bits, hashed at rest; one use; 7-day expiry; revoking an invite is immediate.

## 6. UI (desktop first; TUI and CLI follow)

Follows [brand-guide.md](brand-guide.md) and [design-system.md](design-system.md): cream/paper surfaces, hairline dividers, **underline tabs**, **monotone line icons**, no emoji, one short line per option with detail in tooltips, orange only for the primary action. A static mock is in [mocks/teams.html](mocks/teams.html).

- **Team switcher**: the existing server picker, with the team name as the title ("Acme · 4 people"). One team per core in v1, so no separate switcher widget; the row appears only when `teams = true`.
- **Team screen** (new sidebar item, "Team"): underline tabs **Members · Invites · Settings · Audit** (admin sees all four; members see **Members** read-only).
  - Members: avatar, name, handle, role pill (`Admin`/`Member`), last active; row menu: Change role, Remove. The last admin's menu has those items disabled with a tooltip.
  - Invites: email + role, "Create invite" gives the link with a Copy button, pending list with Revoke.
  - Settings: autonomy ceiling (select), "Members can run shell and code" (toggle, off), "Personal accounts in team channels" (Ask / Allow / Deny), retention days.
  - Audit: the ledger list filtered to `team.*`.
- **Share toggle** on the agent sheet and the channel header: a two-state control **Private · Team** (segmented, not a switch), with one line of help ("Everyone on the team can use it"). Changing to Team shows a one-step confirmation that lists what becomes visible (persona, tools, memory for agents; history for channels) and any blocker (a delegate that is private).
- **Ownership badges**: a small monotone icon and label on agent and channel rows: lock + "Private", people + "Team", plus "by Dana" when not yours. In a channel header: the owner's name and the human member avatars.
- **Empty and denied states**: one line and one action. "You don't have access to this. Ask Dana to share it." (no existence leak for private items: it is the same as not found). Members opening the Team screen's admin tabs see "Only admins can change this" with the read-only list. Signed in but not a member: "You're not on this team yet" with an invite-code field and a Sign out link.
- **Approvals**: cards say who asked ("Dana asked · uses your Gmail") and, when a team room will see the result, "Visible in #design".
- **Removal dialog**: shows what Dana owns and offers **Transfer to me** (default) / **Transfer to…** / **Archive** / **Delete after 30 days**.

## 7. Security review

| # | Threat | Mitigation |
|---|---|---|
| 1 | **Privilege escalation by API**: member calls an admin route, edits `role`, or passes `role`/`owner_user_id` in a body (mass assignment) | Admin routes are `Manage` actions in `decide`; request structs for member-editable objects have no `owner`/`role` fields; ownership changes only via `transfer`; the route-classification test; `decide` matrix tests |
| 2 | **IDOR**: guessing agent/channel/session ids | Single-object handlers load `*Facts` and call `decide`; repos filter by `Viewer`; "not visible" returns the same 404 as "does not exist" |
| 3 | **Device token escalation** | A device cap never exceeds the user's role; members cannot mint owner-scope devices; device rows are bound to `user_id`; removing a user revokes their devices in the same transaction |
| 4 | **Shared-agent prompt injection across members**: B posts text that makes A's run misuse A's accounts | `peer` taint (§2.3): a run that read another human's posts cannot use the requester's personal credentials for writes without approval; approvals go only to the requester and show who asked and what text triggered; wakes inherit the human requester; delegates must be visible to the requester |
| 5 | **Secret leakage between members** | Resolver is by principal, never by agent; `/v1/secrets` and `/v1/connections` list only the caller's and team-owned names; values are never returned; personal tool results in team rooms ask first (`personal_in_team`); secrets scrubbed from transcripts by the existing `redact_with`, which is given the requester's secret values; a shared agent's persona or memory is **not** a place for credentials, and the agent sheet warns |
| 6 | **Memory leakage**: personal memory in a group, or a shared agent's memory learning one member's private details and repeating them | Personal memory only in single-human contexts (§2.3); agent-team memory writes carry provenance and the requester; members can view and delete agent-team memory of agents they use; tainted notes stay `unverified`; compaction summaries inherit the narrowest scope of what they summarize |
| 7 | **Sharing exposes more than intended** (owner shares an agent and its workspace files, delegates, or a channel and its history) | Share confirmation lists exactly what becomes visible; private delegates block sharing; unshare is immediate and revokes future access; past posts a member already saw are not recallable (stated in the dialog) |
| 8 | **Host-level reach by members** (shell, code, MCP stdio, bundles, webhooks) | Admin-only (§2.6); `member_tools` default off; sandbox rules unchanged; per-user workspace roots, so a member's file tool cannot reach another's |
| 9 | **Removal of a member** | One transaction: membership `removed`, `users.disabled_at`, devices revoked, SSE streams closed, Telegram links deleted, personal connections' secrets deleted, personal-scoped runs cancelled, their routines paused. Then per the admin's choice: their **shared** agents and channels transfer (default: to the removing admin) or archive; **private** agents/channels/DMs are archived and unreadable, purged after `retention_days` (default 30) unless transferred or exported; personal memory is purged at the same time. Posts keep a name snapshot; ledger rows stay (append-only) and keep the user id; the tombstoned user row holds no email |
| 10 | **Admin demotion and removal races** | Membership changes use `BEGIN IMMEDIATE` plus the §1.1 trigger, so two admins demoting each other cannot both succeed: the second sees zero other admins and fails. Role is read from the DB per request, so a demotion takes effect on the next call; streams close at once |
| 11 | **Last-admin protection** | Trigger and handler check; also a user cannot remove themselves if last admin ("transfer admin first"); `tern team recover` on the local socket (owner of the machine) can re-add an admin, which the ledger records, so a locked-out team is never bricked |
| 12 | **Invite abuse**: forwarded link, enumeration, replay | Email-bound, `email_verified` required, hashed token, single use, 7 days, rate limit, uniform error text, admin can revoke; invites to an address with an existing active membership are refused |
| 13 | **Identity confusion**: same email, different Clerk user, or changed email | Lookup is by `(clerk, sub)`; email is display and invite binding only; no auto-link by email |
| 14 | **Admin snooping** | Admins cannot read others' private content by default; the break-glass option (open question 1) is a ledgered, owner-notified action |
| 15 | **Audit tampering** | Ledger remains append-only with triggers and hash chain; admin actions are ledgered in the same transaction as the change |
| 16 | **Cost abuse** | Per-user budgets, team provider keys behind them, wake limits unchanged |
| 17 | **Stale clients showing too much** | UIs gate on `capabilities` but receive only visible data; cached data is dropped on sign-out and on a `not_a_member` |

Security acceptance for the slice: a **two-user leak suite** (T12) that runs the same scripted scenarios as admin, member A, member B and removed member C against the API and asserts what each can list, read, run and approve. It is the release gate.

## 8. Work breakdown

Estimates are for one focused implementation session: **S** about half a day, **M** one to two days, **L** three to five days. File ownership is exclusive per package so parallel sessions do not collide.

### 8.1 Shared-file protocol (do this once, in T0)

These files are touched by many packages. T0 pre-creates the hooks so later sessions edit only their own files:

| Shared file | Hook added in T0 |
|---|---|
| `crates/tern/src/daemon/api.rs` (router) | `mod` and `.merge(...)` lines for `api_team`, and a `routes!` classification table in `daemon/route_policy.rs` (each package adds rows in its own section) |
| `crates/tern/src/cli.rs`, `main.rs` | `TeamCommand` subcommand with a stub `team_cmd.rs` |
| `crates/tern-store/src/lib.rs`, `migrations.rs` | module stubs and the four migration slots **0031 to 0034** (empty files); each package fills only its own file; renumber at merge if another session lands a migration first |
| `crates/tern-core/src/lib.rs` | `pub mod policy;` |
| `apps/desktop/lib/main.dart`, sidebar | a `team` route stub |
| `changelog.d/` | one fragment per package (never `CHANGELOG.md`) |

### 8.2 Packages

Phase labels: **A5** is the smallest shippable alpha slice ("Teams A"); **B** and **C** are later.

| # | Package | Phase | Scope | Owns | Depends on | Parallel with | Tests | Est. |
|---|---|---|---|---|---|---|---|---|
| **T0** | Foundations | A5 | Freeze this design (owner answers to §9); `tern_core::policy` types (`Principal`, `Cap`, `Resource`, `Action`, `Decision`), `LocalOwner` allow-all and `decide` with the §2.2 matrix as table-driven tests; shared-file hooks (§8.1); route-classification test scaffold (fails open for now with a list of unclassified routes); two-user test fixture crate in `crates/tern/tests/it/` (`teams_fixture.rs`) | `crates/tern-core/src/policy/**`, `daemon/route_policy.rs`, `tests/it/teams_fixture.rs`, hook edits | none | nothing (everything waits) | matrix tests (every cell), local-owner allow-all, fixture boots a 3-user core | M |
| **T1a** | Schema and identity repos | A5 | Migrations 0031 (users, identities, teams, memberships, invites, trigger), the bootstrap (§1.5), `users_repo`, `teams_repo`, `invites_repo`; `[account] teams` config key; last-admin trigger; migration tests on a populated alpha-4 DB | `crates/tern-store/migrations/0031*`, `src/{users,teams,invites}_repo.rs`, `crates/tern-core/src/config/` (teams key only) | T0 | T2 (stubbed), T5 (stubbed), T7 (against contract), T8-design | migration on a fixture DB keeps every row; bootstrap idempotent; last-admin trigger; invite token hashing | M |
| **T1b** | Ownership, visibility and scoped repos | A5 | Migrations 0032, 0033, 0034; `Viewer`; every list/get in the repos filters by it; `channel_members`; per-user DM uniqueness; attribution columns; **ledger hash-compat** | `migrations/0032-0034*`, `agents_repo`, `channels_repo`, `connections_repo`, `ledger_repo`, `repo.rs`, `memory_repo`, `outbox_repo`, `waits_repo`, `daemon_repo` (callers pass `Viewer`; call-site fixes in other crates are mechanical, listed in the PR) | T0, T1a | T2, T5, T7 | per-repo visibility tests (own, team, named, other); a pre-migration ledger chain still verifies; unscoped call does not compile | L |
| **T2** | Principal resolution | A5 | Replace the single-owner gate: id token → identity → membership → `Principal`; `email_verified`; `/v1/me`; `not_a_member` and the pre-member allowlist; device `user_id`, `Cap`; SSE stream close on removal/demotion; `teams = false` keeps the old path verbatim | `daemon/auth.rs` (incl. device routes), `daemon/account.rs`, `account_cmd.rs` | T0, T1a | T1b, T5 | JWT matrix (unknown sub, removed, demoted mid-session, wrong audience); `teams = false` golden test equals alpha-4 behaviour; device cap never exceeds role | M |
| **T3a** | API enforcement: agents, sessions, runs | A5 | `Principal` into handlers; `decide` calls; visibility/transfer/share endpoints; sharing validation (private delegates, workspace); response fields `owner`, `visibility`, `can` | `daemon/api_agents.rs`, `api_runs.rs`, `api.rs` session routes | T1b, T2 | T3b, T3c, T5, T6 | per-route two-user tests; IDOR 404s; share-blocker rules | L |
| **T3b** | API enforcement: credentials and admin surfaces | A5 | `secrets` and `connections` become personal plus team-owned; `providers`, `config`, `webhooks`, `bundles`, `mcp` become admin-only; `member_tools` check; member-safe variants | `api_secrets.rs`, `api_connections.rs`, `api_providers.rs`, `api_config.rs`, `api_webhooks.rs`, `api_bundles.rs`, `api_mcp.rs`, `api_skills.rs`, `crates/tern-core/src/secrets*` | T1b, T2 | T3a, T3c, T5, T6 | secret never in any response; B cannot list A's; admin-only routes 403 for members | L |
| **T3c** | API enforcement: ledger, outbox, schedules, waits, workspaces, tasks, subagents, events | A5 | Viewer-scoped listings; routine `runs as` owner; event-stream visibility filter; workspace sharing | `api_ledger.rs`, `api_outbox.rs`, `api_schedules.rs`, `api_waits.rs`, `api_workspaces.rs`, `api_tasks.rs`, `api_subagents.rs`, `crates/tern-core/src/events.rs` and the daemon SSE handler (bus filter) | T1b, T2 | T3a, T3b, T5, T6 | SSE leak test (B never receives A's private channel events); ledger scoped views | L |
| **T4a** | Run attribution and spend | A5 | `runs.requester_user_id` set at start; wakes, delegation, routines inherit; ledger `actor_user_id`/`requester_user_id` on every writer; spend per user; `user_budgets` pause (the table ships in A5, **enforcement in B**) | `crates/tern-core/src/agent/**` (run start), `daemon/manager/**` (run creation), `delegations.rs`, `wakes.rs`, `scheduler.rs`, ledger writers' call sites | T1b | T4b, T4c, T5, T6 | requester frozen and inherited through a wake chain; routine charges owner; ledger rows carry both ids | M |
| **T4b** | Credentials and memory scoping | A5 | Resolver by principal (personal, then team-owned, never another's); `personal_in_team` ask rule; memory scopes personal / agent-team / channel and the prompt builder; compaction scope | `tern-core/src/{secrets*.rs,memory.rs,prompt.rs,compaction.rs}`, `memory_repo` read-path call sites, `tern-mcp` credential lookup | T1b, T4a (the run's requester) | T4c, T5, T6 | B's run never sees A's token or personal memory; multi-human channel injects team memory only; redaction covers the requester's secrets | L |
| **T4c** | Approvals, taint, autonomy ceiling, tool ceiling | A5 | Approval routing to the requester; team-owned approvers; admin override; `peer` taint; effective autonomy `min(...)`; `member_tools` applied to the run's tool set | `tern-core/src/approval*`, `agent/` approval and tool-scope code, `daemon/waiting.rs`, `outbox.rs` (daemon) | T1b, T4a | T4b, T5, T6 | approval for A's run cannot be decided by B; `peer` run's personal write asks; ceiling cannot be exceeded by agent or member | L |
| **T5** | Team admin API and invites | A5 | `api_team.rs`: team, members, roles, invites, accept, removal flow with transfer/archive/delete, `recover`, settings, audit, spend; ledger `team.*`; retention purge job | `daemon/api_team.rs`, `daemon/team_retention.rs`, `tern/src/team_cmd.rs` | T0, T1a, T2 | T3x, T4x, T6, T7 | invite lifecycle (expiry, replay, wrong email, unverified email, rate limit); last-admin races with concurrent calls; removal transaction effects | L |
| **T6** | Channels with several humans | A5 minimal / B rest | A5: `visibility`, `channel_members`, per-user DMs, author attribution, `agent_replies`, queue-not-cancel, adding agents rule. B: presence events, `@handle` mentions with notifications, unread | `daemon/api_channels.rs`, `daemon/channels.rs`, `suggestions.rs`, `tern-core/src/channels.rs`, `members_repo.rs` (mention resolution) | T1b, T2 | T3x, T4x, T5, T8b | two humans in one channel: both post, agent answers by mention, neither cancels the other's run; private channel invisible to non-members | L |
| **T7** | Dart client | A5 | Models and endpoints in `tern_client`: `Me`, `Team`, `Member`, `Invite`, visibility/transfer, 403 `not_a_member`, `can` flags; mock server fixtures | `apps/desktop/packages/tern_client/**` | the §5.3 contract (T0); real server not required | all backend work | Dart unit tests against fixtures | M |
| **T8a** | Desktop: Team screen | A5 | Team screen (Members, Invites, Settings, Audit tabs), removal dialog, role menu, last-admin states, invite link copy | `apps/desktop/lib/ui/team/**` (new), `lib/state/team.dart` (new) | T7 | T8b, T8c | widget tests per state (admin, member, last admin) | M |
| **T8b** | Desktop: sharing and ownership | A5 | Private/Team control on agent sheet and channel header; badges; share confirmation with blockers; own/other filters; permission-denied and empty states; "who asked" on approval cards | `lib/ui/agent_sheet.dart`, `agents.dart`, `channel_view.dart`, `channel_items.dart`, `auth_card.dart`, `approvals` card files | T7 | T8a, T8c | widget tests: badges, blocker text, hidden controls when `can.share` is false | M |
| **T8c** | Desktop: invite accept and signed-in states | A5 | `NotAMember` screen with invite code, deep link `tern://invite`, accept call, team name in the server switcher, drop cached data on sign-out | `lib/state/account.dart`, `lib/state/connection.dart`, `lib/ui/offline.dart`, new `lib/ui/join.dart` | T7 | T8a, T8b | widget tests for each state; sign-out clears caches | M |
| **T9** | TUI | B | Status line user and team, `@handle` completion, denied states | `crates/tern-tui/**` | T2, T6 | T10, T11 | TUI snapshot tests | S |
| **T10** | Telegram identity | B (A5 keeps owner-only, bound to the owner user) | Pairing code minted per user, principal on inbound, approvals and notifications to the requester's chat, refuse unpaired senders, `tern telegram pair` per user | `crates/tern-gateway/src/telegram/**`, `gateway_cmd.rs`, `telegram_pairings` repo section | T1b, T2, T4c | T9, T11 | paired member runs as themselves; unpaired refused; private-agent bot answers only its owner | M |
| **T11** | CLI | A5 minimal / B rest | `tern team status\|members\|invite\|remove\|role\|recover` (A5: status, invite, members, recover) | `crates/tern/src/team_cmd.rs` (shared with T5: T5 owns the API, T11 owns the commands; coordinate by the T0 stub) | T5 | T9, T10 | CLI integration tests | S |
| **T12** | Security suite and gate | A5 | The two-user leak suite (§7); route-classification test turned strict; `decide` fuzz; removal and race tests; local-mode golden tests; final review | `crates/tern/tests/it/teams_*.rs` | T3a, T3b, T3c, T4b, T4c, T5, T6 (writes cases early against T0's fixture) | T8x | the suite itself; all 17 rows of §7 each map to at least one test | M |
| **T13** | Docs and release | A5 | `docs/recipes/teams-setup.md`, deployment-spec §10.4b, reference updates, changelog fragments checked, release notes | `docs/**` (not this file), `changelog.d/` | the packages | everything | docs build/lint | S |

### 8.3 Order and parallelism

```
Wave 0:  T0 ─────────────────────────────────────────────────────────────┐
Wave 1:  T1a ║ T7 (Dart client vs contract) ║ T12 (write cases vs fixture)
Wave 2:  T1b ║ T2 ║ T5 (invites/admin API, stubs for repos)
Wave 3:  T3a ║ T3b ║ T3c ║ T4a ║ T6 ║ T8a ║ T8c          (T4b, T4c start when T4a lands)
Wave 4:  T4b ║ T4c ║ T8b ║ T11 ║ T12 (leak suite goes green)
Wave 5:  T13, bug bash, alpha-5 release
Later:   T9, T10, T6 (B), per-user budget enforcement, L1..L6
```

Hot spots to protect: `api.rs` and `cli.rs` (hooks in T0), migration numbers (slots in T0, renumber at merge), and `daemon/manager/**` (T4a lands first; T4b and T4c rebase on it). Each package is one branch and one PR; the owner merges.

### 8.4 The smallest shippable alpha slice (alpha-5, "Teams A")

Included: T0, T1a, T1b, T2, T3a, T3b, T3c, T4a, T4b, T4c, T5, T6 (visibility, DMs, attribution, queue-not-cancel only), T7, T8a, T8b, T8c, T11 (minimal), T12, T13.

What a user can do: the owner turns on `teams = true`, invites teammates by link, they sign in with Clerk and join as members; everyone creates agents and channels that are private by default and flips them to Team; shared agents run with each requester's own accounts, approvals reach the requester, memory stays separate, an admin sets an autonomy ceiling and removes people with a transfer.

Deliberately **not** in alpha-5: presence and typing, human `@mentions` and notifications (the unread mark is basic), TUI changes (it works as a device for any user), Telegram for non-owners (stays owner-only, bound to the owner user), per-user budget enforcement (spend is recorded and shown), email-sent invites, owner and guest roles, multiple teams per core, break-glass reads.

Why not cut further: the slice cannot ship without T4b and T4c because sharing an agent before credentials, memory and approvals are per requester would leak.

### 8.5 Later phases

| Phase | Items |
|---|---|
| **B** (alpha-6) | Presence, human mentions and notifications, T9 TUI, T10 Telegram identity, per-user budget enforcement and UI, spend table for admins, member-owned provider keys |
| **C** | **L1** owner role and billing seam; **L2** guest/viewer and per-channel roles (also M0 group-room guests); **L3** Telegram, Slack and Discord group rooms; **L4** several teams per core and a real team switcher; **L5** emailed invites, Clerk Organizations adapter, SSO and SCIM; **L6** break-glass and audit export |

## 9. Open questions for the owner

Each has a default; the design assumes the default unless you say otherwise.

1. **Can an admin read a member's private channels and DMs?** You said admins "control channels and everything in the team". Default: admins manage private items (see existence, owner, activity; archive, transfer, delete) but **cannot read content**; reading is a break-glass action that is ledgered and notifies the owner (L6, not in alpha-5). Alternative: admins read everything, which is simpler and weaker for trust.
2. **One team per core in v1, or several?** Default: one team per core, schema ready for more. Several teams per core need a real team switcher and tenant scoping on every query (L4).
3. **How is an invite accepted?** Default: link or code bound to an email, and the signed-in Clerk identity must have that **verified** email. Alternative: whoever holds the code may join (easier for people with a different login address, weaker).
4. **Who may create agents and channels?** Default: every member may, private by default, and an admin can disable member agent creation per team later. Alternative: admins only create team agents.
5. **When a member leaves, what happens to their shared agents and channels?** Default: transfer to the removing admin (changeable in the dialog); their private items archive for 30 days then purge; personal memory purges with them. Is 30 days right, and should purge be immediate for personal credentials (default: yes, immediate)?
6. **Personal accounts used in a team room** (a member's Gmail by an agent in #design): default `ask` every time, with a warning that the result is visible to the channel. Alternatives: `deny` (personal accounts only in DMs) or `allow` after one-time consent.
7. **Members and shell/code**: default off for members, admin can enable per team. Is that the right default for your first teams (technical founders may want it on)?
8. **Email sending for invites**: alpha-5 copies a link. Do you want automatic email (needs the control plane or Clerk invitation emails) before alpha-5, or is copy-link fine?
9. **Billing owner role**: ship admin and member only and add owner with billing later (default), or add the non-removable owner now?
10. **Telegram**: in alpha-5 only the owner uses Telegram. Do other members need Telegram before B?
11. **Migration safety**: existing agents and channels become **private to the owner** when teams are enabled (default, safest). Alternative: make the default agent `tern` and existing topic channels team-visible on first enable.
12. **Team name and handle policy**: handles are chosen by the user at join and unique per team, and a handle equal to an agent slug is refused (default). OK?

## 10. Appendix: how the existing pieces map

| Existing | In teams mode |
|---|---|
| `[account] owner` | bootstrap of user `owner`'s identity; database is truth afterwards |
| `Auth::Account { user_id }` | resolved to `Principal::User` (identity, membership) |
| `Auth::Device { scope }` | cap on the device's user |
| `OWNER_ONLY`, `CHAT_WRITES` prefix lists | `Cap` mapping plus the policy; route table classifies everything |
| `channels.id = 'home'` | the owner's DM; new users get their own DMs |
| `channel_agents` | unchanged; adds the "team agents only in team channels" rule |
| `connections.owner_member` | `owner_user_id`; resolver by principal |
| `memories.agent_id` NULL = shared | adds `user_id` for personal; scopes per §2.3 |
| `ledger.actor = 'owner'` | still valid (user `owner` exists); new rows add the user |
| taint | adds reason `peer` |
| autonomy per agent | `min` with the team ceiling |
| `telegram_pairings` | gains `user_id` |
| `author_id = 'owner'` on posts and messages | user id |
