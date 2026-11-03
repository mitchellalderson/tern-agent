# tern deployment topologies: install tiers, storage backends, remote cores, hosted cloud

Status: draft, 2026-11-03. Post-MVP. D1 (remote core) is built except the GUI's remote profiles; see DECISIONS.md "D1". The rest isn't built yet. This is a design to pull work from as it comes up,
not a milestone plan. Each section names the engine change it needs and the phase it belongs to (§12).

Related: [gui-spec.md](gui-spec.md) (the Flutter client, §6 and §7.4 there are superseded by §6–§7 here),
[research/inngest-tie-in.md](research/inngest-tie-in.md) (V2 event plumbing; §10.6 here is where it fits).

## TL;DR

- **Four ways to run tern, one engine.** (1) `tern` alone: CLI, TUI and daemon, SQLite, nothing else. (2) The same
  plus the desktop app, which attaches to the local daemon over the Unix socket. (3) A self-hosted *server core*:
  the same binary in a container, reachable over TLS, optionally on Postgres. (4) **tern Cloud**: we run the core;
  desktop and mobile apps sign in. Every client talks the same `/v1` API, so the difference between the tiers is
  transport, auth and storage, not features.
- **Storage becomes a trait with two backends.** The `Store` is a synchronous `Mutex<rusqlite::Connection>` today and
  is called directly from async code. It becomes an async `StoreBackend` trait with a SQLite implementation (rusqlite on
  a dedicated thread, unchanged schema) and a Postgres implementation (`tokio-postgres` + `deadpool-postgres` +
  rustls) behind a `postgres` cargo feature. Not sqlx: see §4.2.
- **Postgres is for self-hosters, not a prerequisite for the cloud.** The recommended first cloud design is one core
  per tenant (§10.2). That works on SQLite on a volume with streaming backup. Postgres becomes required only if we
  later move to shared multi-tenant cores (§10.7). Building it is still worth it: people deploying tern on a server
  expect it, and the trait refactor is needed anyway.
- **Remote access = TCP listener + device tokens + pairing**, modelled on the Telegram pairing that already exists.
  Self-hosters reach it over Tailscale, a reverse proxy or a tunnel. The cloud product adds an outbound relay, so a
  core behind NAT is reachable too (§6.5).
- **Mobile needs push, and push needs us.** iOS won't keep an SSE socket alive in the background, and APNs only
  accepts pushes signed with the app publisher's key. So even self-hosted cores send notifications through a small
  push relay we run, with end-to-end encrypted payloads (§8). That relay is also a natural free-tier hook into the paid
  product.

## 1. Goals and non-goals

**Goals**
- Keep the lightest install exactly as light as today: one static binary, no network listener, no database server,
  no account. Nothing in this spec may add a mandatory dependency or listener to tier 1.
- One client protocol for every tier. A GUI build works against a laptop daemon, a home server and the cloud without
  code paths per tier beyond the transport (§7.2).
- A self-hosted server deployment that is boring to operate: one container, one volume, optional Postgres,
  documented backup and upgrade.
- A hosted product people pay for because it is always on, reachable from their phone, and maintained for them, not
  because their data is locked in. Export and "move back to local" are features (§10.5).

**Non-goals (for this spec)**
- Multiple human users sharing one core (teams, RBAC). The model stays *one owner per core*. Device scopes (§6.2)
  are about limiting a lost phone, not about other people. Teams would be a separate spec.
- High availability with several active cores on one database. §4.6 keeps one active core per database and sketches
  what HA would need.
- Windows-specific packaging. The TCP listener (§6.1) does remove the "no non-Unix transport" blocker noted in the
  GUI spec.

## 2. Install tiers

| Tier | What you install | Storage | Clients | Reachable from | Who runs it |
|---|---|---|---|---|---|
| 1. **Core** | `tern` binary (brew, curl script, cargo) | SQLite in `~/.tern` | TUI, `tern run`, Telegram | this machine (Unix socket) | the user |
| 2. **Core + desktop** | tier 1 + `tern.app` (may bundle the binary) | SQLite | + desktop GUI | this machine | the user |
| 3. **Server core** | container image or binary on a VPS / home server | SQLite on a volume, or Postgres | TUI (remote attach), desktop, mobile, Telegram | LAN, Tailscale, reverse proxy, tunnel | the user |
| 4. **tern Cloud** | nothing server-side; apps sign in | managed (§10.3) | desktop, mobile, TUI (remote attach), Telegram | anywhere | us |

Rules that keep the tiers honest:
- **Same binary.** Tiers 1–3 run the same `tern` executable. The server image (§9) is the static musl build with the
  `postgres` and `tls` features on. The default brew/curl build may leave `postgres` off to stay small (§11).
- **The desktop app never *requires* the CLI install.** It can bundle `tern` in `Contents/Resources` (already an open
  question in gui-spec §11.3). Recommendation: bundle it, and if a `tern` on `PATH` is newer and its API is compatible,
  prefer it. One daemon per `TERN_HOME` (the PID lock already enforces this), so the CLI and app never fight.
- **Moving between tiers is a supported operation**: local to server, server to cloud, cloud back to local, through
  `tern store export` / `import` (§4.7) plus the file inventory (§5).

## 3. Architecture

```
                         ┌──────────────── clients ────────────────┐
                         │ TUI · desktop GUI · mobile GUI · tern run│
                         └──────┬───────────────┬──────────────────┘
                     unix socket│               │HTTPS (device token / cloud JWT)
                                │               │   direct, or via relay (§6.5)
┌───────────────────────────────▼───────────────▼──────────────────────────────────┐
│ tern core (`tern daemon`)                                                         │
│  api (axum)  ─ listeners: unix │ tcp+tls          auth: owner │ device │ jwt      │
│  manager ─ agent loop ─ journal ─ tools ─ sandbox ─ MCP ─ Luau ─ scheduler        │
│  gateways: telegram (long poll │ webhook)         push: relay client (§8)         │
│  StoreBackend ─────────────┬───────────────────   Library (files, §5)             │
└────────────────────────────┼──────────────────────────────────────────────────────┘
                 ┌───────────┴───────────┐
             SQLite file             Postgres
```

The core is the daemon as it exists after M5. This spec adds four seams to it: the store trait (§4), the library
abstraction for non-database state (§5), listeners plus auth on the API (§6), and the push client (§8). Everything else
(the agent loop, journal semantics, tools, approvals) is unchanged. That is deliberate: deployment shape shouldn't leak
into the agent.

## 4. Storage backends

### 4.1 Where it is today

- `tern-store` exposes a concrete `Store { conn: Arc<Mutex<rusqlite::Connection>> }` with about 40 synchronous methods
  across `repo.rs` and `daemon_repo.rs`, used by `tern-core` (agent, journal), `tern-tools` (memory, schedule),
  `tern-lua` (kv), `tern-gateway` (pairing, update offset) and the binary.
- The calls happen directly on tokio worker threads with no `spawn_blocking` (for example
  `journal::step` → `store.start_step` / `complete_step`). DECISIONS M0 already flags this: "revisit if contention
  shows up in the daemon". A network database makes it mandatory: a blocking Postgres client inside the runtime either
  stalls workers or panics (the sync `postgres` crate runs its own runtime).
- SQLite-specific features in the schema: `INTEGER PRIMARY KEY` rowids, FTS5 external-content tables with triggers
  (`messages_fts`, `memories_fts`) ranked by BM25, `PRAGMA user_version` migrations, WAL, `foreign_keys`.
- One SQL escape hatch outside the crate: `Store::conn()` is public, used by one test in
  `tern-core/src/agent/tests.rs`. It has to go (replace it with a test helper inside `tern-store`).
- The journal relies on one atomicity guarantee: `complete_step` writes the step's output *and* its appended messages
  in one transaction (DECISIONS M1). Any backend must keep that.

### 4.2 Library choice

| Option | Verdict |
|---|---|
| **sqlx** (0.9) for both backends | No. sqlx's SQLite driver links its own `libsqlite3-sys`. Cargo allows only one crate per `links = "sqlite3"`, so rusqlite and sqlx would have to agree on the `libsqlite3-sys` version forever: the same coupling DECISIONS M0 avoided by not using `rusqlite_migration`. Moving *everything* to sqlx would rewrite a working, tested SQLite layer and lose rusqlite's synchronous simplicity in tests. Compile-time query checking needs a live DB or offline metadata in CI, which is friction for a small team. |
| **diesel / sea-orm** | No. A DSL or ORM over ~40 hand-written queries adds a lot of dependency and macro weight for little gain. |
| **`tokio-postgres` + `deadpool-postgres` + `tokio-postgres-rustls`** for Postgres; keep **rusqlite** for SQLite | **Yes.** Small, mature, async-native, rustls-capable, so no OpenSSL (keeps the musl rule). Hand-written SQL, as today. The rustls stack is already in the tree via reqwest. |

Check current versions and the rustls adapter's maintenance status when starting. Measure the binary size delta of
the `postgres` feature and record it in DECISIONS, as the Inngest spike did. Budget: +2 MB at most.

### 4.3 The trait

```rust
// tern-store/src/lib.rs (sketch)
#[async_trait]
pub trait StoreBackend: Send + Sync + 'static {
    // sessions
    async fn create_session(&self, s: &NewSession<'_>) -> Result<SessionRow>;
    async fn session(&self, id: &str) -> Result<Option<SessionRow>>;
    // … one method per existing Store method, same names and row types …

    // journal: must stay atomic
    async fn complete_step(&self, run_id: &str, idx: u32, out: &StepCompletion<'_>) -> Result<()>;

    // search: backends rank differently but share the contract in §4.4
    async fn search_messages(&self, q: &SearchQuery<'_>) -> Result<Vec<MessageHit>>;
    async fn search_memories(&self, q: &SearchQuery<'_>) -> Result<Vec<MemoryRow>>;

    // ops
    async fn schema_version(&self) -> Result<u32>;
    async fn health(&self) -> Result<StoreHealth>; // integrity_check / SELECT 1 + version
    fn kind(&self) -> StoreKind;                     // Sqlite | Postgres
}

#[derive(Clone)]
pub struct Store(Arc<dyn StoreBackend>);   // keeps the cheap-clone handle callers already use
```

Design rules:
- **Coarse-grained methods, no generic transactions.** Callers never see a transaction or connection object. Each
  method that needs atomicity (`create_run` with its input message, `complete_step` with messages, `consume_pairing_code`,
  `mark_schedule_ran`) is one transaction inside the backend. This keeps the trait object-safe and keeps SQL out of
  callers.
- **Row types stay as they are** (`SessionRow`, `RunRow`, `StepRow`, …). IDs stay `String` (UUIDv7 hex) and `i64`
  millisecond timestamps, so nothing above the store changes shape.
- **SQLite implementation**: one dedicated OS thread owns the `rusqlite::Connection`. Methods send a closure over a
  channel and await a oneshot reply. That's simpler and fairer than `spawn_blocking` plus a `Mutex`, and it keeps the
  existing synchronous query code nearly verbatim. A second read-only connection for FTS queries can come later
  (WAL allows concurrent readers) if search ever blocks writes.
- **Errors**: `StoreError` gains `Postgres(tokio_postgres::Error)`, `Pool(..)`, and `Unavailable` (the network DB is
  down). The agent treats `Unavailable` like a provider outage: the run fails as retryable and the startup triage
  (spec §5.3, `triage_running_runs`) resumes it later. It doesn't crash the daemon.
- **Migration path for callers**: mostly mechanical, adding `.await`. Do it as its own change (phase D0) with SQLite only
  and no behaviour change. The Postgres backend then lands without touching callers.

### 4.4 Schema on Postgres

Same tables, columns and indexes. Differences:

| SQLite | Postgres | Notes |
|---|---|---|
| `INTEGER PRIMARY KEY` (messages, memories, allowlist) | `BIGINT GENERATED ALWAYS AS IDENTITY` | ids stay `i64` |
| `INTEGER` ms timestamps | `BIGINT` | no `timestamptz`; keep one representation across backends |
| `TEXT` JSON (`content_json`, `input_json`, `output_json`, `usage_json`) | `TEXT` | **not** `JSONB`: JSONB reorders keys and drops duplicates, and the journal hashes canonical JSON. Byte-for-byte storage keeps replay identical. JSONB columns can be added later for analytics without changing the canonical ones |
| `enabled INTEGER` | `BOOLEAN` | mapped in the backend |
| FTS5 external content + triggers | `text_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', text)) STORED` + GIN index | no triggers needed |
| `bm25(messages_fts)` ranking | `ts_rank_cd(text_tsv, query)` | see below |
| `PRAGMA user_version` | `tern_schema (version INT NOT NULL)` single-row table | |
| `PRAGMA foreign_keys` | native | |

**Search contract.** `fts_query` builds an OR of quoted words today, so user text can't inject syntax (DECISIONS M2).
On Postgres the same word list becomes `to_tsquery('simple', 'w1 | w2 | …')`, built from lexemes, never from raw user
syntax and never via `websearch_to_tsquery` on raw input. The `simple` configuration (no stemming) matches FTS5's
default `unicode61` tokenizer most closely, so results between backends are comparable. The contract tests (§4.8)
assert *which documents match*, and only loosely *which comes first*: Postgres' `ts_rank_cd` is not BM25 and ranks
worse on large corpora. If ranking quality becomes a complaint, detect a BM25 extension at startup (`pg_textsearch` or
ParadeDB `pg_search`) and use it when present. Not required, since many managed Postgres providers don't offer them.

**Migrations.** `migrations/sqlite/NNNN_*.sql` and `migrations/postgres/NNNN_*.sql`, one list each, the same length,
enforced by a unit test. Version N means the same logical schema on both. The existing four files move to
`migrations/sqlite/` unchanged (the embedded SQL text doesn't change, so existing databases are unaffected).
Postgres migrations run under `pg_advisory_xact_lock` so two cores starting at once can't both migrate. "Refuse a newer
schema" (`StoreError::TooNew`) applies to both backends.

### 4.5 Config

```toml
[store]
backend = "sqlite"                 # "sqlite" (default) | "postgres"
# sqlite: path defaults to $TERN_HOME/tern.db
url_secret = "TERN_DATABASE_URL"   # postgres: secret holding postgres://user:pass@host/db?sslmode=require
pool_size = 8
schema = "tern"                    # postgres: lets tern share a database with other apps
```

The URL is a secret, not a config value, because it holds a password. Env first, then `secrets.toml`: the existing
resolution order, which suits containers. `sslmode=require` is the default for any non-localhost host. `tern doctor`
gains a store section: backend, reachability, schema version, latency of `SELECT 1`.

### 4.6 One active core per database

Today the PID lock guarantees one daemon per `TERN_HOME`, and much of the daemon assumes it: startup triage resumes
every `running` run, the scheduler advances `next_run_at` without claiming it, and in-memory routes hold API approvals.
Two cores on one Postgres database would double-run everything.

- **v1 rule: a core takes `pg_try_advisory_lock(<tern-core key>)` on a dedicated connection at startup and holds it.**
  If it can't, it refuses to start ("another tern core is using this database"), exactly like the PID lock. If the
  lock connection drops, the core stops scheduling and starting runs and tries to re-acquire. Runs in flight are left
  `running`, which the next owner resumes. This gives cold-standby failover (start a second container; it waits on the
  lock) for free.
- **What real HA would need (not now):** run leases (`runs.lease_owner`, `lease_expires_at`, heartbeat in the journal
  step path), scheduler claims with `SELECT … FOR UPDATE SKIP LOCKED`, approvals and event fan-out through
  `LISTEN/NOTIFY` or the event table (§6.3), and Telegram long polling pinned to one instance (Bot API allows only one
  `getUpdates` consumer). That's the shared multi-tenant design in §10.7, and the only real reason to build it.

### 4.7 Moving data between backends

- `tern store export [--out file.jsonl.zst]`: a versioned, backend-neutral dump, one JSON object per row, tables in FK
  order, with the schema version in a header line. Also usable as a backup.
- `tern store import <file>` into an empty store of either backend. Refuses a non-empty target unless `--force`.
- `tern store migrate --to postgres://…` = export plus import in one step, then prints the config change to make.
- The export contains no secrets (§5). Moving a core is export + the library files (§5) + re-entering secrets, or the
  encrypted secrets bundle in §5.2.

### 4.8 Tests

- One **conformance suite** (`tern-store/tests/conformance.rs`) generic over `StoreBackend`, run against SQLite always
  and against Postgres when `TERN_TEST_DATABASE_URL` is set. CI gets a Postgres service container for one job. All
  existing store tests move into it.
- The M1/M2/M5 end-to-end tests (crash resume, compaction, scheduler) get a Postgres variant in the same CI job:
  those are the ones that prove journal atomicity holds over the network.
- A replay test: run a scripted conversation on SQLite, export, import into Postgres, then resume a run cut off mid-step
  and assert it produces identical step hashes.

## 5. State outside the database

A deployed or hosted core can't assume a home directory someone can `vim`. Inventory of `$TERN_HOME`:

| Item | Today | Tier 3 (server) | Tier 4 (cloud) |
|---|---|---|---|
| `config.toml` | file | file on volume, plus `TERN_*` env overrides for container-friendly keys (store, listeners, auth) | generated by the control plane, read-only to the user except through `/v1/config` |
| `secrets.toml` | file, 0600 | env vars (orchestrator secrets), or the encrypted store below | encrypted store, key from KMS |
| `persona.md`, `user.md` | files | volume | library in DB |
| `skills/`, `connectors/`, `connectors/drafts/` | dirs | volume | library in DB |
| workspace | dir | volume (`/data/workspace`) | tenant volume |
| `tern.db` | file | volume or Postgres | §10.3 |
| logs | files | **stdout as JSON** when `TERN_LOG_FORMAT=json` (containers) | collected by the platform |
| socket, pid | files | unchanged (local admin via `docker exec tern tern …`) | unchanged |

### 5.1 Library abstraction (phase D5, needed for the cloud)

Skills, connectors, drafts and the persona files are read in a few places (`skills.rs`, `tern-lua` loading,
`prompt.rs`, `connector_write`). Put them behind a `Library` trait with `list`, `read`, `write`, `delete` over logical
paths (`skills/foo/SKILL.md`), with two implementations: `FsLibrary` (today's layout, the default everywhere) and
`StoreLibrary` (a `library_files(path PK, content BYTEA/BLOB, sha256, updated_at)` table). A filesystem core never
changes behaviour. A core using `StoreLibrary` is a single unit of state, so backup, export and moving between hosts
need only the database. Agent-written connectors and GUI edits go through the API either way, so clients can't tell
the difference.

The workspace stays a real directory in every tier: `shell` needs a filesystem.

### 5.2 Secrets at rest

- Tier 1–2: unchanged (`secrets.toml`, 0600, env first).
- Tier 3: env vars are the recommended path. Optional encrypted store: a `secrets` table holding XChaCha20-Poly1305
  ciphertexts (RustCrypto `chacha20poly1305`, pure Rust), key from `TERN_MASTER_KEY` (32 bytes, base64). Written by
  `PUT /v1/secrets/{name}` (write-only, as already planned in gui-spec §7.1). Never returned by any endpoint.
  Redaction keeps working, since it uses the resolved values in memory.
- Tier 4: the same table, with the data key wrapped by the cloud KMS (envelope encryption). The control plane never
  sees plaintext secrets after the user enters them in the app, which they send directly to their core.
- `tern store export --with-secrets` produces a separately encrypted bundle (passphrase via age or the same AEAD), so
  moving a core doesn't mean re-entering every key.

### 5.3 Time zones (small, but it bites servers)

Cron is evaluated in `chrono::Local` (DECISIONS M5), and so are the times tools show the model. On a laptop that's the
user's zone. In a container it's UTC, so "every weekday at 8 am" would fire at 8 am UTC. Fix:
- A `timezone` config key (IANA name), defaulting to the system zone, and a `schedules.tz` column (migration) set at
  creation. Cron is evaluated in the schedule's zone and the prompt shows times in the user's zone.
- Needs `chrono-tz` (embeds the tz database; measure the size, around 100–300 KB depending on filtering) or
  `jiff`, which reads the system tzdb and can bundle it. Decide at implementation. This is needed for tier 3 and is
  harmless for tier 1, so do it in D0.
- The GUI sends the device's zone at pairing time as the default for that core when none is configured.

## 6. Network API and remote access

### 6.1 Listeners

```toml
[api]
listen = ["unix"]                         # default: unchanged
# listen = ["unix", "tcp://0.0.0.0:7420"] # server core
tls = "auto"           # "auto" | "files" | "off"
tls_cert = "/data/tls/cert.pem"           # for tls = "files"
tls_key  = "/data/tls/key.pem"
trust_proxy = false    # honour X-Forwarded-For/Proto from a reverse proxy on loopback
```

- **Unix socket is always on** and keeps its current meaning: the OS user who can open it is the owner, with no token.
  This is the local admin path even on a server (`docker exec … tern devices pair`).
- **TCP is off by default** and requires auth (§6.2) on every request, *including* loopback: any local process or user
  can reach a loopback port, unlike a 0600 socket.
- **TLS**:
  - `auto`: on first start, generate a self-signed certificate (`rcgen`), store it in `$TERN_HOME/tls/`, and pin its
    SHA-256 fingerprint in the pairing payload (§6.2). Clients pin it, so it's secure without a CA. That suits
    LAN and Tailscale use, where nobody has a public hostname.
  - `files`: a real certificate (from Caddy, certbot, Tailscale `tailscale cert`). Clients verify normally, no pin.
  - `off`: allowed only when every TCP listener is loopback **and** `trust_proxy = true`, meaning a TLS-terminating
    proxy is in front. Otherwise the core refuses to start: no plaintext tokens on a network.
- Implementation: axum 0.8 serves any `tokio::net::TcpListener`. TLS is a `tokio-rustls` accept loop with
  `hyper-util`, or `axum-server` with its rustls feature if that's maintained and on our axum/hyper versions. Same
  router for both listeners. An `Auth` extension on the request carries `Owner | Device { id, scope } | CloudUser {..}`.
- `GET /healthz` (no auth, no details) for orchestrators. `/v1/health` (auth required on TCP) keeps the details.

### 6.2 Device pairing and tokens

Modelled on Telegram pairing (DECISIONS M5): an owner-initiated, short-lived, single-use code.

1. Owner runs `tern devices pair [--scope owner|chat|read] [--name "Mitch's iPhone"]`, or clicks **Add device** in the
   desktop app (which is the owner over the local socket). The core creates an 8-character code (same alphabet, 10-minute
   TTL) and prints a QR code containing:
   `tern://pair?u=https://home.example.ts.net:7420&c=K7QX9M2P&fp=sha256:AB12…&n=<core name>`
   (`fp` only for `tls = auto`).
2. The phone or remote desktop scans or pastes it, connects with the pinned fingerprint, and calls
   `POST /v1/auth/pair {code, device_name, platform, push?: {...}}` (§8 for `push`).
3. The core returns `{device_id, token}`. The token is `tern_dev_` + 32 random bytes base62. Stored as a blake3 hash
   (already a dependency) in a `devices` table: `id, name, platform, scope, token_hash, created_at, last_seen_at,
   last_ip, revoked_at, push_json`.
4. Clients send `Authorization: Bearer tern_dev_…`. The token lives in the Keychain / Android Keystore
   (`flutter_secure_storage`), never in app preferences.

Scopes, enforced per route:
- `owner`: everything, including config, secrets, connectors activation, devices.
- `chat`: conversations, approvals, schedules, memories. No config, secrets, devices, or connector activation. The
  default for phones, so a lost phone can't exfiltrate API keys or install code.
- `read`: view only. For a dashboard or a widget.

Management: `tern devices list | revoke <id> | rename`, the same in the GUI (owner scope only), and
`GET/DELETE /v1/devices[/{id}]`. Revocation takes effect immediately, closing that device's SSE streams.

Hardening: pairing attempts are rate-limited per IP (5/min) and codes are single-use. Request bodies are capped (1 MB,
except uploads). No CORS headers (no browser clients), audit lines in the log for pairing, revocation and every
owner-scope mutation from a device.

### 6.3 Events over unreliable links

SSE stays the transport: it's plain HTTP, proxies and tunnels handle it, and the Dart client already plans for it. A
phone on cellular drops connections constantly, though, and today's resync-on-reconnect (gui-spec §6.3) means a full
refetch every time. Add replay:
- Every event gets a monotonically increasing `seq` (the SSE `id:` field, as gui-spec §7.1 already proposes).
- The core keeps recent events in an `events(seq, at, session_id, json)` table with a retention window (24 h or
  10 000 rows), written by the bus asynchronously so the agent loop never waits on it. Text deltas are *not* persisted
  (too many, and the final `assistant_message` supersedes them).
- `GET /v1/events` with `Last-Event-ID: n` replays from `n+1` and then streams live. If `n` is older than the window,
  the core sends one `resync_required` event, and the client does the full refetch.
- Heartbeat comment every 15 s (axum `KeepAlive`), so mobile networks and proxies don't cut idle streams.
- No WebSockets. Everything client → core is a normal POST, and SSE + HTTP/2 multiplexes fine.

### 6.4 Approvals when nobody is looking

Today: no API client attached → deny. With remote and mobile clients, "attached" is the wrong test, because the phone
is in a pocket with its SSE stream closed. New rule when at least one paired device has push enabled:
- The approval is held for `approvals.wait` (default 10 minutes, configurable, max 24 h). The run stays `running`
  with the approval step journaled as started, and a push goes out (§8).
- Any client answering resolves it everywhere (`approval_resolved` event, already planned).
- On timeout the answer is deny, and the run continues, as it does today with an immediate deny.
- A daemon restart while an approval waits is already handled: the run resumes and re-asks.
With no push-capable device, keep today's immediate deny, so tier 1 behaviour doesn't change.

### 6.5 Reachability options for a server core

| Option | Effort for the user | tern code needed | Notes |
|---|---|---|---|
| Same LAN | none | §6.1–6.2 | works at home only |
| **Tailscale / WireGuard** | install Tailscale on both ends | none | **recommended default for self-hosters**. Private, NAT-proof, gives a stable name and `tailscale cert` for `tls = files`. Write a guide, not code |
| Reverse proxy (Caddy) + domain | DNS + proxy config | `trust_proxy` | for people who already run one |
| Cloudflare Tunnel / ngrok | account + daemon | none | works, but routes traffic through a third party |
| **tern relay** (part of the cloud product) | sign in | yes (below) | the core dials out, so no open ports, no domain |

**tern relay.** The core opens an outbound HTTP/2 (or WebSocket) connection to `relay.tern.cloud` and registers
with a relay token from the user's cloud account. Clients connect to `https://<core-id>.relay.tern.cloud`, and the
relay forwards requests and streams down that connection. TLS: the client ↔ core session stays end-to-end encrypted
(the relay forwards a TLS stream addressed by SNI, pinned to the core's `auto` certificate), so the relay can't read
traffic. This is the same "worker connects out" shape as Inngest Connect (see the V2 research doc), so there may be
shared infrastructure or at least shared design there. It makes a laptop or home box behave like a cloud core
from the phone's point of view, and it's a good paid feature that doesn't require hosting anyone's agent.

### 6.6 Shell and sandbox on a reachable core

A core reachable over the network with the `host` sandbox runs model-chosen shell commands on that host. That's
fine on a laptop the user is sitting at. It's a larger risk on a server reachable from a phone. Rules:
- If any TCP listener is enabled and `sandbox.backend = "host"`, `tern doctor` warns, and the daemon logs a warning at
  start. Not a hard refusal: in the server image the container *is* the sandbox (§9.2).
- Device scope `chat` can approve shell commands. That's the point of mobile approvals. The approval card on mobile shows
  the full command, never only the summary.

## 7. GUI connection model

Replaces gui-spec §6.1 and §7.4.

### 7.1 Connection profiles

The app keeps a list of **cores**. Each window shows one. The sidebar footer (gui-spec §2.1, "daemon status") becomes a
core switcher.

| Kind | Configured by | Transport | Credential |
|---|---|---|---|
| **This Mac** | automatic: `$TERN_HOME` socket, or the bundled daemon | Unix socket | none (OS user) |
| **Remote** | scanning or pasting a pairing link (§6.2) | HTTPS, pinned fingerprint or CA | device token (Keychain) |
| **Cloud** | signing in to a tern account | HTTPS to the cloud edge or relay | short-lived JWT + refresh token (§10.4) |

Signing in to a cloud account lists that account's cores: the hosted ones *and* self-hosted cores registered with the
relay. One account can therefore be "my cloud core" plus "my home server".

Mobile has no "This Mac". On first launch it offers *Sign in* or *Scan pairing code*.

### 7.2 `tern_client` transports

```dart
abstract interface class TernTransport {
  Future<HttpClientResponse> send(String method, String path, {Object? body, Map<String, String>? headers});
  Stream<SseEvent> events({int? lastEventId});
}
final class UnixTransport implements TernTransport { /* HttpClient.connectionFactory → unix socket */ }
final class HttpsTransport implements TernTransport { /* base URL, bearer token, optional SHA-256 pin */ }
final class CloudTransport implements TernTransport { /* HttpsTransport + token refresh + core routing */ }
```

- Pinning in Dart: `SecurityContext(withTrustedRoots: false)` plus `badCertificateCallback` comparing
  `sha256(cert.der)` with the pin. Reject everything else. Unit-test against a self-signed fixture.
- Everything above the transport (models, SSE reducer, stores) stays transport-agnostic, as gui-spec §6.4 already
  requires.
- A 401 means token revoked or expired → the profile shows "Signed out of <core>" with re-pair or sign in. No
  retry loop.

### 7.3 Capabilities instead of assumptions

`/v1/health` gains:

```json
{ "api": 1, "version": "0.4.0", "deployment": "local" | "server" | "cloud",
  "store": "sqlite" | "postgres", "library": "fs" | "store",
  "auth": { "kind": "owner" | "device" | "cloud", "scope": "owner" | "chat" | "read" },
  "features": ["schedules", "push", "uploads", "event_replay", "devices", …] }
```

The GUI derives behaviour from it rather than from the profile kind:
- **Start daemon / Run at login / Reveal in Finder / Open in editor** only for `deployment = local`.
- **Attach file**: local copies into the workspace by path, as gui-spec §2.2 says. Remote uses a new
  `POST /v1/uploads` (multipart, 25 MB cap, stored under `workspace/uploads/<uuid>/<name>`, path returned). The
  feature flag says which.
- **Connector editing**: local can open the file. Remote edits through the API only (gui-spec P2 in-app editor becomes
  the only option there).
- Owner-only screens (Settings → Models, secrets, devices) are hidden for `chat` scope.

### 7.4 Reconnect and offline

- Reconnect backoff as in gui-spec §6.2 (0.5 s doubling to 10 s), plus `Last-Event-ID` replay (§6.3). On mobile,
  reconnect on app foreground and on network change (`connectivity_plus`), and close the stream on background (iOS will
  anyway).
- Composing while offline: the message stays in the composer with "Not connected". No offline queue in v1, since
  queued sends that land minutes later would cancel whatever run started meanwhile (the one-run-per-session rule).

## 8. Push notifications

### 8.1 Why a relay is unavoidable

- iOS suspends backgrounded apps and their sockets, so a closed SSE stream can't deliver an approval request. Waking
  the app requires APNs (alert or background push).
- APNs only accepts pushes authenticated with the **app publisher's** key (`.p8` token or certificate) for that bundle
  id. A self-hosted core can't have our key, and we can't hand it out. FCM is the same for the published Android app.
- So every core, self-hosted included, sends pushes through a relay we run that holds the keys. This is how Home
  Assistant's companion app and ntfy's hosted push work.

### 8.2 Design

- At pairing (§6.2) a mobile device sends `push: {provider: "apns" | "fcm", device_token, public_key}`. The
  `public_key` is an X25519 key generated on the device, with its private half in the Keychain.
- Core → relay: `POST https://push.tern.cloud/v1/send {provider, device_token, ciphertext, collapse_id, kind}`,
  authenticated with a per-core relay key (free registration, rate-limited). `ciphertext` is the notification body
  (title, text, `approval_id`, `session_id`) sealed to the device key (HPKE or crypto_box). **The relay sees only
  opaque bytes and a device token.**
- The relay forwards as an APNs `mutable-content` alert. An iOS Notification Service Extension decrypts and renders it.
  Android decrypts in the FCM handler. If decryption fails, the fallback text is "tern needs your attention".
- **Approval actions** (Allow once / Deny) on the notification call the core directly with the device token (in the
  background on Android; on iOS a notification action with `authenticationRequired` launches the app briefly to POST).
  The relay never carries decisions.
- Kinds: `approval` (time-sensitive interruption level), `run_finished` (only for runs started from that device,
  or scheduled runs whose `deliver_to` is `device:<id>`), `run_failed`. A new delivery target `device:<id>` (and
  `devices`, meaning all push-capable devices) joins `telegram:<chat_id>` and `none`.
- Desktop doesn't need the relay: native notifications from the running app (gui-spec §5).
- Android without Google services: optional UnifiedPush/ntfy endpoint instead of FCM, sent directly from the core,
  with no relay. Low priority.

### 8.3 Pricing hook

Push through the relay is free for a modest volume (enough for a person) for self-hosted cores, and unlimited with a
cloud account. That keeps self-hosting fully usable while making the account the obvious upgrade.

## 9. Self-hosted server core (tier 3)

### 9.1 Image

- `ghcr.io/<org>/tern:<version>`: the static musl build with `postgres` and `tls` features (§11), on `distroless/static`
  or `scratch` plus CA bundle (the webpki roots are already merged in, `net.rs`) and `tzdata` if the tz approach in
  §5.3 needs it. Runs as a non-root user (uid 10001). Read-only root filesystem except `/data`.
- Optional "tools" variant with `git`, `python3`, `curl`, `jq`, `ripgrep`: the shell tool is only as useful as what's
  installed, and most users will want this one. Size it honestly in the docs.
- Entrypoint `tern daemon`. `TERN_HOME=/data`. Container-friendly env overrides: `TERN_DATABASE_URL`,
  `TERN_API_LISTEN`, `TERN_MASTER_KEY`, `TERN_LOG_FORMAT=json`, `TERN_TIMEZONE`, provider keys by their secret
  names.
- `tern daemon` already handles SIGTERM by leaving runs `running` for the next start (DECISIONS M5), which is exactly
  what rolling container restarts need.

### 9.2 Sandbox inside a container

The docker sandbox backend shells out to the `docker` CLI. Inside a container that means mounting the host's Docker
socket, which is root on the host. Options:
1. **`host` backend, container as the boundary (recommended default in the image).** Shell runs inside the tern
   container as the non-root user, with the workspace on `/data/workspace`. Single-tenant, disposable container, no
   host access. Document what it can reach (the network, `/data`).
2. Docker socket mount for sibling sandbox containers. Supported, documented as "gives tern root on the host".
3. A rootless option later (gVisor `runsc`, bubblewrap, or the microVM backend the cloud needs anyway, §10.2). Track in
   the sandbox work.

### 9.3 Example compose file (to ship in `deploy/compose.yaml`)

```yaml
services:
  tern:
    image: ghcr.io/<org>/tern:0.4
    restart: unless-stopped
    environment:
      TERN_API_LISTEN: "unix,tcp://0.0.0.0:7420"
      TERN_DATABASE_URL: "postgres://tern:${PG_PASSWORD}@db/tern"
      TERN_TIMEZONE: "America/Los_Angeles"
      ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY}"
    volumes: ["tern-data:/data"]
    ports: ["7420:7420"]
    depends_on: [db]
  db:
    image: postgres:17
    environment: { POSTGRES_USER: tern, POSTGRES_PASSWORD: "${PG_PASSWORD}", POSTGRES_DB: tern }
    volumes: ["pg-data:/var/lib/postgresql/data"]
volumes: { tern-data: {}, pg-data: {} }
```

Plus a SQLite-only variant with no `db` service, which is what most people should start with.

### 9.4 Operations

- **First run**: `docker compose exec tern tern config init` then `tern devices pair` (prints the QR in the terminal).
  The desktop app's "Add remote core" flow says exactly this.
- **Backups**: SQLite with `VACUUM INTO` on a schedule (a `tern store backup <path>` command), or Litestream alongside
  for continuous S3 replication. Postgres with `pg_dump` or the provider's backups. `tern store export` works for both.
- **Upgrades**: pull and restart. Migrations run at start under the advisory lock. A newer schema than the binary is
  refused, so rollback means restoring a backup. Say so in release notes for each migration.
- **Observability**: JSON logs to stdout. A Prometheus `/metrics` endpoint (runs by status, step latency, provider
  errors, token usage) is a later nice-to-have behind owner auth or a separate loopback listener.

## 10. tern Cloud (tier 4, the paid product)

### 10.1 What people pay for

- **Always on**: schedules run and Telegram answers while the laptop sleeps.
- **Everywhere**: desktop and mobile on one core, push included, no networking setup.
- **Maintained**: upgrades, backups, sandboxing and monitoring done for them.
- **Optional bundled models**: no API keys to manage (§10.4).
- **Relay for self-hosted cores** (§6.5) and push (§8): the low tier, for people who want to keep their core at home.

### 10.2 Architecture: one core per tenant first

```
 apps ──HTTPS──▶ edge (TLS, auth, routing by core id) ──▶ tenant core (microVM, volume)
                    │                                          │ outbound only:
 control plane ◀────┘                                          ├─ LLM providers / model gateway
   accounts, billing (Stripe), provisioning, wake scheduler,   ├─ Telegram, MCP, connector hosts
   KMS, push relay, relay, usage metering                      └─ push relay
```

| | (A) Instance per tenant | (B) Shared multi-tenant cores |
|---|---|---|
| Engine changes | small: JWT auth, library in DB, wake hooks | large: `tenant_id` everywhere, run leases, HA (§4.6), per-tenant tool/MCP/Luau isolation inside one process |
| Isolation | strong: a tenant's shell, MCP servers and Luau VMs share nothing | the hardest part: shell and MCP stdio servers per tenant still need separate sandboxes |
| Cost per idle tenant | low *if* suspended (scale to zero) | lowest |
| Fits tern's design | yes: it's the self-hosted core, run by us | fights the "one owner, one process" model |

**Recommendation: (A).** Each tenant gets a core in a Firecracker-class microVM (Fly Machines, or our own Firecracker
fleet later) with a persistent volume. The agent runs arbitrary shell commands, so a microVM boundary is needed
regardless, and per-tenant processes are what tern already is. Revisit (B) only if per-tenant cost dominates the
business.

### 10.3 Storage for cloud tenants

Under (A), two reasonable options:
- **SQLite on the tenant volume + Litestream to object storage.** Cheapest, no shared database to operate, restore =
  new volume + Litestream restore. Matches tier 1–2 exactly, so "move to cloud" and "move back" are file copies.
- **Per-tenant database (or schema) in a shared managed Postgres cluster.** Easier fleet-wide queries and PITR, at the
  cost of an always-on cluster and connection limits across thousands of tenants (needs PgBouncer-style pooling).

Start with SQLite + Litestream. Keep the Postgres backend for tier 3 and for (B). This is the main reason the Postgres
work (D3) and the cloud (D5) are independent in §12.

### 10.4 Auth, models and metering

- **Accounts**: Sign in with Apple, Google, and email magic link, run by the control plane (or a provider such as Clerk,
  WorkOS or Auth0; decide on pricing and Apple's requirement to offer Sign in with Apple when other social logins
  exist).
- **Core auth mode `jwt`**: the core is configured with the control plane's JWKS URL and its own core id. Clients get a
  short-lived (15 min) access token scoped to `core:<id>` with a device id claim, refreshed by the app. The core
  verifies the signature, audience, expiry and a revocation list it pulls periodically. Device scopes (§6.2) apply as
  claims, so the same route checks work.
- **Models**: BYOK (secrets, as today) *or* "tern models": the core's provider points at a model gateway the control
  plane runs (an OpenAI-compatible or Anthropic-compatible endpoint, `ProviderKind` already supports both), authenticated
  with a per-core key. The gateway meters tokens per tenant and enforces plan limits. The journal's `usage_json` gives
  an independent per-run usage record that the core can report for reconciliation.
- **Billing**: Stripe subscriptions plus metered model usage. Plan limits enforced in the control plane (suspend,
  gateway refusal), never inside the agent loop.

### 10.5 Lifecycle

- **Provisioning**: sign up → create volume + core VM → generate config (`[api] listen = tcp`, auth `jwt`,
  `library = store`, tz from the device) → healthy → return core id. A durable workflow (retries, compensation on
  failure). This is a natural place to use Inngest (see §10.6).
- **Scale to zero and wake**: idle cores (no client, no run, no approval waiting for N minutes) are suspended. Wake
  triggers:
  - an API request at the edge (cold start budget ≤ 2 s: the binary starts in milliseconds, so this is VM resume
    time),
  - Telegram: in the cloud the gateway switches from long polling to **webhook mode** (`setWebhook` to the edge),
    which wakes the core,
  - schedules: before suspending, the core reports its next `next_run_at` to the control plane, which wakes it a
    minute before. A new core endpoint `GET /v1/internal/next-wake` (internal auth only).
- **Upgrades**: rolling, per tenant, at idle. Same migration rules as tier 3. Canary cohort first.
- **Export and leave**: one button produces `tern store export --with-secrets` plus the library. `tern cloud pull`
  restores it into a local `TERN_HOME`. `tern cloud push` does the reverse for onboarding. Making leaving easy is
  part of the pitch, and it's cheap given §4.7.
- **Deletion**: account deletion destroys the volume, backups (after the retention window) and KMS keys.

### 10.6 Where Inngest fits

The V2 research doc concluded Inngest belongs at tern's edges, not inside the agent loop. The cloud control plane is
exactly such an edge and a good fit, in any language: provisioning and deprovisioning workflows, wake-before-schedule
timers (`step.sleep_until`), billing webhooks, upgrade rollouts with concurrency limits per region, and usage
aggregation. The core itself stays Inngest-free, so tiers 1–3 are unaffected.

### 10.7 Path to shared cores (only if needed)

If density forces (B): Postgres backend with `tenant_id` on every table plus row-level security keyed by a session
variable set per connection, run leases and scheduler claims (§4.6), per-tenant sandboxes as separate microVMs
attached on demand, Telegram in webhook mode, and per-tenant MCP processes managed by a supervisor. That's a large
project. The design above doesn't block it, and nothing earlier depends on it.

### 10.8 Abuse and safety (cloud only)

Agent shell access on our infrastructure invites crypto mining, spam and scanning. Needs: per-VM CPU and egress caps,
egress rate limits and SMTP blocked by default, outbound connection logging, sign-up friction (payment method for
plans with shell), and a kill switch per tenant in the control plane.

### 10.9 Pricing sketch (placeholder numbers, to validate)

| Plan | Price | Includes |
|---|---|---|
| Self-hosted | free | everything in tiers 1–3, push relay at personal volume |
| Connect | ~$5/mo | relay for a self-hosted core (§6.5), unlimited push |
| Cloud | ~$15–20/mo | a hosted core, 10 GB, backups, relay, push, BYOK models |
| Cloud + models | usage on top | the model gateway at cost plus a margin |

## 11. Build, features and packaging

| Cargo feature | Default build (brew/curl) | Server image | Adds |
|---|---|---|---|
| `postgres` | off (decide after measuring size) | on | tokio-postgres, deadpool, rustls adapter |
| `tls` | on | on | tokio-rustls server accept, rcgen |
| `push` | on | on | HPKE/crypto_box for payload sealing |

- No feature may change behaviour for a config that doesn't use it. The default config keeps `listen = ["unix"]` and
  `backend = "sqlite"`.
- If `backend = "postgres"` is configured on a build without the feature, fail at start with "this tern was built
  without Postgres support; use the server image or `cargo install tern --features postgres`".
- CI: a feature matrix build (default, all features), the Postgres service job (§4.8), the musl `zigbuild` with all
  features (check that nothing brings in OpenSSL: `cargo tree -i openssl-sys` must be empty, the check the Inngest
  spike used), and an image build + smoke test (start, pair, send a message to the mock provider over TLS).
- Desktop app: bundles the default-feature `tern`. The mobile apps bundle no core.

## 12. Phases

Each is shippable on its own. Order by need, not strictly sequential: D3 and D4 are independent of each other.

| Phase | Scope | Unblocks |
|---|---|---|
| **D0 Prep** | async `StoreBackend` trait, SQLite on a dedicated thread, remove `Store::conn()` from callers; schedule time zones; `/v1/health` capabilities; event `seq` + `Last-Event-ID` replay; JSON logs | everything below; desktop GUI benefits directly |
| **D1 Remote core** | TCP listener, TLS (`auto` with pinning, `files`), device pairing, scopes, `tern devices`, uploads endpoint; GUI remote profiles and pinning in `tern_client`; TUI `--remote <url>` attach | tier 3 usable from another machine |
| **D2 Server packaging** | container image (+ tools variant), compose files, Tailscale and Caddy guides, `tern store backup`, sandbox guidance | tier 3 documented |
| **D3 Postgres** | backend, migrations, advisory-lock single active core, conformance suite in CI, `tern store export/import/migrate`, doctor | tier 3 on Postgres |
| **D4 Mobile + push** | relay service, encrypted payloads, approval wait-with-timeout, `device:` delivery, mobile app (iOS first) | phone approvals |
| **D5 Cloud alpha** | control plane, `jwt` auth, `StoreLibrary`, encrypted secrets with KMS, provisioning, scale-to-zero + wake, Telegram webhook mode, export/pull/push | tier 4 |
| **D6 Connect** | outbound relay for self-hosted cores, account-linked cores in the GUI | paid tier without hosting |
| later | metrics, model gateway, shared cores (§10.7), teams | |

## 13. Open questions

1. **Postgres in the default binary?** Depends on the measured size cost. If it's under ~1 MB, including it removes a
   "which build do I need" support question.
2. **Desktop app and the CLI**: bundle the binary and manage the daemon (recommended above), or require a separate
   install? Carries over from gui-spec §11.3.
3. **Approval timeout default** (§6.4): 10 minutes is a guess. Should scheduled runs wait longer than interactive
   ones?
4. **Cloud storage**: confirm SQLite + Litestream (§10.3) over a shared Postgres cluster for the alpha.
5. **Hosting provider** for tenant microVMs: Fly Machines (fast to start, suspend/resume built in) vs. our own
   Firecracker on bare metal (cheaper at scale, much more to run).
6. **Relay transport**: SNI-forwarded TLS (end-to-end, relay sees nothing) vs. relay-terminated TLS (simpler, enables
   web clients later, but the relay can read traffic). The spec assumes the former.
7. **Mobile platform order**: iOS first (matches macOS-first desktop) or Android first (push and background rules are
   looser)?
8. **Branding and domains** (`tern.cloud` is a placeholder throughout).

## Sources

- sqlx 0.9 (docs.rs): <https://docs.rs/crate/sqlx/latest>
- Postgres BM25 options: Tiger Data `pg_textsearch` <https://tigerdata.com/blog/pg-textsearch-bm25-full-text-search-postgres>,
  comparison of Postgres search options <https://docs.pgedge.com/vchord-bm25/v0-3-0/comparison-to-other-solution-in-postgres/>
- iOS background sockets and APNs as the wake path: <https://kiwix.lema.org/stackoverflow.com_en_all_2023-11/questions/64413865/flutter-push-notification-to-ios-android-without-firebase>,
  ntfy Flutter plugin (self-hosted SSE + push) <https://pub.dev/documentation/ntfy/latest/>
- Codebase: `crates/tern-store/src/lib.rs`, `crates/tern-store/migrations/`, `crates/tern/src/daemon/{mod,api}.rs`,
  `crates/tern/src/remote.rs`, `crates/tern-core/src/{home,secrets,config}.rs`, `DECISIONS.md` (M0–M5).
