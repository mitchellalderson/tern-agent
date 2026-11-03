# MCP server management

Status: spec, not built. Covers the engine, TUI, desktop app and CLI.

## 1. Problem

MCP servers can be configured only by hand-editing `config.toml`:

```toml
[mcp.servers.github]
transport = "stdio"
command = "github-mcp-server"
args = ["stdio"]
env_secrets = ["GITHUB_TOKEN"]
```

Nothing else touches them:

- **TUI**: no command. `/tools` lists `mcp__*` tools once a server has started, and `/reload` picks up config edits, but only when the TUI runs the engine itself. Attached to the daemon, `RemoteBackend::reload` says "restart `tern daemon`".
- **Desktop**: the side panel's tool list shows `mcp__*` tools. There is no MCP view; gui-spec §4.2 describes one, but it was never built.
- **API**: no `/v1/mcp` routes (gui-spec §7.1 lists `GET /v1/mcp` and `POST /v1/mcp/{name}/restart` as planned).
- **Status** is a free-text string inside `McpServer` (`"not started"`, `"3 tools"`, `"unavailable: …"`). Only the system prompt and `/tools` see it.
- **Errors are hard to see.** A stdio server's stderr goes to `tracing::debug!`, so "failed to start: connection closed" is all the user gets. `tern doctor` checks only that the command is on `PATH`.
- **HTTP servers can't authenticate.** `McpServerConfig` has no headers or token, so any hosted MCP server that needs a bearer token can't be used.
- **No hot reload.** `McpServers` is built once in `build_tools` and the `Agent` holds an `Arc<ToolRegistry>`, so adding a server means restarting.

## 2. Goals

- **Manage servers in the app**: list, add, edit, enable or disable, restart and remove MCP servers from the TUI (`/mcp`), the desktop app (Library → MCP) and the CLI (`tern mcp`), all through one engine API.
- **Test before saving**: start the server, list its tools, and show its stderr when it fails, before anything is written.
- **No restart**: changes apply to the next run, daemon included. Servers that didn't change keep their connection.
- **Secrets inline**: when a server needs `GITHUB_TOKEN` and it's missing, the form asks for it and stores it through the existing secrets path. Secret values never go into `config.toml`.
- **Paste a config from elsewhere**: most MCP servers document a Claude Desktop / Cursor style `mcpServers` JSON snippet. Pasting it should be enough.

Out of scope: a server registry or catalog, OAuth for MCP servers, sandboxing stdio servers, MCP resources and prompts (tools only, as today), and per-agent MCP scoping (that belongs with agent profiles, gui-spec §7.2).

## 3. Config changes

New optional fields on `McpServerConfig`. Existing configs keep working unchanged.

| Field | Type | Default | Purpose |
|---|---|---|---|
| `enabled` | bool | `true` | Disable without deleting. A disabled server isn't started and adds no tools. |
| `env` | table of strings | `{}` | Plain, non-secret environment for stdio servers (`LOG_LEVEL = "warn"`). Values that look like keys are rejected by the write path (§6.3). |
| `cwd` | string | none | Working directory for stdio servers, `~` expanded. |
| `bearer_token_secret` | string | none | HTTP only: sent as `Authorization: Bearer <value>`. |
| `header_secrets` | table of strings | `{}` | HTTP only: header name → secret name, for servers that use something else (`X-API-Key = "FOO_KEY"`). |
| `disabled_tools` | list of strings | `[]` | Remote tool names to hide from the model, e.g. a server's `delete_repo`. |

`Config::secret_uses` gains `bearer_token_secret` and `header_secrets`, so they show as "used by mcp github" in the Secrets view and stay in the redaction list.

**Name rules** (checked on write, not on load, so old configs don't break): 1–24 characters from `[a-z0-9_-]`, unique. The 24-character cap keeps `mcp__<server>__<tool>` from hitting the 64-character hash fallback in `exposed_name` for most tool names.

## 4. Engine

### 4.1 Live server set

`McpServers` becomes mutable from the inside, so the `ToolRegistry` and `Agent` don't change:

```rust
pub struct McpServers {
    servers: RwLock<BTreeMap<String, Arc<McpServer>>>,
    secrets: Arc<Secrets>,
}

impl McpServers {
    /// Diff against the current set: drop removed servers, replace changed
    /// ones, keep unchanged ones (and their connection). Added and changed
    /// servers that are enabled start in the background straight away.
    pub fn apply(&self, cfg: &BTreeMap<String, McpServerConfig>);
    pub fn statuses(&self) -> Vec<McpStatus>;
    pub async fn restart(&self, name: &str) -> Result<McpStatus, String>;
}
```

- **Removed or changed**: the old `Arc<McpServer>` leaves the map. Calls already in flight hold their own `Arc` and finish; when the last one drops, the rmcp client drops and the child process is killed. A changed server is a new `McpServer`, which `apply` starts in the background (§10, decision 3). Startup at daemon boot stays lazy, as today: only config writes start servers eagerly.
- **Restart**: drops the client, clears backoff (`failures = 0`, `retry_at = None`) and calls `ensure` straight away, so the reply shows the new state.
- **Disabled** servers stay in the map (so the list can show them) but `prepare` and `tools` skip them.
- **Replay**: a journaled call to a tool whose server was removed gets the normal "unknown tool" error result. That's the same as a server that's down, so no special case.

`Runtime` gains `reload_mcp()`, which re-reads `config.toml` and calls `apply`. Every MCP write calls it. This sits next to `reload_providers()` from provider-setup-spec §9.2; both should land on the same `Runtime` config swap.

### 4.2 Structured status

Replace the status string with:

```rust
pub enum McpState {
    Disabled,
    NotStarted,
    Connecting,
    Running { tools: usize, since: DateTime<Utc> },
    Unavailable { error: String, retry_at: Option<DateTime<Utc>>, failures: u32 },
}

pub struct McpStatus {
    pub name: String,
    pub config: McpServerConfig,       // no secret values in here
    pub state: McpState,
    pub tools: Vec<McpToolInfo>,       // { exposed, remote, description, disabled }
    pub missing_secrets: Vec<String>,
    pub server_info: Option<String>,   // name and version from the MCP initialize reply
    pub stderr_tail: Vec<String>,      // last 50 lines, redacted
}
```

`describe()` formats it the same way as today, so the system prompt is unchanged.

### 4.3 Stderr capture

The stderr reader keeps a ring buffer of the last 50 lines (each line run through `secrets.redact` first, as now) on the `McpServer`, as well as logging them. `Unavailable.error` for a stdio server that exits during startup ends with its last stderr line, so "failed to start: connection closed" becomes "failed to start: connection closed (`Error: GITHUB_TOKEN is invalid`)".

### 4.4 Verify

`tern_mcp::verify(name, &McpServerConfig, &Secrets, overrides) -> Result<Verified, VerifyError>` starts a throwaway `McpServer`, connects, lists tools and shuts it down. It never touches the live set or the config.

- `overrides` holds secret values typed in the form but not saved yet, so a token can be tested before it's stored.
- 30 s timeout (the existing `CONNECT_TIMEOUT`).
- `Verified { tools, server_info }`. `VerifyError { kind, message, stderr_tail }` with `kind` one of `CommandNotFound`, `SpawnFailed`, `ExitedEarly`, `Timeout`, `Unauthorized` (HTTP 401/403), `Unreachable`, `Protocol`, `MissingSecret`.

### 4.5 Config writes

Uses the shared config edit library from provider-setup-spec §9.1 (`tern_core::config::edit`, moved out of `config_cmd::set`), adding `upsert_mcp_server(name, &McpServerConfig)` and `remove_mcp_server(name)`. Both keep comments and layout with `toml_edit`, validate through `Config::parse`, and write atomically. If the provider spec hasn't landed, this spec builds that module first.

### 4.6 Import

`tern_mcp::import(text) -> Vec<ImportedServer>` accepts:

- The common `{"mcpServers": {"name": {...}}}` JSON (Claude Desktop, Cursor, Windsurf), or just the inner object, or a single server object.
- Per server: `command` + `args` + `env` → stdio. `url` (or `type: "http" | "streamable-http"`) → http, with `headers`. `type: "sse"` is rejected with a warning: "tern doesn't support the SSE transport yet; check whether the server also offers streamable HTTP" (§10, decision 2).
- **`env` and header values are split**: anything that looks like a key (§6.3) becomes a secret named after the variable (`GITHUB_TOKEN`), with the value carried to the save step but not into config. Everything else goes into `env`. `Authorization: Bearer x` becomes `bearer_token_secret`.
- Placeholder values (`<YOUR_TOKEN>`, `${GITHUB_TOKEN}`, `your-api-key-here`, empty) are not stored; the secret is listed as missing instead, and the form asks for it.

Import saves nothing by itself. It returns drafts that the add flow (§5) shows for review.

## 5. The add/edit flow (shared shape)

```
Paste JSON ─▶ Import ──┐
                       ├─▶ Form ─▶ Secrets ─▶ Test ─▶ Tools ─▶ Save
Stdio / HTTP ──────────┘           (missing   (verify) (review,
                                    only)               disable some)
```

- **Form**: name; transport; command, args, `env`, `cwd` (stdio) or URL, bearer token, headers (HTTP); "Ask before each call" (`requires_approval`) and "Safe to retry" (`idempotent`).
- **Secrets**: one masked field per secret the server needs that isn't set. A secret already set shows as `GITHUB_TOKEN ✓ set (env)` and can be changed. Values go to `/v1/secrets` only on Save.
- **Test**: runs verify with the typed secrets as overrides. On failure the form stays filled and shows the error and stderr tail. The user can save anyway ("Save without testing"), which is how you add a server whose dependency isn't installed yet.
- **Tools**: the list from verify with a checkbox each. Unchecked tools go into `disabled_tools`.
- **Save**: writes secrets, then config, then reloads, which starts the server. The list shows it as Connecting, then Running once its tools are listed, so they're ready before the next run. If it fails to start (say, after "Save without testing"), the row shows the error and stderr like any other unavailable server.

**Editing** opens the same form filled in. Renaming is allowed and is remove + add. The form warns that allowlist entries for `mcp__old__*` won't carry over.

**Removing** asks for confirmation, and offers to delete secrets nothing else uses (`Config::secret_uses`), like provider removal.

## 6. Security

### 6.1 Scope

stdio servers run on the daemon's host, **outside the sandbox**, with the daemon user's rights. Adding one is the same as running a command there, so:

- All MCP writes, `verify` and `import` need **Owner** scope. `GET /v1/mcp` stays **Read**.
- **stdio writes over TCP are refused** unless `api.remote_mcp_stdio = true` (default `false`). Over the Unix socket (the local owner) they're always allowed. A paired remote owner device can still add HTTP servers, enable, disable, restart and remove. The error says how to turn it on.
- `verify` on a stdio config is held to the same rule, since it spawns the command.

### 6.2 The agent can't do this

There is no tool for adding MCP servers. If the model asks the user to "add this MCP server", the user does it through these surfaces. This mirrors connectors, where the agent writes drafts but activation goes through approval.

### 6.3 Keeping secrets out of config

Every write path rejects plain `env` values, header values and `args` that look like credentials: a value matching a known prefix (`sk-`, `ghp_`, `github_pat_`, `xoxb-`, `AKIA`, …), or any value over 24 characters with high entropy under a name containing `TOKEN`, `KEY`, `SECRET` or `PASSWORD`. The error offers to move it to a secret instead: "`GITHUB_TOKEN` looks like a secret. Store it as a secret instead?" The desktop form and TUI do that move with one action.

### 6.4 Rest

- HTTP URLs: `http` only for loopback and private-range hosts, as for provider base URLs. Bearer tokens are never sent over cleartext to a public host.
- `verify` is rate limited to 10 per minute per device.
- Secret values typed in the form travel in request bodies only, and are dropped after a verify that isn't followed by a save.

## 7. API

| Method | Path | Scope | Purpose |
|---|---|---|---|
| GET | `/v1/mcp` | Read | `[McpStatus]` |
| GET | `/v1/mcp/{name}` | Read | one `McpStatus`, including `stderr_tail` |
| POST | `/v1/mcp/verify` | Owner | `{name, config, secrets?: {NAME: value}}` → `Verified` or `VerifyError`. Saves nothing |
| POST | `/v1/mcp/import` | Owner | `{text}` → `[{name, config, secrets: {NAME: value?}, warnings[]}]`. Saves nothing |
| POST | `/v1/mcp` | Owner | `{name, config, secrets?}`: store secrets, write config, reload. 409 if the name exists |
| PUT | `/v1/mcp/{name}` | Owner | replace (`{name?, config, secrets?}`; a different `name` renames) |
| PATCH | `/v1/mcp/{name}` | Owner | `{enabled?, disabled_tools?, requires_approval?, idempotent?}`: the quick toggles |
| DELETE | `/v1/mcp/{name}?remove_secrets=` | Owner | |
| POST | `/v1/mcp/{name}/restart` | Owner | reconnect now; returns the new `McpStatus` |

`auth.rs`: `/v1/mcp` isn't added to `OWNER_ONLY` (that would make `GET` owner-only too). Non-GET routes are already owner-only by default because `/v1/mcp` isn't in `CHAT_WRITES`, so nothing changes there. The stdio-over-TCP rule (§6.1) is checked in the handler, which can see whether an `Auth::Device` extension is present.

Events: `mcp_changed { name, state }` on every state change (connect, fail, restart, config write). Clients refresh their list and the tool list from it. `/v1/health` adds the `mcp` capability flag so the desktop app can hide the view against older daemons.

## 8. Clients

### 8.1 TUI

**`/mcp`** opens a panel built like `SecretsPanel`.

List mode:

```
 MCP servers                                        a add · p paste · q close
 ● github       stdio  running · 23 tools
 ○ linear       http   disabled
 ✕ postgres     stdio  unavailable · retry in 12s · `pg-mcp` exited (FATAL: password…)
 · filesystem   stdio  not started
```

Keys: ↑↓/jk move, Enter open details, `a` add, `p` paste config (import), `e` edit, Space enable/disable, `r` restart, `d` remove (confirm), `q`/Esc close.

Detail mode: config summary, server info, the tool list with each tool's enabled state (Space toggles, writing `disabled_tools`), and the stderr tail (`l` toggles it).

Add/edit mode follows §5 as steps: `Transport → Fields → Secrets → Testing → Tools → Saved`. Tab moves between fields; `args` is one field split shell-style. Secret fields use the masked input from `SecretsPanel` and stay out of input history. Paste mode is a multi-line input (bracketed paste) that runs import, then opens the form with the first draft; several servers in one paste are reviewed one after another.

Shortcuts: `/mcp add`, `/mcp restart <name>`, `/mcp disable <name>`, `/mcp enable <name>`. `/tools` gains a status line per server pointing at `/mcp`.

`Backend` trait additions, with `Err("this backend can't manage MCP servers")` defaults like the secrets methods:

```rust
fn mcp_servers(&self) -> BackendResult<Vec<McpStatus>>;
fn verify_mcp(&self, name: &str, cfg: &McpServerConfig, secrets: &BTreeMap<String, String>) -> BackendResult<Verified>;
fn import_mcp(&self, text: &str) -> BackendResult<Vec<ImportedServer>>;
fn save_mcp(&self, old_name: Option<&str>, name: &str, cfg: &McpServerConfig, secrets: &BTreeMap<String, String>) -> BackendResult<McpStatus>;
fn set_mcp_enabled(&self, name: &str, enabled: bool) -> BackendResult<()>;
fn remove_mcp(&self, name: &str, remove_secrets: bool) -> BackendResult<()>;
fn restart_mcp(&self, name: &str) -> BackendResult<McpStatus>;
```

The embedded backend calls the runtime directly; `RemoteBackend` calls §7. Verify and restart run as an `Effect` off the input loop with a spinner. `mcp_changed` events refresh an open panel.

### 8.2 Desktop app

**Library → MCP servers**, opened from the sidebar's Library section (gui-spec §4.2), in the centre column.

- **List**: a row per server with a state dot, name, transport chip, tool count, and for unavailable servers the error and retry countdown. A toggle on each row enables or disables. Header buttons: **Add server** and **Paste config**.
- **Detail** (click a row): config summary with **Edit**, **Restart**, **Remove**; server name and version; the tool list with a switch per tool and each tool's JSON Schema on expand (reuse the spec view from `tools.dart`); a collapsible **Logs** block with the stderr tail in mono and a copy button.
- **Add server sheet**: two cards, **Local command** (stdio) and **Remote URL** (HTTP), then the §5 form. `args` is a chip list (Enter adds), `env` and headers are key/value rows with a "Make secret" action per row. Missing secrets are masked fields with **Paste** and **Show**, like the provider key field. **Test connection** shows "Connected — github-mcp-server 0.4.1, 23 tools" or the error with the stderr tail, then the tool checklist and **Save**.
- **Paste config**: a multi-line field that imports on paste and opens the sheet filled in. Several servers show as tabs in the sheet.
- **Over remote core** without `remote_mcp_stdio`, the Local command card is disabled with "Adding local commands from another device is off. Turn on `api.remote_mcp_stdio` on the host."
- The side panel's tool list groups `mcp__*` tools under their server with a state dot, and a server that's down links to its detail page.

`tern_client` additions: `mcpServers()`, `mcpServer(name)`, `verifyMcp(...)`, `importMcp(text)`, `addMcp(...)`, `updateMcp(...)`, `patchMcp(...)`, `removeMcp(...)`, `restartMcp(name)`, and model classes in a new `mcp.dart`. The store listens for `mcp_changed`.

### 8.3 CLI

```
tern mcp list [--json]
tern mcp show NAME
tern mcp add NAME -- COMMAND [ARGS…] [--env K=V] [--secret NAME] [--cwd DIR]
tern mcp add NAME --url URL [--bearer-secret NAME] [--header-secret H=NAME]
tern mcp import [FILE]          # reads stdin when FILE is omitted
tern mcp test NAME
tern mcp enable|disable|restart|rm NAME
```

`add` and `import` run verify first and prompt for missing secrets (hidden input), unless `--no-test`. They go through the daemon when one is running, otherwise the embedded runtime, like `tern setup`. `claude mcp add`-style argument order (`NAME -- COMMAND ARGS`) is deliberate, so copied commands mostly work with `claude` swapped for `tern`.

`tern doctor`'s MCP section uses verify for enabled servers (with `--quick` skipping it), so it reports "connected, 23 tools" or the real startup error instead of only checking `PATH`.

## 9. Phases

| Phase | Scope | Done when |
|---|---|---|
| **M0 engine** | config fields (§3), live `McpServers::apply`, structured status, stderr ring, `verify`, `import`, config edit functions, `Runtime::reload_mcp`, API (§7), `mcp_changed`, stdio-over-TCP rule, doctor on verify | Tests with the existing `tests/fixtures/mcp_server.py`: add through the API and the next run sees its tools with no restart; editing one server leaves another's process running (same PID); removing kills the child; verify reports a crashing server's stderr; import handles Claude Desktop JSON, placeholders and bearer headers; a TCP device gets 403 for a stdio add |
| **M1 TUI and CLI** | `/mcp` panel and shortcuts, `RemoteBackend` parity, `tern mcp` | From an empty config, `/mcp` → paste a Claude Desktop snippet → enter the token → test → save → the agent calls the tool, with no file edits, embedded and attached. `App` state tests for each panel mode |
| **M2 desktop** | Library → MCP list, detail, add sheet, paste import, tool grouping in the side panel | The same flow in the app against a real daemon; widget tests for the add sheet's states (missing secret, verify failure with stderr, remote stdio disabled) |
| **Later** | OAuth for remote servers, a curated catalog, per-tool approval overrides, MCP resources and prompts, sandboxed stdio servers (run the command in the Docker sandbox), per-agent scoping, the SSE transport, project-local `.mcp.json` | separate specs |

## 10. Decisions

1. **Remote stdio is off by default.** Adding or verifying a stdio server over TCP is refused unless `api.remote_mcp_stdio = true` (§6.1). The reason: shell commands the agent runs go through the Docker sandbox, but a stdio MCP server is spawned directly on the host as the daemon's user, with its files, network and credentials. A paired remote owner device that could add one could run any command on the host, which is more than it can do today. Local use (the TUI, desktop app or CLI on the same machine, over the Unix socket) is unaffected. Sandboxed stdio servers (Later) would make this rule unnecessary.
2. **No SSE transport for now.** Streamable HTTP and stdio only; import rejects `type: "sse"` with a clear message (§4.6).
3. **Saving starts the server.** Every config write starts added or changed servers straight away (§4.1, §5). Daemon boot stays lazy.
4. **No project-local `.mcp.json` for now.** `config.toml` is the only source.
