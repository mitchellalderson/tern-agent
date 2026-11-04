# tern reference

Everything tern does, command by command. For installing and a first chat, see the [README](../README.md); for how the pieces fit together, its [architecture](../README.md#architecture) section.

## What it can do

| | |
|---|---|
| **Models** | Anthropic, plus any OpenAI-compatible endpoint (OpenRouter, Ollama, vLLM, LM Studio). Models are `provider:model`; switch with `--model` or `/model`. |
| **Tools** | Files (`read_file`, `list_dir`, `search_files`, `write_file`, `edit_file`), `shell`, `python` (sandboxed compute that calls your tools), `http_fetch`, long-term memory and capped profile notes (`user.md`, `memory.md`), search over past conversations, skills, schedules. |
| **Approvals** | Every tool has an effect, and tern (not the model) decides from it: `read` never asks, `write` asks unless you chose "always", `destructive` asks every time. Prompts show it, e.g. `write · lua__gmail__send …`. Workspace writes (`~/tern-workspace` by default) and tern's own memory and schedules are pre-granted; reads or writes outside the workspace ask. Shell commands are `destructive` unless an "always" pattern (e.g. `git *`) covers them. Tools flagged `money_movement` or `trade` are refused, and nothing can approve them. A run that has read untrusted content (a web page, mail, a tool marked `untrusted`) is **tainted**: its `write` and `destructive` calls ask whatever "always" says, and the prompt says why ([Taint](#taint)). A write nobody answers in time waits in the [outbox](#outbox) instead of being denied, and `autonomy` sets how much runs may do without asking ([Autonomy](#autonomy)). |
| **MCP** | Ask the agent ("add the GitHub MCP server"; you approve the exact command each time, and only on this machine for local commands), or add, test, toggle and remove servers with `/mcp` in the chat or [`tern mcp`](#tern-mcp), or edit `[mcp.servers.<name>]` (stdio or streamable HTTP); changes apply without a restart, a server's pasted `mcpServers` JSON works too, and their tools appear as `mcp__<name>__<tool>`. Effects come from the server's `readOnlyHint` / `destructiveHint` (else `write`), and `effects = { tool = "read" }` overrides them. For a browser, see [Recipe: give tern a browser](recipes/browser.md). The other way round, [`tern mcp serve`](#tern-as-an-mcp-server) offers your connectors, memory and channels to Claude Desktop, Cursor or Claude Code, under tern's approvals. |
| **Luau connectors** | Small sandboxed scripts in `~/.tern/connectors/*.luau` that declare which hosts and paths they may reach, which credentials tern adds to their requests (the script never sees them), a request rate, and an `effect` per tool (`write` if left out). The agent can write and test its own, and activates one only with your approval of its capabilities. `tern connectors` lists them. |
| **Skills** | Folders in `~/.tern/skills/` with a `SKILL.md` (agentskills format). |
| **Telegram** | Chat from your phone, approve tool calls with buttons, get scheduled results. |
| **Schedules** | "Every weekday at 8am, summarize my unread GitHub notifications" becomes a routine whose results are delivered to the chat that asked: on a cron, once at a time, or on events from a connector's watch (no model call when nothing happened). Runs with nothing to say stay silent, a gate can skip a run, each routine's initiative decides whether its result interrupts you or waits for a digest, results wait out your quiet hours, and each can be capped per day. See [Schedules](#schedules). |
| **Durability** | Kill tern mid-run and `tern resume <run>` (or the daemon, automatically) continues it without re-running completed tools. `tern runs show <run>` prints the step journal. |


## Install

```sh
curl -fsSL https://raw.githubusercontent.com/mitchellalderson/tern-agent/main/install.sh | sh
```

The script picks the right build (Linux x86_64/aarch64, static musl; macOS Apple Silicon/Intel), verifies its SHA-256 checksum, and installs to `~/.local/bin` (`TERN_INSTALL_DIR` to change; `TERN_VERSION=v0.1.0` pins a release, `TERN_REPO=owner/name` installs from a fork). Or build from source with Rust 1.89+:

```sh
cargo build --profile dist -p tern    # → target/dist/tern
```

### Desktop app

macOS 11+ (Apple Silicon and Intel): download [tern-desktop-macos.dmg](https://github.com/mitchellalderson/tern-agent/releases/latest/download/tern-desktop-macos.dmg) and drag tern to Applications. The app carries its own `tern`, so you don't need the CLI. On first launch it asks where tern should run: **On this Mac** (with **Start tern when I log in**, which starts tern and its menu-bar icon at login; both appear in System Settings › General › Login Items), or **On a tern server**: paste the pairing link from `tern devices pair` on that server. You can switch later in Settings. Then add a model. To also use that binary from a terminal:

```sh
ln -s "/Applications/tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray" ~/.local/bin/tern
```

Until the app is notarized, macOS blocks the first open: open it once, then allow it under **System Settings → Privacy & Security → Open Anyway**.

### Verify a download

Every release asset has a `.sha256` next to it and a GitHub build provenance attestation:

```sh
gh attestation verify tern-desktop-macos.dmg --repo mitchellalderson/tern-agent
```

## Commands

| Command | What it does |
|---|---|
| [`tern`](#tern) | Interactive chat in the terminal. |
| [`tern run`](#tern-run) | Run one prompt non-interactively and print the answer. |
| [`tern resume`](#tern-resume) | Continue an interrupted or failed run. |
| [`tern daemon`](#tern-daemon) | Run the scheduler, gateways, and local API in the foreground. |
| [`tern start` / `tern stop`](#tern-start-and-tern-stop) | Start tern in the background, or stop it. |
| [`tern service`](#tern-service) | Start tern at login (launchd on macOS, systemd on Linux). |
| [`tern tray`](#tern-tray) | The menu-bar icon (macOS): is tern running, Open, Start/Quit, Start at login. |
| [`tern gateway pair`](#tern-gateway-pair) | Pair a Telegram account, or a sender on a gateway connector. |
| [`tern gateway add`](#gateway-connectors-ntfy-slack) | Install the ntfy or Slack gateway and enable it. |
| [`tern gateway list`](#gateway-connectors-ntfy-slack) | Gateways in `config.toml` and whether each can run. |
| [`tern agents`](#tern-agents) | List, create, edit or archive your agents. |
| [`tern sessions`](#tern-sessions) | List or show conversations. |
| [`tern runs`](#tern-runs) | List runs or print a run's step journal. |
| [`tern grants`](#new-sites-in-a-tainted-run) | The "always" grants approvals have recorded (commands, tools, sites), and removing them. |
| [`tern outbox`](#outbox) | Writes waiting for your approval: list, show, approve (optionally edited), reject. |
| [`tern hooks`](#webhooks) | Webhook routines: create (URL and secret shown once), list, rotate, revoke, fire locally. |
| [`tern connectors`](#tern-connectors) | List or validate Luau connectors, replay or record their test fixtures, or generate one from an OpenAPI document. |
| [`tern connections`](#tern-connections) | Accounts of connectors and MCP servers: sign in, add another, set the default, remove. |
| [`tern setup`](#tern-setup) | Connect a model provider: paste a key, pick a model. |
| [`tern providers`](#tern-providers) | List, add, or remove model providers (for scripts). |
| [`tern mcp`](#tern-mcp) | List, add, import, test and manage MCP servers. |
| [`tern models`](#tern-providers) | List the models each provider offers. |
| [`tern config`](#tern-config) | Create, read, and edit `config.toml`. |
| [`tern doctor`](#tern-doctor) | Check configuration, providers, database, daemon, and integrations. |
| [`tern devices`](#remote-access) | Pair and manage devices that reach this tern over TCP. |
| [`tern report`](#tern-report) | Write a redacted bug report. |
| [`tern reset`](#tern-reset) | Delete all local data, stopping tern first. |

`tern help <command>` or `tern <command> --help` prints each command's options; `tern --version` prints the version. Commands exit non-zero on failure and print `error: …` on stderr.

### `tern`

With no subcommand, tern opens the chat TUI. If a daemon is running it attaches to it over `~/.tern/tern.sock`, so the conversation, schedules, and approvals are shared with Telegram; otherwise it starts its own runtime in-process.

Chat commands: `/new` · `/model [model]` · `/setup` · `/sessions` · `/resume <session or run id>` · `/tools` · `/secrets [NAME]` · `/mcp` · `/stop` · `/reload` · `/usage` · `/help`.

`/setup` (or `/providers`) opens a panel for model providers: add one (Anthropic, or any OpenAI-compatible server by base URL), change its API key, choose its default model, or remove it. Keys are checked with the provider before they're saved, shown only masked (`sk-ant…a1B2`), and never reach the transcript, input history, or model. Ctrl+E uses a key already set in the environment. Changes apply to the next message, with no restart, including when attached to the daemon.

`/model` with no argument opens a picker of every provider's models (fetched from the providers and cached): type to filter, Enter to use one for this conversation, Ctrl+D to also make it the default, Ctrl+R to refresh. A model the list doesn't have can still be typed and used. `/model <id>` sets one directly; the provider can be left off when only one provider offers that model.

`/secrets` opens a panel listing every secret tern knows about: those in `secrets.toml` and those that config or an active connector refers to, with where each is set (file, environment) and what uses it. `n` adds one, Enter or `e` sets the selected one's value, `d` removes it from `secrets.toml` (confirm with `y`), and Esc closes the panel. `/secrets NAME` goes straight to entering NAME's value. Values are shown only as a character count and never reach the transcript, input history, or model.

Keys: Enter sends; Shift+Enter, Alt+Enter, or Ctrl+J adds a newline; Ctrl+O expands tool output; Ctrl+C cancels the current run (twice to quit).

### `tern run`

```sh
tern run "<prompt>" [--model provider:model] [--session <id>] [--yes]
```

Runs one prompt to completion and prints the final answer on stdout. Progress (tool calls, results, retries) goes to stderr, so `tern run "..." > answer.txt` captures only the answer.

| Option | |
|---|---|
| `--model provider:model` | Use this model instead of `default_model`. |
| `--session <id>` | Continue an existing conversation (ids from `tern sessions`) instead of starting a new one. |
| `--yes` | Approve every tool call without asking. For scripts and tests; use with care. |

Without `--yes`, tool calls that need approval are asked about on the terminal (`[y]es / [a]lways / [N]o`). When stdin or stderr isn't a terminal they are denied.

Exit codes: `0` completed; `130` interrupted (Ctrl+C: the first cancels the run, the second quits immediately); `1` failed. In both of the last two cases tern prints the `tern resume <run>` command that continues it.

### `tern resume`

```sh
tern resume <run-id> [--yes]
```

Continues a run that was interrupted, failed, or killed. Model calls and tool results already in the journal are replayed instead of re-executed (shown as `(replayed)`), so completed side effects don't happen twice. Output and exit codes are the same as `tern run`. Run ids are printed by `tern run` and listed by `tern runs list`.

You rarely need this while the daemon runs: on start it resumes recent runs that were cut off and marks older ones interrupted.

### `tern daemon`

```sh
tern daemon
```

Runs in the foreground until SIGINT or SIGTERM, and provides:

- **Scheduler**: checks for due schedules every 30 seconds, polls routines' watches, and starts runs for new watch events. A schedule missed while the daemon was down runs once on the next start, then continues from now (no backfill). It also enforces each schedule's caps and sends results held for quiet hours (see [Schedules](#schedules)).
- **Gateways**: Telegram, if `[gateway.telegram]` is enabled (see [Telegram](#telegram)). The daemon refuses to start if the bot token secret isn't set. And each enabled [gateway connector](#gateway-connectors-ntfy-slack); one that can't load is skipped with a warning in the log.
- **Local API** on `~/.tern/tern.sock` (mode 0600), which `tern` and the desktop app attach to.
- **Crash recovery**: runs in progress when the daemon stops are left `running` and resume on the next start, exactly as after a crash.

Only one daemon may use a data directory; a second one exits with the first one's pid. `tern doctor` reports whether it's running and what manages it. To keep it running, use [`tern start`](#tern-start-and-tern-stop) or [`tern service install`](#tern-service); logs go to `~/.tern/logs/`.

### `tern start` and `tern stop`

```sh
tern start    # in the background: through the service if installed, else detached
tern stop     # stop it; tasks in progress pick up where they left off next start
```

`tern start` starts tern through the login service when [`tern service`](#tern-service) installed one, and otherwise runs `tern daemon` detached (output in `~/.tern/logs/daemon.log`). It waits until tern answers, and does nothing if it's already running.

`tern stop` asks tern to stop (`POST /v1/shutdown`). tern exits cleanly with code 0, so the service manager doesn't start it again, and runs in progress stay paused until the next start. If tern hasn't gone after 10 seconds (it's stuck, or an older version without the endpoint), `tern stop` stops it by force: it has the service manager let go of it first (`launchctl bootout` or `systemctl --user stop`, so it isn't brought back; the login item stays), then sends SIGTERM to the pid in `~/.tern/tern.pid`, and SIGKILL 5 seconds later. It only signals a `tern` process owned by you.

The desktop app does the same from **tern › Quit tern completely** (⌥⌘Q), so nobody needs a terminal to turn tern off.

### `tern service`

```sh
tern service install [--no-login] [--tray]   # start tern (and the menu-bar icon) at login
tern service status [--json]                 # installed? how? loaded? running?
tern service uninstall                       # stop starting them at login
```

- **macOS, from the app** (the `tern` inside `tern.app`): registers with macOS through `SMAppService`, so tern and its menu-bar icon show in System Settings › General › Login Items, where you can also turn them off. macOS may ask you to allow them there first.
- **macOS, a CLI install**: writes the LaunchAgent `~/Library/LaunchAgents/ai.tern.daemon.plist` (and `ai.tern.tray.plist` with `--tray`) and loads it. Either way the daemon has `KeepAlive = { SuccessfulExit = false }`: launchd restarts tern after a crash, but not after `tern stop` or Quit. `tern doctor` says which method is in use.
- **Linux**: writes the systemd user unit `~/.config/systemd/user/tern-daemon.service` with `Restart=on-failure`, for the same reason, and enables it.

Logs go to `~/.tern/logs/daemon.{out,err}.log`. `--no-login` installs the service without starting tern at login (`tern start` then starts it through the service). Installing or uninstalling never stops a running tern; `tern stop` does. The desktop app's **Settings › Start tern when I log in** runs these commands.

### `tern tray`

A menu-bar icon for tern on macOS, in its own small process: ● running (with a ring while a task runs, and a dot when something needs you), ◐ starting, stopping or not answering, ○ stopped. Its menu shows what tern is doing and has **Open tern**, **Start at login** and **Quit tern**, which stops tern the way `tern stop` does (asking first if a task is in progress), closes the app and removes the icon. When tern is stopped it offers **Start tern** and **Close this icon**. The tray only watches and asks; launchd keeps tern itself running. `tern service install --tray` starts it at login; inside `tern.app` it's the `tern Tray.app` login item. Builds without the `tray` cargo feature (on by default; its dependencies are macOS-only) don't have it.

### `tern gateway pair`

```sh
tern gateway pair
```

Prints an 8-character one-time code on stdout. Send it to your Telegram bot within 10 minutes to pair that Telegram account, or send it as your first message on a [gateway connector](#gateway-connectors-ntfy-slack) to pair yourself there. Only paired accounts can talk to the bot or answer its approval buttons. Works whether or not the daemon is running, but the bot only answers while the daemon runs.

### `tern agents`

```sh
tern agents [list] [--all]                    # each agent: @name, name, memory, model, routines (--all: archived too)
tern agents new <name> [--slug s] [--template research|inbox|marketing|blank] [--memory private|shared]
tern agents edit <name>                       # open its persona.md in $VISUAL / $EDITOR
tern agents archive <name> [--undo]           # hide it and pause its routines; nothing is deleted
```

Works without the daemon; a running one picks changes up at the agent's next run. See [Agents](#agents).

### `tern sessions`

```sh
tern sessions [list]      # the 50 most recently updated: id, time, source, message count, title
tern sessions show <id>   # one conversation's messages
```

A session is a conversation; its source is where it started (`cli`, `tui`, `api` for the daemon's socket, `telegram`, `cron`).

### `tern runs`

```sh
tern runs list            # the 20 most recent runs: id, status, start time, model, "tainted"
tern runs show <run-id>   # run details and its step journal
```

A run is one prompt driven to a final answer within a session. Its journal records each model call and tool result; `runs show` is the quickest way to see what a run did, and where an unfinished one stopped. A tainted run shows `tainted  <why>` in its details, untrusted tool results are marked `[untrusted]`, and mail from a tainted run `[tainted]`. Steps a [fallback model](#fallback-chains) answered say so.

### `tern connectors`

```sh
tern connectors [list]     # active connectors and drafts, with capabilities and missing secrets
tern connectors test <name> [--record [--tool T --args JSON]]
tern connectors from-openapi <url|file> [--list [--tag T] [--search S] [--offset N]] [--ops a,b,tag:x] [--name N] [--base-url U]
```

`list` loads every `.luau` file in `~/.tern/connectors/` (active) and `~/.tern/connectors/drafts/` (written by the agent, not yet activated) and prints each one's description, declared hosts and paths, credentials, rate and secrets, its tools with their effect (`- lua__mail__search [read]: … (untrusted)`), and any secrets that aren't set. Files that fail to load are marked `✗` with the error. Active connectors that keep the pre-effects approval default are marked `grandfathered`, and those that read secret values themselves with `ctx.secret` are marked `legacy`.

`test <name>` validates `<name>.luau` (active first, then drafts) and lists the tools it exposes, with their effect and flags (untrusted, read via POST, idempotent, requires approval, refused). Then it replays the connector's **recorded fixtures** offline: every recorded call runs again against the saved responses, with no network and no secrets, and must return what it returned when recorded. It exits non-zero if the connector doesn't exist or doesn't load, or a recorded call fails or returns something else, so it can run in CI. `list` writes nothing.

**Fixtures** live beside the connector as `<name>.fixtures.json` (`~/.tern/connectors/drafts/` for a draft; activating moves them to `~/.tern/connectors/`), so they travel with it. `test <name> --record` calls the live API and saves the calls: by default the read calls already recorded, or every read tool that needs no arguments; `--tool T --args '{…}'` records one call of any tool (a write really happens). The agent records as it works: `connector_test`'s first live call of a tool with given arguments is saved, and testing the same arguments again replays it offline without asking (`mode: "live"` calls the API again, `mode: "replay"` never does). Nothing secret is saved: requests are recorded before tern adds credentials, every known secret value is replaced with `[REDACTED]`, request headers with credential-like names and all response headers except content type, paging, rate-limit and caching ones are dropped, and the string values of JSON fields and query parameters named like credentials (`access_token`, `api_key`, `password`, `client_secret`, …, but not paging tokens such as `next_page_token`) are redacted. Responses are cut at 256 KB. Look over a fixtures file before sharing it anyway: an API can return personal data that isn't a credential. `connectors.record_tests = false` turns recording off.

`from-openapi` generates a draft from an **OpenAPI 3.x or Swagger 2.0** document (JSON or YAML, from a URL fetched like `http_fetch` or a local file): one tool per operation, named from its `operationId` (or method and path), with its parameters and JSON or form body as typed params. GET and HEAD are `read` (marked `untrusted`), DELETE is `destructive`, the rest `write`. `capabilities.http` is the server's host with each operation's path (`{id}` segments as `*`; more than 40 collapse to the base path), `auth` comes from the security scheme the chosen operations use (an API key in a header or query, a bearer token, HTTP Basic, each as a secret named after the connector such as `PETSTORE_API_KEY`; OAuth 2 and OpenID Connect as `capabilities.oauth` with the scopes those operations need, a built-in profile's provider when the API is one tern knows), and `rate` comes from an `x-rate-limit`-style extension or "N requests per minute" in the description. Without `--ops` it takes the first `connectors.openapi_tools` (8) non-deprecated reads; a document with more than `connectors.openapi_max_tools` (40) operations is listed instead and nothing is written, so a 500-operation API never becomes 500 tools: pick with `--ops` (tool names, operationIds, `"GET /path"`, or `tag:<tag>`). `--list` pages through the operations with their tags. Operations that need a file upload or a cookie can't be generated. The draft is not active: ask tern to activate it, and you approve its capabilities like any other. The agent does the same with `connector_from_openapi`, and prefers it to writing a connector by hand when a service publishes such a document.

A connector tool declares what it does:

```lua
search = { effect = "read", untrusted = true, ... },   -- never asks; returns third-party text
send   = { effect = "write", ... },                    -- the default: asks unless you chose "always"
purge  = { effect = "destructive", ... },              -- asks every time
query  = { effect = "read", read_via_post = true, ... }, -- a read that POSTs (GraphQL, search APIs)
pay    = { money_movement = true, ... },               -- refused; also `trade = true`
```

A `read` tool can only send GET and HEAD unless it sets `read_via_post`; anything else fails before the request goes out. Connectors that were already active when tern gained effects keep their old behaviour (tools without an `effect` don't ask) until you activate them again or edit them.

And what it may reach, with which credentials:

```lua
capabilities = {
  http = { "api.github.com/repos/*", "api.github.com/search/*" },  -- hosts, optionally with paths
  auth = { secret = "GITHUB_TOKEN", scheme = "token" },           -- tern adds "Authorization: token …"
  rate = "60/min",                                                 -- requests per sec, min or hour
}
```

- **`auth`**: tern adds the credential to each request to the connector's hosts; the script never sees the value, and it is replaced with `[REDACTED]` in anything a server sends back (body, headers) and in errors. Forms: `auth = "NAME"` (Bearer), `{ secret = "NAME", header = "X-Api-Key" }`, `{ secret = "NAME", header = "Authorization", scheme = "token" }`, `{ secret = "NAME", query = "api_key" }`, `{ basic = { user = "U", password = "P" } }` (both secret names). `hosts = { … }` sends an entry to only some of the declared hosts, and a list of entries declares several. A request can pass `auth = false` to send none, or `auth = "NAME"` for just one.
- **Paths**: `host/path` entries allow only matching paths on that host; `*` is one path segment and a trailing `*` is the rest. A host listed without a path allows every path. Every request and every redirect is checked; a path-restricted host also refuses encoded `/`, `\` and `.` (`%2f`, `%5c`, `%2e`).
- **Rate**: each connector has one budget, shared by every run, routine, sub-agent and `python` script that calls it; past it a request fails at once with `rate limited … try again in Ns` and nothing is sent. Without `rate`, `connectors.rate` applies. A server's `429` or `503` with `Retry-After: <seconds>` pauses the connector that long (at most an hour).
- **Legacy `ctx.secret`**: `capabilities.secrets = { "NAME" }` with `ctx.secret("NAME")` still works for older connectors, but the script then holds the value and could send it to any host it may reach. The activation prompt, `tern connectors list`, `tern doctor` and the Secrets view (`reads the value itself (legacy ctx.secret)`) say so.

Activating a connector shows all of this, and replacing an active one lists what changed (`+ http api.github.com/search/*, - rate 60/min`). Changing any of it afterwards stops the connector from loading until you approve it again.

### `tern connections`

```sh
tern connections [list]                                    # every account, its sign-in method, default and status
tern connections add <name> [--connection work] [--label L] [--key] [--device] [--kind connector|mcp]
tern connections reconnect <name> <connection> [--device]  # sign in again after a revoked or expired grant
tern connections default <name> <connection>               # the account reads use when none is named
tern connections rename <name> <connection> <label>
tern connections remove <name> <connection>                # revoke at the provider, delete its tokens, keys, cache and grants
```

A **connection** is one account of a connector or MCP server: *gmail · work (me@inngest.com)* and *gmail · personal (me@gmail.com)* are two connections of one `gmail` connector. Each is signed in and stored separately; tokens and keys live in `secrets.toml` under `conn/<kind>:<name>/<connection>/` (`conn/connector:gmail/work/`, `conn/mcp:linear/work/`) and never reach the agent or a script. Until you add a second account, every connector and MCP server has one implicit `default` connection that uses its plain secret names, as before.

`add` signs in with OAuth when tern knows how (a built-in profile, the server's own metadata, or an earlier account's settings): it opens your browser on the service's own sign-in page and catches the answer on a one-shot `127.0.0.1` port. `--device` prints a code to enter on any device instead, for a tern running elsewhere. `--key` asks for the connector's or server's declared secrets (hidden) instead. The connection name defaults to `default` for the first account and to a name made from the account's email after that (`me@inngest.com` → `inngest`, `me@gmail.com` → `me`). Signing in to an account that is already connected refreshes it instead of adding a duplicate. At most `[auth] max_connections` (10) per connector or server.

With two or more accounts, every tool of that connector or server takes a `connection` argument naming one. Reads without it use the default; writes and deletes without it are refused with the list of accounts, so the agent has to choose, and the approval card names the account (`lua__gmail__send as gmail · personal (me@gmail.com)`). An account whose sign-in was revoked or expired shows `needs reauth` and refuses calls until you reconnect it.

In the TUI, `/connections` lists the accounts. The desktop app has a Connections view.

**Connector code** declares OAuth with `capabilities.oauth = { google = { scopes = { "https://www.googleapis.com/auth/gmail.readonly" } } }` (one provider: a built-in profile, `google`, `microsoft`, `github`, `slack`, `notion`, `linear`, `atlassian`, `dropbox`, or a provider the agent discovered, named by its whole domain like `acme_com`). tern sends the account's access token as `Authorization: Bearer …` to every declared host (or where `auth = "oauth"` entries say), refreshes it when it expires or the service answers 401, and the script sees only `ctx.connection` (`{ slug, label, account }`). OAuth apps you registered yourself go in `[auth.clients]` (see [Configuration](#configuration)).

### `tern setup`

Opens the chat TUI on the `/setup` panel (see [`tern`](#tern)): choose a provider, paste its API key, pick a model. Uses the running daemon when there is one, so it picks up the change at once.

### `tern providers`

```sh
tern providers [list]                                  # providers, key source (masked), default
tern providers add anthropic [--default-model M]       # key from stdin, or asked for hidden
tern providers add openai-compat --url URL [--name N] [--default-model M] [--no-key | --env-key]
tern providers rm NAME [--keep-key]
tern models [PROVIDER] [--refresh] [--json]
```

`add` checks the key with the provider before saving anything and exits non-zero if it's rejected. The key goes to `secrets.toml` as `<NAME>_API_KEY`; `--env-key` uses one already set in the environment instead. Without `--default-model`, the first listed model becomes the default when there's no usable default yet. `rm` also deletes the provider's key unless `--keep-key` or something else uses it. Like `tern setup`, these go through the running daemon when there is one.

### `tern mcp`

```
tern mcp [list] [--json]                               # servers and their state
tern mcp show NAME                                     # config, tools, recent stderr
tern mcp add NAME [--env K=V] [--secret NAME] [--cwd DIR] -- COMMAND [ARGS…]
tern mcp add NAME --url URL [--bearer-secret NAME] [--header-secret H=NAME]
tern mcp import [FILE]                                 # mcpServers JSON, from stdin without FILE
tern mcp test NAME
tern mcp enable|disable|restart|rm NAME
tern mcp serve [--agent SLUG]                          # tern as an MCP server over stdio
tern mcp serve --print-config [--agent SLUG]           # the mcpServers snippet for a client
```

`add` and `import` start the server and list its tools before saving (`--no-test` skips that), and ask for missing secrets without echoing them. Values in a pasted snippet that look like keys are stored in `secrets.toml`, never `config.toml`. In the chat, `/mcp` opens the same as a panel (`/mcp add`, `/mcp paste`, `/mcp restart|enable|disable NAME`).

To browse the web, add a browser MCP server; [Recipe: give tern a browser](recipes/browser.md) has the commands, effects and cautions.

#### tern as an MCP server

`tern mcp serve` lets other AI clients (Claude Desktop, Cursor, Claude Code) use your Luau connectors, memory, channels and routines through tern. Add the snippet `tern mcp serve --print-config` prints to the client's `mcpServers` (Claude Desktop: `claude_desktop_config.json`; Cursor: `~/.cursor/mcp.json`; Claude Code: `claude mcp add tern -- tern mcp serve`). The client launches it over stdio; it talks to the running daemon (starting it if needed) over the local socket. `tern doctor` says which clients have it configured.

- **What a client sees**: with no `[mcp.serve] agent`, only `memory_search`, `channel_read`, `schedule_list` and `skill_read`. With `agent = "<slug>"` (or `--agent`), that agent's tool scope, limited to its Luau connector tools (`lua__<name>__<tool>`), `memory_search`, `memory_save`, `channel_read`, `channel_post`, `schedule_*` and `skill_read`. Tools that read files, run commands or Python, fetch URLs, ask for secrets, sign in or manage MCP servers are never offered, and a tool outside the list can't be called by name.
- **Policy holds**: each call is a run with trigger `mcp` in one session per client (source `mcp`), journaled and visible in tern like any run. It **starts tainted** ("a call from an MCP client"): every `write` and `destructive` call asks, no "always" grant covers it, and "always" isn't offered. Approvals appear in tern (the app, the TUI), not in the client; with nobody attached the call goes to the [outbox](#outbox) and the client is told it is queued. A denial reaches the client as a tool error. `[mcp.serve] autonomy` can lower `autonomy` for MCP calls (`readonly` makes the server read-only) but not raise it.
- **Results**: an untrusted tool's text reaches the client inside `<untrusted source="…">`; every known secret value is redacted from results and tool descriptions; posts a client makes are tainted posts.
- **API**: `GET /v1/mcp-serve/tools[?agent=]` and `POST /v1/mcp-serve/call` `{name, arguments, session_id?, client?, agent?}` (owner scope over TCP; the stdio server uses the local socket). Streamable HTTP isn't offered yet.

### `tern config`

```sh
tern config init [--force]    # create ~/.tern, a commented config.toml, persona.md, user.md, and the workspace
tern config get [key]         # print a value by dotted key, or the whole effective config
tern config set <key> <value> # set a value by dotted key
```

`init` never overwrites `persona.md` or `user.md`, and overwrites `config.toml` only with `--force`.

`get` prints the effective config, defaults included, so `tern config get sandbox` shows every sandbox setting even if `config.toml` doesn't mention them.

`set` parses the value as TOML (`40`, `true`, `["a", "b"]`) and stores it as a string otherwise; missing tables are created. It keeps the file's comments and formatting, and refuses to write a config that wouldn't load. Secrets don't belong in `config.toml`: config refers to them by name (`api_key_secret`, `bot_token_secret`, `env_secrets`).

```sh
tern config set max_iterations 60
tern config set sandbox.backend docker
tern config set mcp.servers.github.transport stdio   # a new server needs its transport first
tern config set mcp.servers.github.command github-mcp-server
tern config set mcp.servers.github.args '["stdio"]'
```

### `tern doctor`

```sh
tern doctor [--offline]
```

Checks, in order: the data directory; `config.toml` and `secrets.toml` (including its 0600 permissions); each provider (that its secret is set and, unless `--offline`, that its endpoint answers with that key) and `default_model`; every model in each fallback chain (configured provider, and listed by it when online); the database's schema version and integrity; whether the daemon is running and what manages it (launchd, systemd, or nothing); schedules paused by a daily cap or whose USD cap can't apply (no known price), and deliveries held for quiet hours; `user.md` and `memory.md` against their caps (over a cap is a warning: only the first part is in the prompt); the sandbox backend (whether the OS sandbox is available for `host`, Docker on `PATH` for `docker`); the Telegram gateway and each gateway connector (that it loads and can send, its secrets, and whether anyone has paired); the [webhook](#webhooks) listener and whether senders can reach it; each active Luau connector (a warning for one that reads secret values itself with `ctx.secret`); and each enabled MCP server: its secrets, and that it starts and lists its tools (with `--offline`, only that its command is on `PATH`). Each problem comes with a hint for fixing it.

It exits `1` if anything failed, `0` otherwise (warnings are things not set up yet). It is read-only and doesn't create `~/.tern`.

### `tern report`

```sh
tern report [-o <path> | -o -] [--run <run-id>]
```

See [Troubleshooting](#troubleshooting).

### `tern reset`

```sh
tern reset [--keep-config] [-y]
```

Stops tern, then deletes everything in `~/.tern`: conversations, runs, memory, skills, connectors, logs, config and secrets. `--keep-config` keeps `config.toml` and `secrets.toml` (your providers and keys). It asks first unless `-y`.

## Configuration

`~/.tern/config.toml` (written by `tern config init`):

| Key | Default | |
|---|---|---|
| `default_model` | none | `provider:model` used when `--model` / `/model` isn't given, or a [fallback chain](#fallback-chains) `["anthropic:…", "openrouter:…"]`. |
| `workspace` | `~/tern-workspace` | Where file tools work without approval. |
| `max_iterations` | `40` | Model calls per run before it stops. |
| `context_window` | `128000` | Tokens; older messages are summarized past 75%. |
| `providers.<name>.kind` | | `anthropic` or `openai_compat`. |
| `providers.<name>.base_url` | | Required for `openai_compat`; defaults to `https://api.anthropic.com` for `anthropic`. |
| `providers.<name>.api_key_secret` | | Name of the secret holding the key (omit for keyless local servers). |
| `providers.<name>.context_window` | | Overrides `context_window` for this provider. |
| `providers.<name>.prices` | `{}` | USD per million tokens per model, for `usd_per_day`: `{ "claude-sonnet-4-5" = { input = 3.0, output = 15.0, cache_read = 0.3, cache_write = 3.75 } }` (cache prices default to `input`). Wins over prices the model list gives (OpenRouter's does). |
| `schedules.quiet_hours` | `"22:00-07:30"` | Local time; a scheduled result finishing inside waits until the window ends. `"none"` turns it off. |
| `schedules.runs_per_day` | none | Default cap on runs per schedule per local day. |
| `schedules.usd_per_day` | none | Default cap on a schedule's spend per local day (sub-agents included), where prices are known. |
| `schedules.tokens_per_run` | none | Default cap on one scheduled run's tokens (input + output + cache). |
| `schedules.initiative` | `"digest"` | How a routine's result reaches its chat unless the routine sets its own: `silent`, `digest`, `notify` or `interrupt`. See [Routines](#routines-triggers-watches-gates-initiative). |
| `schedules.initiative_dial` | `"normal"` | Every routine one level `quieter` or `louder`, or `vacation` (nothing pushed). Also `PUT /v1/initiative`. |
| `schedules.digest_times` | `["08:30", "17:30"]` | Local times when digest results are sent, one message per chat. |
| `schedules.gate_model` | none | The model that decides whether a `notify` result is worth a push; unset = the cheapest configured model with a known price. |
| `schedules.max_events_per_poll` | `20` | New watch events that each start a run; the rest of one poll share one run. |
| `memory.user_md_tokens` | `1500` | Cap on `user.md` as injected into the prompt (tokens ≈ chars / 4). |
| `memory.memory_md_tokens` | `1500` | Cap on `memory.md` (core facts) as injected. |
| `memory.flush` | `true` | Before older context is summarized, give the agent one silent turn to save durable facts with `memory_save`. |
| `sandbox.backend` | `host` | `host` or `docker`. |
| `sandbox.shell_timeout_secs` | `120` | Per shell command. |
| `sandbox.network` | `false` | IP networking for every shell command (both backends). Off, a command gets the network only when called with `network: true`, which asks every time. While a command can reach the network (it asked, `network = true`, or no OS sandbox enforces off), its output is untrusted and taints the run ([Taint](#taint)); with connectors or MCP servers active and the network reachable for every command, `tern doctor` says so. |
| `sandbox.confine` | `auto` | Host backend: `auto` (sandbox when the OS can, else warn), `required` (refuse to run unconfined), `off`. |
| `sandbox.read_paths` | `["~/.gitconfig", "~/.config/git"]` | Host backend: extra read-only paths. |
| `sandbox.write_paths` | `[]` | Host backend: extra writable paths. |
| `sandbox.docker_image` | `debian:stable-slim` | Image for the `docker` backend. |
| `sandbox.docker_memory` / `docker_cpus` / `docker_pids` | `2g` / `2.0` / `512` | Container limits; empty or 0 for none. |
| `gateway.telegram.enabled` | `false` | |
| `gateway.telegram.bot_token_secret` | | Name of the secret holding the bot token. |
| `gateway.connectors.<name>.enabled` | `true` | Run active connector `<name>` as a gateway. |
| `gateway.connectors.<name>.poll_secs` | `30` | How often to check it for messages. |
| `gateway.connectors.<name>.strangers` | `"reply"` | Unpaired senders: `reply` (a fixed note), `ignore`, or `taint` (a tainted run of their own). |
| `gateway.connectors.<name>.settings` | `{}` | Handed to the connector: topics, channel ids. Never secret values. |
| `mcp.servers.<name>.transport` | | `stdio` or `http` (streamable HTTP). |
| `mcp.servers.<name>.command` / `args` | | Executable and arguments, for `stdio`. |
| `mcp.servers.<name>.enabled` | `true` | `false` keeps the server configured but doesn't start it or offer its tools. |
| `mcp.servers.<name>.env` / `cwd` | `{}` / none | Plain (non-secret) environment and working directory, for `stdio`. Values that look like keys are refused on write; use `env_secrets`. |
| `mcp.servers.<name>.url` | | Endpoint, for `http`. Plain `http` with a token only to loopback or private addresses. |
| `mcp.servers.<name>.env_secrets` | `[]` | Secrets passed to the server's environment. |
| `mcp.servers.<name>.bearer_token_secret` | none | For `http`: a secret sent as `Authorization: Bearer …`. |
| `mcp.servers.<name>.header_secrets` | `{}` | For `http`: header → secret, e.g. `{ X-API-Key = "FOO_KEY" }`. |
| `mcp.servers.<name>.disabled_tools` | `[]` | The server's own tool names to hide from the model. |
| `mcp.servers.<name>.idempotent` | `false` | Its tools are safe to re-run when resuming. |
| `mcp.servers.<name>.requires_approval` | `false` | Ask before each of its tool calls. |
| `mcp.servers.<name>.untrusted` | `false` | Its tools return third-party text (a browser, mail, chat): their results taint the run ([Taint](#taint)). |
| `mcp.servers.<name>.effects` | `{}` | Effect per tool, by the server's tool name (`"*"` for the rest): `{ search_issues = "read", delete_repo = "destructive" }`. Overrides the server's `readOnlyHint` / `destructiveHint`; tools without either are `write`. |
| `mcp.serve.agent` | none | The agent profile (slug) whose tool scope [`tern mcp serve`](#tern-as-an-mcp-server) offers MCP clients. Unset: `memory_search`, `channel_read`, `schedule_list` and `skill_read` only. |
| `mcp.serve.autonomy` | none | Caps `autonomy` for MCP calls (never raises it); `readonly` makes the server read-only. |
| `subagents.default_model` | none | Sub-agents' model when `agent_spawn` names none (unset = the parent's). May be a fallback chain. |
| `subagents.models` | `{}` | Aliases `agent_spawn` can pass as `model`: `{ fast = "anthropic:claude-haiku-4-5" }`. Each may be a fallback chain. |
| `secret_request_timeout_secs` | `900` | How long the agent's prompt for a secret waits for you. |
| `autonomy` | `"supervised"` | `readonly`, `supervised`, `autonomous` or `yolo` ([Autonomy](#autonomy)). Routines can set their own. |
| `outbox.wait_secs` | `120` | How long a `write`/`destructive` approval waits for an answer before it goes to the [outbox](#outbox) (at once when no client is attached). |
| `outbox.expire_hours` | `72` | How long an outbox item waits for you. |
| `taint.egress` | `"ask"` | In a tainted run, a read from a site it hasn't reached asks first (`ask`) or doesn't (`allow`). An agent can set its own ([New sites in a tainted run](#new-sites-in-a-tainted-run)). |
| `connectors.rate` | `"120/min"` | HTTP requests per Luau connector for those that declare no `rate`, shared by every caller ([`tern connectors`](#tern-connectors)). Read at start. |
| `connectors.record_tests` | `true` | `connector_test` saves a live call as a fixture beside the draft (no credentials) and replays it offline when the same call is tested again. Read at start. |
| `connectors.openapi_tools` | `8` | Operations `connector_from_openapi` and `tern connectors from-openapi` generate when none are named (reads first). Read at start. |
| `connectors.openapi_max_tools` | `40` | Most operations one generated connector may have; a larger document is listed for choosing instead of generated. Read at start. |
| `auth.clients` | `{}` | OAuth apps you registered, by provider: `google = { client_id = "…", client_secret_secret = "GOOGLE_CLIENT_SECRET" }` (the secret's name, not its value). Needed for built-in profiles (Google wants a "Desktop app" client); servers that register clients themselves need none. An app is only sent to its provider's endpoints: a built-in profile's, or the origin in its `issuer = "https://…"`, which any other provider needs (discovered providers are named by their whole domain, `notion_com`). |
| `auth.local_issuers` | `[]` | OAuth servers allowed on private or loopback addresses (`"http://127.0.0.1:8080"`), for a self-hosted identity provider. Every other token, metadata or registration endpoint must be public https. |
| `auth.sign_in_timeout_secs` | `300` | How long a sign-in waits for the browser or a device code. |
| `auth.max_connections` | `10` | Accounts per connector or MCP server ([`tern connections`](#tern-connections)). |
| `auth.idle_timeout_secs` | `600` | An extra account's stdio MCP server stops after this long unused (the default account's stays up). |
| `auth.client_metadata_url` | `""` | An https URL hosting a client ID metadata document, used as the client ID by servers that accept one instead of registration. |

The desktop app shows all of this (except providers, MCP servers and the Telegram gateway, which have their own windows) under **tern › Configuration…** or **Settings › All settings (config.toml)…**: every setting by section with its description from `config.example.toml`, a mark on what you've changed, a reset to the default, search, and a note on settings that matter for safety (`sandbox.network`, `sandbox.confine`, the API listeners). Changes are saved as you make them, keeping the file's comments. `default_model`, `schedules.*`, `home.*` and `api.remote_mcp_stdio` apply from the next run; the rest when tern restarts, which the window offers to do. The same is on the API, owner scope only:

| Method | Path | |
|---|---|---|
| `GET` | `/v1/config/settings` | `{path, error?, sections: [{id, title, description?, settings}]}`; each setting is `{key, kind, choices?, min?, max?, optional, value, default, set, description, applies, restart_pending, security?, risky, editable}`. |
| `PUT` | `/v1/config/settings/{key}` | Set `{"value": …}` (JSON; `null` unsets an optional setting). `400` for a wrong type or range or a config that wouldn't load, `404` for an unknown key. |
| `DELETE` | `/v1/config/settings/{key}` | Back to the default: the key is removed from the file. |

```sh
curl --unix-socket ~/.tern/tern.sock -X PUT http://tern/v1/config/settings/sandbox.shell_timeout_secs \
  -H 'content-type: application/json' -d '{"value": 300}'
```

Secrets are looked up by name in the environment first, then in `~/.tern/secrets.toml` (mode 0600):

```toml
ANTHROPIC_API_KEY = "sk-ant-..."
TELEGRAM_BOT_TOKEN = "123456:..."
```

Rather than editing the file by hand, use `/secrets` in the chat, or the daemon API on `~/.tern/tern.sock`:

| Method | Path | |
|---|---|---|
| `GET` | `/v1/secrets` | Every known secret as `{name, in_file, in_env, used_by}`. Values are never returned. |
| `PUT` | `/v1/secrets/{name}` | Store `{"value": "..."}` in `secrets.toml`. |
| `DELETE` | `/v1/secrets/{name}` | Remove it from `secrets.toml` (`404` if it isn't there). |

```sh
curl --unix-socket ~/.tern/tern.sock -X PUT http://tern/v1/secrets/ANTHROPIC_API_KEY \
  -H 'content-type: application/json' -d '{"value": "sk-ant-..."}'
```

Both create `secrets.toml` with mode 0600 if needed, keep its comments, and take effect without a restart for providers, connectors, and newly started MCP servers. The Telegram gateway reads its token only when the daemon starts. A secret set in the environment takes precedence over the file.

**The agent can ask you for a secret.** When a connector it wrote (or anything else) needs a key that isn't set, the agent calls `secret_request` with the name and a reason. You get a private prompt: inline in the TUI, as a card in the desktop app, or in Telegram as a pointer to the app (a chat never takes the value). The prompt shows what would read the secret, for example `connector weather (draft) · reaches api.weather.com`. The value goes straight into `secrets.toml`; the agent only learns whether each name was `set`, `already_set`, `skipped` or `timed_out`. Values that are already set aren't asked for again unless the agent asks to replace them. A prompt waits up to `secret_request_timeout_secs` (default 900); runs with nobody to ask (`tern run --yes`) skip it. API clients see prompts on `/v1/events` as `{"secret_requested": …}`, list them with `GET /v1/secret-requests`, and answer with `POST /v1/secret-requests/{id}` `{"values": {"NAME": "…"}}` (names left out are skipped; only names the request asked for are accepted; owner scope).

Environment variables: `TERN_HOME` (data directory, default `~/.tern`) and `TERN_LOG` (log level, e.g. `debug`).

### Fallback chains

A model setting can be a list instead of one model: `default_model = ["anthropic:claude-sonnet-4-5", "openrouter:anthropic/claude-sonnet-4.5"]`. So can `subagents.default_model` and each `subagents.models` alias.

- **What moves on.** When a model call fails with an overload (`529`, Anthropic's `overloaded_error`), a server error (`5xx`), a rate limit (`429`), a timeout or network error, or an auth or billing error (`401`, `402`, `403`), it is retried as usual, and once those retries are spent the *same call* goes to the next model in the list. Other `4xx` errors (a bad request, a model that doesn't exist, a body too large) fail the run without trying the rest: the request itself is wrong. A refusal is an answer, not an error, so it never falls back.
- **Which runs get it.** A chain belongs to its first model: any run on that model gets the chain, whether `default_model`, a session's `/model`, a schedule or `agent_spawn` picked it. Picking a different model in `/model` runs without fallbacks; picking a new default keeps the chain's fallbacks behind it. If two chains start with the same model, the first (in the order above) applies and `tern doctor` warns.
- **What is recorded.** The journal stores which model answered each step, so `tern resume` and replays reuse that answer and never pick again. `tern runs show` prints `answered by openrouter:… (anthropic:… HTTP 529 (overloaded))` under such a step, and the app's run inspector shows the same. Tokens are counted against the model that answered, so schedule `usd_per_day` caps use its price.
- **Next call starts over.** Each model call tries the first model again; a run can switch back and forth. Prompt caching doesn't carry across providers, so a switched call costs full input price.
- `tern doctor` checks every model in each chain: that it parses, that its provider is configured, and (online) that the provider lists it. A fallback whose provider can't be built when a run starts (its key unset) is skipped with a warning in the log.

## Memory

tern remembers in two places:

- **`user.md` and `memory.md`** in `~/.tern`: what every conversation needs, in the system prompt each time. `user.md` is about you; `memory.md` holds core facts the agent keeps. You can edit both; the agent changes them with `profile_edit` (read, append, or replace to consolidate). Each has a cap, 1500 tokens by default (`[memory]` in config). A write that would go over is refused and the agent is asked to consolidate: merge, drop what's stale, and move details to long-term memory. A file you edit past its cap is cut in the prompt with a note asking the agent to consolidate, and `tern doctor` warns. Sub-agents see both files but can't change them.
- **Long-term memory**: facts saved with `memory_save`, found with `memory_search`, never in the prompt by themselves.

**Flush before compaction.** When a long conversation is about to be summarized, the agent first gets one silent model call over the part being summarized, with only `memory_save`, to keep durable facts (preferences, decisions, people, commitments). It's journaled, so a resumed run doesn't repeat it, and its tokens count toward the run's budget. If it fails, compaction goes ahead. Sub-agents and scheduled runs don't flush, nor does a run whose token budget can't cover it. `memory.flush = false` turns it off.

## Agents

You can have several agents, each with its own job: a name and avatar, a **persona** (who it is, its job, its tone), a model (or a fallback chain), the **tools** it may use, its **memory**, and its own **routines**. Each has a **DM**, a main channel like Home: one continuous chat with the same cuts, carry-over, History and queued sends. See [agent-profiles-design.md](agent-profiles-design.md).

- **The agent you already had is `tern`**, the default agent. Its persona is `~/.tern/persona.md`, its memory the shared memory, and its DM is Home. It can be renamed and given an avatar (its id stays `tern`); its model is `default_model`. It can't be archived.
- **Persona**: `~/.tern/agents/<name at creation>/persona.md`, editable in an editor or through the API. A new agent starts from a template (Research, Inbox, Marketing, Blank), never a blank page. tern always adds its own sections under every persona (memory, time, workspace, secrets, untrusted content, sub-agents, and, with more than one agent, who the others are): a persona can change who the agent is, not those rules.
- **Tools**: `{"builtins": "all" | "read_only" | [names], "connectors": "all" | [names], "mcp": "all" | [servers], "skills": "all" | [names]}`; a missing key means all. A new agent gets every built-in tool except `shell`, no connectors, no MCP servers, all skills. Tools out of scope aren't offered to the model and are refused if called. Every agent keeps `channel_read`, `channel_post`, `history_search`, `secret_request` and its memory tools. Its sub-agents get no more than it has.
- **Memory**: `private` (the default for new agents) or `shared`. `user.md` is always shared: who you are is the same for every agent. A private agent has its own `memory.md` (next to its persona) and its own long-term memories: `memory_save` keeps them its own unless `shared: true`, and `memory_search` finds its own and the shared ones. `session_search` only ever searches the agent's own conversations, never another agent's DM.
- **Routines belong to an agent.** A schedule an agent creates (`schedule_create` in its DM) is its own: it runs as that agent and posts to its DM. Made in a topic channel, it posts to that channel instead (and its agent joins it). `schedule_create` with `"agent": "research"` makes the routine Research's, so it runs as Research rather than as the caller asking Research each time; that is allowed only for an agent the caller may hand work to (`delegates`), checked by tern. The `schedule_*` tools see and change only the calling agent's schedules. Archiving an agent pauses its routines.
- **Agents hand work to each other.** A run that can start sub-agents may start one *as another of your agents*: `agent_spawn` with `"agent": "research"` runs it with Research's persona, tools, model and memory, and it reports back like any sub-agent (same limits, budget and taint). Its prompt lists the agents it may hand work to, each with what it's for, and says to delegate a matching request rather than start a generic sub-agent. Who may hand work to whom is each agent's **Can hand work to** (`delegates`): tern may reach every agent unless you narrow it, other agents none until you choose. The sub-agent card shows the agent's avatar and name.
- **Approvals land where the run is**: an agent's DM shows its own, and Home shows a pointer to it (one request, answered in either place).
- **Edits apply to the next run.** A run freezes its agent's persona, model, tool scope and memory scope when it starts.
- **In the desktop app**, AGENTS in the sidebar lists your agents under Home (which carries tern's avatar; right-click it to edit tern): a badge counts what waits for you, a spinner shows one working, a dot new routine posts, and **+** creates one. An agent's row opens its DM, the same view as Home (dividers, New topic, History) with its name and avatar, its model, and **Edit** (the tune icon). The sheet has a template (Research, Inbox, Marketing, Blank; a template's routine can be added with it), name, avatar (glyph and colour), persona with what tern adds to every agent shown read-only beneath, model (a comma-separated list is a fallback chain), tools (built-ins, connectors, MCP servers with their status, skills; **Read-only** in one click), memory (Private / Shared) and Archive. The side panel follows the agent: its routines (**All** shows every agent's), the tools it may use, and its memory with chips for its own and the shared memories (in Home, Shared and each private agent). An approval from an agent's DM shows there, with a pointer in Home: one request, answered in either.
- **TUI**: `/agent <name>` talks to that agent in its DM (`/new` starts a new topic there, `/agent tern` goes back), `/agents` lists them with what's working and waiting. A sub-agent of the current run with the same label wins.
- **API** (`agents` in `/v1/health`): `GET/POST /v1/agents`, `PATCH /v1/agents/{id}` (any field, `archived: true`), `GET/PUT /v1/agents/{id}/persona` (GET adds `system_sections`), `GET /v1/agents/templates`, `GET /v1/memories?agent=shared|<agent>&q=`, `GET /v1/schedules?agent=` and `agent` on schedule create/update. `/v1/events` adds `{"agents_changed": {}}`; run events, approvals and secret requests carry `agent_id`, and sessions their `agent_id` when not `tern`.

## Schedules

The agent creates schedules with `schedule_create` and changes them with `schedule_update`; the desktop app calls them routines. `schedule_create` answers with a `tern://routine/<id>` link the agent passes on; in the desktop app it opens the routine, and the conversation shows a card with when it runs, who runs it and where it posts. Every routine is listed in three places: its agent's side panel, the channel it posts to (the side panel, and the clock with a count in a topic channel's header), and **Routines** under LIBRARY in the sidebar. Each has run now, edit, pause and delete. `/v1/events` sends `{"schedules_changed": {"id"}}` whenever a tool or the API creates, changes or deletes one, and the lists refresh. Each one runs its prompt in a session of its own and delivers the final answer to its `deliver_to` (a Telegram chat, a [gateway connector](#gateway-connectors-ntfy-slack) chat such as `ntfy:<topic>` or `slack:<channel id>`, or nowhere: the result stays in the session).

- **Nothing to say, nothing sent.** A scheduled run is told it may answer exactly `NO_REPLY`. Then nothing is delivered (no Telegram message, no desktop notification), and the run is recorded as completed and *quiet*: `GET /v1/sessions/{id}/runs` has `quiet: true`, `run_finished` events carry `quiet: true`, and the app shows "Last run quiet".
- **Quiet hours.** A result that finishes inside quiet hours (`[schedules] quiet_hours`, default 22:00–07:30 local; per schedule `quiet_hours = "HH:MM-HH:MM"`, `"none"` or `"default"`) is held, not the run: the run happens on time. Held results are kept in the database (a restart keeps them) and sent when the window ends, as one message per chat if there are several. A schedule marked `urgent` delivers at once.
- **Caps.** Per schedule, or as defaults under `[schedules]`: `runs_per_day`, `usd_per_day` and `tokens_per_run`.
  - Daily caps count from local midnight. When a run is due and a cap is reached, the schedule **pauses until the next day** and posts one notice to its delivery target. Resume it sooner from the app (the routine's menu), with `schedule_update` `resume: true`, or `PATCH /v1/schedules/{id}` `{"resume": true}`; counting then starts again from that moment. "Run now" isn't capped.
  - `usd_per_day` needs the model's price, from `providers.<name>.prices` or the provider's model list. Without one the USD cap is ignored; the log says so once, and `tern doctor` lists it.
  - A run that uses more than `tokens_per_run` is stopped and fails with the reason ("stopped: this run used N tokens, over its cap of M"), which is delivered like any failure.
- `tern doctor` reports schedules paused by a cap, USD caps that can't apply, and deliveries waiting for quiet hours to end.
- **Results are posted to a channel** (its agent's DM, Home for `tern`, unless the schedule's `channel` says otherwise; `POST`/`PATCH /v1/schedules` take `channel`, a slug or id). A `NO_REPLY` run posts nothing. `deliver_to` stays the push to Telegram.

### Routines: triggers, watches, gates, initiative

A schedule is a **routine**: a prompt with a trigger. The tools and the API keep their `schedule_*` and `/v1/schedules` names; schedules from before routines are cron routines that push every result, as they did.

- **Triggers.** `cron` (repeating), `at` (once, a local `"YYYY-MM-DD HH:MM"`: reminders; it turns itself off after running, and a new `at` turns it on again), `watch`, or a [webhook](#webhooks).
- **Watches.** A connector can declare watches next to its tools:

  ```lua
  watches = {
    review_requests = {
      description = "PRs where my review is requested",
      default_every = "5m",
      fn = function(ctx, cursor)
        -- poll; return what is new and where you got to
        return { events = { { id = "123", title = "Fix login" } }, cursor = { last = "123" } }
      end,
    },
  },
  ```

  A routine with `watch = "github.review_requests"` (and `every`, at least `1m`, default the watch's own) is polled by the daemon. **A poll that finds nothing costs no tokens**: no model is called. Each new event starts an ordinary run in the routine's session with the event attached as untrusted input, so the run is [tainted](#taint) (its writes ask, or wait in the outbox). Events are deduplicated by their `id` (or their content), and the cursor is saved with them, so a restart or an overlapping poll never starts an event twice. The first poll only records what is already there, unless `backfill: true`; events marked `origin = "self"` are skipped unless `include_self: true`; more than `max_events_per_poll` new events at once share one run. A watch runs in the connector's sandbox with its hosts and secrets, GET only (`read_via_post = true` allows POST), and only for an active, approved connector. Creating one tells you its interval and the hosts it reaches. No watch is on until you (or the agent, with your approval) create one.
- **Gates.** A cron, once or webhook routine can have a `gate`: Luau run before each slot, at no token cost. False skips that run. An expression (`os.date("*t").wday ~= 1`) or statements with `return`; it gets `ctx` and `routine` (`id`, `last_run_at`, `now`), and with `gate_connector` that connector's hosts (GET only), secrets and tools as `connector` (`#connector.tools.unread.run(ctx, {}) > 0`). A gate that fails lets the run go ahead.
- **`last_check`** in `GET /v1/schedules` (and the app's tooltip) says what the last poll or gate found.
- **Initiative.** Every result is posted to the routine's channel; its initiative decides whether it is also sent to its chat (`deliver_to`):

  | Level | Sent |
  |---|---|
  | `silent` | never |
  | `digest` (the default for new routines) | with the next digest (`schedules.digest_times`, 08:30 and 17:30), all of that chat's held results in one message |
  | `notify` | now if a quick check by the cheapest model says it's worth interrupting for (its headline goes first), else in the digest |
  | `interrupt` | now; quiet hours still hold it unless `urgent` |

  Set it per routine (`initiative`, `"default"` or `null` for the default), or for all with `schedules.initiative`. The **dial** (`schedules.initiative_dial`, or `PUT /v1/initiative {"dial": "vacation"}`) moves every routine one level `quieter` or `louder`, or makes everything silent on `vacation`; `/v1/status` reports it.
- **API**: `POST /v1/schedules` takes one of `cron`, `at` or `watch` (+ `every`, `backfill`, `include_self`), and `gate`, `gate_connector`, `initiative`; `PATCH` changes any of them. The view adds `trigger`, `at`, `watch`, `every_secs`, `last_check`, `gate`, `gate_connector` and `initiative` (each absent when unset).
- **In the desktop app** the routine editor has Repeating / Once / On events / Webhook, an initiative menu, and a gate under "Quiet hours and limits".

### Webhooks

A webhook routine runs its prompt each time something POSTs to its URL: a GitHub push, a form, a script. It's the one thing the internet can call, so it is fenced off:

- **Its own listener.** Deliveries arrive only on `[webhooks] listen`, a port separate from the API's, serving nothing but `POST /v1/hooks/<routine id>` and `GET /healthz`. It's off by default; without it, `tern hooks fire` still delivers locally. Put it behind a tunnel (Cloudflare Tunnel, Tailscale Funnel) or reverse proxy and set `public_url`, so you expose webhooks without exposing the API.

  ```toml
  [webhooks]
  listen = ["tcp://127.0.0.1:8712"]
  tls = "off"                         # the tunnel or proxy terminates TLS; needs [api] trust_proxy = true
  public_url = "https://hooks.example.com"
  ```

  | Key | Default | |
  |---|---|---|
  | `webhooks.listen` | none | `tcp://host:port` listeners. |
  | `webhooks.tls` | `[api] tls` | `auto`, `files` (with `[api]`'s `tls_cert`/`tls_key`) or `off` (loopback listeners behind a proxy, with `api.trust_proxy`). |
  | `webhooks.public_url` | the first listener | The base of each hook's URL. |
  | `webhooks.max_body_kb` | `256` | Larger bodies get 413. |
  | `webhooks.per_minute` | `30` | Deliveries one routine accepts per minute; more get 429. |
  | `webhooks.tolerance_secs` | `300` | How far a signed timestamp may be from now. |
  | `webhooks.header_timeout_secs` | `10` | Time for the TLS handshake and each request's headers, idle keep-alive included; slower connections are closed. Needs a restart. |
  | `webhooks.body_timeout_secs` | `30` | Time for a body once its headers are in; slower gets 408. |
  | `webhooks.max_connections` | `64` | Open connections at once, across the webhook listeners; more get 503 (closed under TLS). Needs a restart. |
- **Its own secret.** Each routine has a secret (`tern_whk_…`), shown once when it's made or rotated. With `auth = "hmac"` (the default) senders sign the body with HMAC-SHA256: GitHub's `X-Hub-Signature-256: sha256=<hex>`, or `X-Tern-Signature: t=<unix seconds>,v1=<hex of HMAC("<t>.<body>")>`, whose timestamp must be within `tolerance_secs`. With `auth = "bearer"` they send `Authorization: Bearer <secret>`. **Device tokens are refused on webhook paths, and webhook secrets on the API**, whatever else the request carries. Management (`/v1/webhooks`) is owner-only.
- **Refusals**: an unknown or revoked hook 404, a body over `max_body_kb` 413, a missing or bad signature or a stale timestamp 401, over `per_minute` 429, a paused routine 409. One address with 20 failures in a minute gets 429 until it passes. A body slower than `body_timeout_secs` 408; past `max_connections`, 503.
- **Each delivery once.** A delivery is keyed by a hash of what was signed (the body, plus the timestamp for `X-Tern-Signature`), so a re-delivery or a captured request replayed starts nothing (`200 {"status": "duplicate"}`). Identical bodies count as one event under `X-Hub-Signature-256` and `bearer`.
- **Untrusted.** The routine sees the event as `{"headers": {…}, "body": …}`: the body (parsed as JSON when it is), and only the headers that name the event (`content-type`, `user-agent`, `*-event`, `*-event-type`, `*-delivery`; never credentials). Its run starts [tainted](#taint) ("started by a webhook") with the event wrapped as untrusted, so writes ask or wait in the outbox. Payloads over 16 KB are cut.
- **Gates filter first.** A gate sees `event` (and `routine`): false drops the delivery with no model call (`200 {"status": "ignored"}`), `true` keeps it, and a table replaces it, e.g. `event.headers['x-github-event'] == 'issues' and event.body.action == 'opened'`, or `return { title = event.body.issue.title }` to send the model only that.
- **CLI**: `tern hooks create "<prompt>" [--auth hmac|bearer] [--gate …] [--gate-connector …] [--agent …] [--channel …] [--initiative …]` prints the URL and secret; `tern hooks list`, `rotate <id> [--auth]` (the old secret stops at once; also revives a revoked hook), `revoke <id>`, and `fire <id> [--body JSON | --file path]` (a local delivery: no secret, same gate, dedup and taint). `tern hook` works too.
- **API** (`webhooks` in `/v1/health`, owner only): `GET /v1/webhooks`, `POST /v1/webhooks` (the fields of `POST /v1/schedules` without a trigger, plus `auth`) → `{schedule, webhook}` with the secret, `POST /v1/webhooks/{id}/rotate {auth?}`, `DELETE /v1/webhooks/{id}`, `POST /v1/webhooks/{id}/fire`. The prompt, gate and limits change through `PATCH /v1/schedules/{id}`; giving the routine another trigger there revokes its secret. The agent's `schedule_*` tools can't make webhook routines (that would put a secret in its context).
- **Desktop**: **Webhook** in a new routine's editor (signed or secret-in-header); the secret is shown once with copy buttons. Editing one shows its URL, **Rotate secret…** and **Revoke**.
- `tern doctor` says whether a webhook listener is on, whether it answers, whether it's loopback-only without a `public_url`, and (unless `--offline`) whether `public_url/healthz` reaches this core from outside.

## Channels

Channels are topic timelines (`#finances`, `#launch`) over one shared memory; see [channels-design.md](channels-design.md). **Home** is the main channel, your direct line to the agent; it always exists and can't be archived.

- A channel's timeline is its **posts**: what you send there, the agent's answers, and routine results. Each post has an `author` to show and a stable `author_id` (`owner` for you, `tern` for the agent, `routine:<schedule id>`), which becomes a member or agent id later. Tool calls stay in the thread's session.
- Sending into a topic channel (`POST /v1/channels/{id}/posts` `{text}`) writes your post and starts a run in the channel's **main thread**; its answer is posted as a reply to yours. Every conversation is flat: there are no side threads, and `reply_to` only quotes the post you're answering (its author answers). A channel's `instructions` are added to the system prompt of its runs.
- Home's conversation is its messages, so sending there writes no post; Home's posts are what comes from elsewhere, such as routine results.
- **Home is one continuous chat** cut into sessions behind the scenes, so no context grows forever. A message starts a new session after `[home] idle_hours` (default 6; `[main]` works too, and the same rules cut every agent's DM) without one, or once a new day has begun (`day_starts`, default 04:00 local) and the last message is an hour old, or when you ask for a **New topic** (the desktop's button or ⌘N, `new_topic: true` on the API, `/new` in Telegram). Never in the middle of a run. The new session's first run starts with a summary of the last one, what's still open there, and its last two exchanges verbatim; the agent can look further back with `history_search`. The desktop shows the stream as one scroll with a divider at each cut (click it for what was carried over), and **History** lists earlier sessions and conversations from before channels (old side chats among them), to read beside the stream. Telegram chats follow the same rules, each as its own stream.
- **Agents see what others posted.** Before each model call, a run in a channel (Home included) is shown the posts that came in from other threads, routines and agents since its conversation last looked: up to 20, newest kept, as one note that calls them information, not instructions. Posts written by a tainted run are framed as untrusted and taint the run that reads them. The agent can page or search posts with `channel_read` and post an update with `channel_post` (`needs_you` marks one that needs you; at most 10 per run; a tainted run's posts are tainted). Sub-agents and routines don't get the catch-up. A sub-agent posts only when spawned with `"post": true` (`agent_spawn`): then it can `channel_post` as `tern/<label>`, and its report is posted when it finishes.
- **Needs you**: a `needs_you` post outside Home also puts a pointer in Home. Both stay open until you mark either one done (**Done** in the app, `POST /v1/posts/{id}/resolve`); `needs_you` in `GET /v1/channels` counts the open ones (in Home, with Home's own approvals and secret requests). An approval or secret request from a run outside Home is such a post (with `approval_id` or `secret_request_id`): it closes when answered, and "Done" alone is refused (`409`). A channel shows one live line per member at work ("tern/research-1 · Running http_fetch"); `GET /v1/runs/active` gives each run's `channel_id` and `author_id`.
- **Sending while tern is working doesn't stop it.** In Home, a channel, or a Telegram chat, the message goes to the run in progress, which reads it at its next step (Stop still stops). `POST /v1/runs` with a `session_id` still replaces the run, as before.
- **No side chats or threads.** Home, agent DMs and topic channels are each one flat conversation; for a tangent, use New topic or move it to a channel. `POST /v1/runs` refuses `branched_from` and `channel`.
- **In the desktop app**, CHANNELS in the sidebar lists topic channels (bold with a dot when there are new posts, a spinner while tern works there; read state stays on this computer) and **+** makes one (a name, and optional instructions). A channel opens as a timeline: posts with who wrote them (◷ for routines), a quote line on a reply when what it answers isn't right above it, **See work** (the run behind a post, tool steps and all, to read in the right pane), and a composer that posts to the channel. The gear edits the title and instructions or archives the channel. A routine's editor chooses the channel it posts to. **Approvals and secret requests wait in the conversation their run is in**, as a card in the channel's timeline (a sub-agent's or delegate's in its top-level run's conversation, a routine's where it posts); Home shows a pointer with **Go to**, and **Approve once** / **Deny** for an approval. The sidebar badges each channel and agent with what waits there.
- **Moving conversations into a channel**: the folder icon on Home's header (the current topic), on a divider (the topic it starts), in the reading pane, or several at once from History (tick them, then **Move to channel…**) moves them into a topic channel, new or existing, with their history; a moved conversation becomes its agent's conversation there if it has none yet, so the channel carries on from it. Where each was, a "Moved to #slug" note stays; the channel gets a "Moved here from …" post. A conversation tern is working in can't move until it finishes. Moving Home's current topic starts a fresh one. API: `POST /v1/channels/move` `{sessions, channel | new_channel: {title, slug?, instructions?}}` (owner scope).
- **Agents in a topic channel.** A channel has a **lead**, the [agent](#agents) that answers posts that mention no one (tern unless you pick another), and **members**: tern is in every channel, others join with **+ Agent** in the header, when you `@mention` them, when a routine of theirs posts there, or when one of their conversations is moved in. `@research` in your post asks Research instead of the lead (two mentions ask both, each in its own thread); the composer completes `@`. Agents' own `@mentions` wake nobody unless the channel's settings turn on **Agents can wake each other**: then at most 2 agents in a row from your message, and 6 agent-started runs an hour (a quiet note says when a mention wasn't followed). A run woken by a post from a tainted run starts tainted. An agent reads and posts only in channels it's a member of, its own DM, and the conversation it's in. API: `GET/POST /v1/channels/{id}/agents` (`{agent}`), `DELETE /v1/channels/{id}/agents/{agent}`, and `lead`, `agent_wakes`, `max_hops`, `wakes_per_hour` on `PATCH /v1/channels/{id}`; posts carry `mentions` and `wake_depth`, and a send's result lists each agent it `woke`.
- `GET /v1/channels/home/stream?before=&limit=` returns the stream's messages and posts interleaved, newest first, with a `cut` item where each session starts, and `next` to page back.
- API: `GET/POST /v1/channels`, `PATCH /v1/channels/{id}` (`title`, `instructions`, `archived`), `GET /v1/channels/{id}/posts?before=&limit=` (newest first), `POST /v1/channels/{id}/posts`, `GET /v1/sessions?channel=` (a channel's conversations). `/v1/events` adds `{"post_created": {"post": …}}` and `{"channels_changed": {}}`, and run events carry `channel_id`.

## Telegram

1. Create a bot with [@BotFather](https://t.me/BotFather) and set `TELEGRAM_BOT_TOKEN` (env or `secrets.toml`).
2. Add to `config.toml`:
   ```toml
   [gateway.telegram]
   enabled = true
   bot_token_secret = "TELEGRAM_BOT_TOKEN"
   ```
3. Start `tern daemon`, run `tern gateway pair`, and send the printed code to your bot.

Only paired Telegram accounts can talk to the bot or answer its approval buttons.

**Agents in Telegram.** `/agents` lists your [agents](#agents); `/agent research` switches the chat to Research's DM (its replies start `Research · `), and `/agent tern` goes back. Each agent keeps its own stream in the chat, cut by the same rules (`/new` is a new topic with the agent you're talking to). In a group with **topics** enabled, where the bot is an admin that may manage topics, `/agent research` opens a topic named Research instead: everything written there goes to Research. Approvals for a run go to the chat or topic that started it.

## Gateway connectors (ntfy, Slack)

A gateway connector is a [Luau connector](#tern-connectors) with a `gateway` table, which tern runs as a chat transport beside Telegram. Routine results (after quiet hours, initiative and digests), approvals, outbox cards and secret notices reach it like Telegram, and messages written there start or continue runs. Two are built in:

```bash
tern gateway add ntfy                          # or --server https://ntfy.example.com
tern gateway add slack --channel C0123456789   # asks for SLACK_BOT_TOKEN
tern gateway pair                              # then send the code as your first message
```

`add` installs `~/.tern/connectors/<name>.luau` (yours to read and edit) and adds `[gateway.connectors.<name>]` to `config.toml`; restart the daemon to start it. Routines then deliver to `<name>:<chat>`; the desktop's routine sheet offers every gateway the daemon runs.

- **ntfy** needs no account or bot: subscribe in the ntfy app to the push topic `add` printed, and write to the reply topic to talk to tern. Approval and outbox notifications carry buttons that answer from the phone. ntfy has no users, so anyone who knows a topic name can read or write it: keep the names private, or use a server with access control and uncomment `auth = "NTFY_TOKEN"` in the file.
- **Slack** uses a bot token, no OAuth: create an app in your workspace with the bot scopes `chat:write` and `channels:history` (`groups:history` for a private channel), install it, save the `xoxb-…` token as `SLACK_BOT_TOKEN`, and invite the bot to the channel. Targets are `slack:<channel id>` or `slack:<channel id>/<thread ts>`. tern reads the channel's messages, not replies inside threads, and approvals ask for a numbered reply (Slack's buttons need a public URL).
- **Pairing** works as in Telegram: only paired senders are the owner. On ntfy, which names no sender, the reply topic pairs. Others get one fixed reply, or with `strangers = "ignore"` nothing; with `strangers = "taint"` their message starts a [tainted](#taint) run in a session of its own, which asks before any write.
- **Without buttons**, approvals list numbered choices (`1 · Allow once, 0 · Deny`): reply with the number. Outbox cards say `approve 12` or `reject 12`. `/new`, `/stop`, `/status` and `/outbox` work as in Telegram.
- **Writing one**:
  ```lua
  gateway = {
    kind = "mychat",
    threads = false,
    send = function(ctx, to, text, opts) ... end,                -- opts: thread, kind, run_id, settings
    buttons = function(ctx, to, text, buttons, opts) ... end,    -- optional; buttons: { label, data }
    poll = function(ctx, cursor, settings)                       -- optional
      return { messages = { { id = "…", chat = "…", from = "…", text = "…" } }, cursor = … }
    end,
  }
  ```
  A tap is a message with `action` set to the button's `data`. `send` and `buttons` may POST; `poll` is GET-only unless the gateway sets `read_via_post = true`. The connector's hosts, `auth` and rate apply. `tern gateway list` and `tern doctor` show whether it loads.

### Remote access

The daemon can also listen on TCP so another computer or a phone can reach it. It's off by default, and every TCP request needs a device token:

```toml
[api]
listen = ["unix", "tcp://0.0.0.0:7420"]
tls = "auto"   # self-signed certificate that devices pin; or "files" with tls_cert/tls_key
```

Paired devices can't add or test `stdio` MCP servers, since those run commands on this host outside the sandbox; set `remote_mcp_stdio = true` under `[api]` to allow it. HTTP servers, toggles, restarts and removal work from any owner device.

Restart `tern daemon`, then run `tern devices pair` (default scope `chat`: conversations and approvals, but not config, secrets or devices). It prints a QR code and a `tern://pair?…` link valid for 10 minutes. On another machine, `tern devices join '<link>'` prints a token; then `TERN_REMOTE_TOKEN=<token> tern --remote https://host:7420 --pin sha256:…` attaches the chat UI. `tern devices list` and `tern devices revoke <id>` manage devices; revoking signs the device out at once.

## Sandboxing

By default `shell` runs on this machine after an approval prompt, with a scrubbed environment (no secrets), a timeout, and an OS sandbox (Seatbelt on macOS, Landlock on Linux): it can write only to the workspace and temp directories, can't read `~/.ssh`, `~/.tern` or the rest of your home directory, and has no IP networking. A command that needs the network (`git push`, `npm install`) asks for it with `network: true`: that asks you every time (the allowlist and "always" don't cover it), and its output counts as untrusted. `network = true` gives every command the network, as before (configs from older `tern config init` say so explicitly). Add paths a toolchain needs:

```toml
[sandbox]
read_paths = ["~/.gitconfig", "~/.config/git", "~/.rustup"]
write_paths = ["~/.cargo/registry"]
network = false
```

To run it in a container instead:

```toml
[sandbox]
backend = "docker"
docker_image = "debian:stable-slim"
```

Commands then run in one long-lived container per workspace, with the workspace mounted at `/workspace`, files owned by you, no capabilities, and limits from `docker_memory`, `docker_cpus` and `docker_pids`. What each backend confines, per platform, is in [docs/sandbox-design.md](sandbox-design.md).

## Compute (`python`)

The `python` tool runs a script in [Monty](https://pydantic.dev/monty), a Python subset written in Rust, inside tern. It is for computation over data your tools fetch: a script calls tools by their names, for example `lua__county__lookup(address=a)` in a loop, and returns a summary. It never asks for approval itself, because every tool call it makes goes through the same checks as a direct one (docs/python-tool-spec.md).

- **What a script can call**: the run's read-only tools (`read_file`, `http_fetch`, `memory_search`, Luau and MCP tools with `effect = "read"`, …). Write and destructive tools, `shell`, sub-agents, posts, secrets and connector or schedule management raise `PermissionError` ("call it directly"), as does a read that would need your approval.
- **Recorded like direct calls**: each call is a `py_call` step in the run's journal (`tern runs show` lists them under the `python` step). After a crash, `tern resume` re-runs the script with the same clock and random seed and answers calls already made from the journal, so a lookup isn't repeated.
- **Taint**: a call to an untrusted tool taints the run, and the script's result reaches the model wrapped as untrusted.
- **Sandbox**: no network, environment variables or processes. Files under the workspace are readable at `/workspace`; nothing is writable. Each script runs in its own `tern __monty-exec` process under the same OS sandbox as `shell` with networking off, so a crash or a runaway script ends that process, not tern (the tool returns an error).
- **Limits** per script: 30 s of interpreter time (10 s between tool calls), 10 minutes in all, 256 MB for large values, 1000 levels of recursion, 200 tool calls, 64 KB of code, and the usual 32 KB result (16 KB of it for `print` output).
- **The subset**: functions, comprehensions, f-strings, `match`, dataclasses and the modules `json`, `re`, `csv`, `math`, `datetime`, `collections`, `itertools`, `functools`, `decimal`, `random`, `hashlib`, `base64`, `bisect`, `tomllib`, `pathlib`. No class inheritance, generators, `del`, `@property`, or packages. Errors come back with a hint on what to write instead. For pandas or numpy the model uses `shell` with `python3`, which asks every time.
- Tool calls are synchronous for now (no `await` or `asyncio.gather`). Builds without the default `python` cargo feature leave the tool out.

## Security model

The threat model, what isn't covered, and how to report a vulnerability are in [SECURITY.md](../SECURITY.md). In short:

- **Secrets** come from the environment or `~/.tern/secrets.toml`, which must be mode 0600. Every secret value tern reads is scrubbed from the journal, conversation history, and connector and MCP server logs, and none reach the shell's environment.
- **File tools** can't modify `~/.tern` and can't read `secrets.toml` or the database. Reads and writes outside the workspace need approval, and paths that escape the workspace through `..` or symlinks are refused.
- **`http_fetch`** refuses loopback, private, link-local, and cloud-metadata addresses, checking every redirect hop and pinning DNS between the check and the connection.
- **Luau connectors** run in sandboxed VMs (no files, processes, or `require`; 5 s and 64 MB per call) and can only reach the hosts and paths they declare, including across redirects, within a request rate. Credentials declared with `auth` are added by tern and never visible to the script; only legacy connectors using `ctx.secret` see secret values, and they are flagged. An activated connector that changes, or that declares different capabilities than you approved, isn't loaded.
- **`python`** scripts reach nothing but the run's read-only tools, each checked and journaled like a direct call, and a read-only view of the workspace; see [Compute](#compute-python).
- **`shell`** runs in an OS sandbox (Seatbelt on macOS, Landlock on Linux) that confines writes to the workspace and keeps your home directory and `~/.tern` unreadable; see [Sandboxing](#sandboxing).
- **Effects** are enforced by tern, not the model: `destructive` calls ask every time and `money_movement` / `trade` tools are refused outright, even with `--yes`.
- **Taint** ([below](#taint)): once a run has read untrusted content, its `write` and `destructive` calls ask, whatever "always" grants say.
- **"Always allow"** is never offered for destructive tools, for calls in a tainted run, for commands that run other commands (`bash`, `env`, `sudo`, `find`, interpreters, …) or for activating connectors, and allowlisted shell commands can't chain (`;`, `|`, `$( )`, …).

### Signing in

When the agent connects a new service it checks how the service signs in (`auth_discover`), shows you a card to choose between signing in and an API key (`auth_propose`), and then a "Sign in to …" card (`connection_add`) that opens the service's own page in your browser, or a code to enter on any device. OAuth tokens are kept like secrets, refreshed by tern, sent only to the connector's declared hosts, and never shown to the agent. Sign-in addresses the agent found on the web must be on the service's own domain or a known identity provider, and the card says when the service publishes nothing to confirm them. See [`tern connections`](#tern-connections).

### Taint

A run is **tainted** once untrusted content reaches it (docs/personal-agent-tooling-spec.md §5.3):

- **What taints**: the result of a tool marked untrusted (`http_fetch`, `mcp_list`, Luau tools with `untrusted = true`, every tool of an MCP server with `untrusted = true`, and `shell` when the command could reach the network: it asked for it with `network: true`, `sandbox.network = true`, or no OS sandbox enforces the default `false`), mail from a tainted run (a sub-agent's report, `agent_send`), and being spawned by a tainted run. Untrusted input that starts a run (webhooks, watches, messages from people who aren't you) will taint it from the start once those exist.
- **It sticks** for the rest of the run, survives a crash and `tern resume` exactly (it's rebuilt from the journal), and doesn't carry to the next run in the conversation.
- **What it changes**: every `write` and `destructive` call asks, even with an "always" grant or a pre-granted one (workspace writes, memory, schedules), and "always" isn't offered. The prompt adds "this run read untrusted content (result of http_fetch)". Calls that coordinate sub-agents (`agent_spawn`, `agent_send`, `agent_cancel`, `agent_ask_parent`) and `channel_post` keep their grants, since what they reach is tainted too. With nobody to ask (a routine at night), the call goes to the [outbox](#outbox). A network read (`http_fetch`, also from a `python` script) to a host the run hasn't reached yet asks too, so a page can't have the agent send what it knows somewhere new; see [New sites in a tainted run](#new-sites-in-a-tainted-run) for saying yes once. A tainted run also skips the memory flush before compaction. Only `autonomy = "yolo"` turns this off.
- <a id="new-sites-in-a-tainted-run"></a>**New sites in a tainted run.** The prompt for a site the run hasn't reached offers, besides **Allow once**: **Allow all sites for this run** (no more site prompts until the run ends; a resumed run remembers it; sub-agents and later runs ask again) and **Always allow <site>** (a standing grant for reads from that site, `host:<site>`; list and remove with `tern grants`). Neither ever covers a `write` or `destructive` call. A script can't ask, so its read from a new site is refused with a note to fetch it directly once (or after "all sites", allowed); sites a script reaches count for later calls. For agents meant to browse, turn the limit off: `[taint] egress = "allow"` in `config.toml`, or per agent (`egress` on `/v1/agents`, **Read any site without asking** in the app's agent editor), frozen on each run when it starts; sub-agents keep their parent's. API answers: `{"decision": "run" | "host"}`; requests carry `egress_host`.
- **What the model sees**: untrusted results arrive as `<untrusted source="http_fetch">…</untrusted>`, and the system prompt says text inside is data, not instructions.
- **Where it shows**: `tern runs list` / `runs show`, `taint` on runs in `GET /v1/sessions/{id}/runs` (absent while clean), `taint` on approval requests, `untrusted: true` on tool step outputs, the desktop's run inspector (a "tainted" badge), and approval cards in the desktop, the TUI, Telegram and `tern run`. A routine or channel answer written by a tainted run is a **tainted post** (`tainted: true`, a shield next to the author in the desktop).

### Outbox

A `write` or `destructive` call that asks while nobody answers (no app, TUI or tray attached, or `[outbox] wait_secs` passing, default 120) goes to the **outbox** instead of being denied (spec §5.2). The model is told it is "queued for approval as outbox #12" and carries on; a routine's post lists what is waiting. Then:

- **Approve** (the desktop's card where the run is, or Home's pointer to it, Telegram's button if the run reports to a chat, `tern outbox approve 12`, or `POST /v1/outbox/12/approve`) runs the call exactly once, on a small system run in the same session: a crash can't run it twice, and a second approval is refused. **Edit…** in the app (`--input '{…}'`, `{"input": {…}}`) approves it with arguments you changed. The result is posted to the channel (and sent to the chat), and the conversation learns it ran.
- **Reject** (`tern outbox reject 12 --note "…"`) drops it. A run still working is told either way, with your note.
- Items **expire** after `[outbox] expire_hours` (default 72) and say so once in Home.
- **Where it waits**: each pending item is a **needs you** post in the channel its run started in, by its agent (Home, an agent's DM, a topic channel), and outside Home also a pointer in Home, counted in `needs_you`. Deciding it (or its expiry) closes both; "Done" alone is refused (`409`), since the item must be approved or rejected. Posts carry `outbox_id`. A routine's items wait in the channel it posts to. In the app, Home and a DM show their own items' cards above the composer (the post says "Waiting below"); a topic channel's post has Approve and Reject, and so does Home's pointer to an item elsewhere.
- **Routines at night**: a routine inside its quiet hours doesn't send approval prompts to its chat (an attached app can still answer; otherwise the write goes straight to the outbox), and no outbox card is sent then. Its delivery, which keeps its quiet hours and initiative (digests included), ends with "Waiting for your approval: …" and "Approve with /outbox".
- **In Telegram**, `/outbox` lists what waits as cards with Approve and Reject (none on host commands, which say to decide them on that computer). A card updates ("→ Approved", "→ Rejected", "→ Expired") however the item is decided.
- **In the TUI**, `/outbox` opens a panel: `a` approve, `e` edit the arguments (JSON) and approve, `r` reject with a note. Deciding needs the background agent (`tern start`); without it the panel only lists.
- Commands that run on the host outside the sandbox can only be decided on that computer (the app or TUI over the local socket), as with approvals.

```sh
tern outbox                 # what is waiting
tern outbox list --all      # decided items too
tern outbox show 12         # arguments, why it waited, the result
tern outbox approve 12 [--input '{"to": "carol@example.com"}']
tern outbox reject 12 [--note "not to Bob"]
```

API: `GET /v1/outbox[?state=all|<state>]`, `GET /v1/outbox/{id}`, `POST /v1/outbox/{id}/approve` `{input?}`, `POST /v1/outbox/{id}/reject` `{note?}` (chat scope), and `{"outbox_changed": {"id", "state"}}` on `/v1/events`. `/v1/health` lists the `outbox` feature. Only the daemon defers: `tern run` and the TUI without a daemon deny as before.

### Autonomy

`autonomy` (spec §5.4) sets how much runs may do without asking. It is frozen on each run when it starts, so a resumed run asks where it asked before; sub-agents run under their parent's.

| `autonomy` | |
|---|---|
| `readonly` | `read` tools only; anything else is refused without asking (the agent is told it is read-only). Sub-agents and posts still work. |
| `supervised` (default) | As described under Approvals: `write` asks unless granted, `destructive` every time. |
| `autonomous` | An untainted `write` doesn't ask. `destructive`, forced prompts (commands outside the allowlist, activating a connector) and anything in a tainted run still ask. |
| `yolo` | Nothing asks, tainted or not. Tools flagged `money_movement` or `trade` are still refused. `tern doctor` shows it in red. |

Set it globally (`autonomy = "autonomous"` in `config.toml`) or per routine: `autonomy` on `POST/PATCH /v1/schedules` (`"default"` clears it). The agent's own `schedule_*` tools can't set it.

## Measurements

Release build (`--profile dist`) on an Apple M4 Max, macOS; Linux sizes from the musl cross-builds.

| Target | Goal | Measured |
|---|---|---|
| Stripped binary | < 25 MB | 13.9 MB (aarch64-apple-darwin); 14 MB aarch64-linux-musl, 16 MB x86_64-apple-darwin, 17 MB x86_64-linux-musl |
| Idle daemon RSS | < 30 MB | 11.9 MB (`ps -o rss` 5 s after start, no gateways) |
| `tern --version`, cold | < 50 ms | 4.8 ms median, 6.8 ms max (30 runs; first run after linking is slower while macOS checks the binary) |

## Troubleshooting

```sh
tern report        # writes ~/.tern/logs/report-<time>.md and prints its path
```

The report gathers what's needed to debug a problem in one file:
- your environment, `tern doctor`, and `config.toml`;
- your connectors;
- the journal of the most recent run that didn't finish (or `--run <id>`);
- any crash files, and the last 200 log lines.

Secret values tern knows about, and strings that look like API keys or tokens, are replaced with `[REDACTED]`. Review it before sharing; `-o -` prints it instead.

If tern hits an internal error it writes `~/.tern/logs/crash-<time>.txt` (message, backtrace, version) and tells you where. Logs are written synchronously, so the lines leading up to a crash are kept; `TERN_LOG=debug` makes them more detailed.

## Data

Everything lives in `~/.tern` (`TERN_HOME` to move it): `config.toml`, `secrets.toml`, `tern.db` (SQLite, WAL), `persona.md`, `user.md` and `memory.md` (included in the system prompt; see [Memory](#memory)), `agents/` (other agents' personas and private `memory.md`; see [Agents](#agents)), `skills/`, `connectors/`, `logs/` (daily rotated; set `TERN_LOG=debug` for more).
