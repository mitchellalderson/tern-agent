# tern reference

Everything tern does, command by command. For installing and a first chat, see the [README](../README.md); for how the pieces fit together, its [architecture](../README.md#architecture) section.

## What it can do

| | |
|---|---|
| **Models** | Anthropic, plus any OpenAI-compatible endpoint (OpenRouter, Ollama, vLLM, LM Studio). Models are `provider:model`; switch with `--model` or `/model`. |
| **Tools** | Files (`read_file`, `list_dir`, `search_files`, `write_file`, `edit_file`), `shell`, `http_fetch`, long-term memory and capped profile notes (`user.md`, `memory.md`), search over past conversations, skills, schedules. |
| **Approvals** | Every tool has an effect, and tern (not the model) decides from it: `read` never asks, `write` asks unless you chose "always", `destructive` asks every time. Prompts show it, e.g. `write · lua__gmail__send …`. Workspace writes (`~/tern-workspace` by default) and tern's own memory and schedules are pre-granted; reads or writes outside the workspace ask. Shell commands are `destructive` unless an "always" pattern (e.g. `git *`) covers them. Tools flagged `money_movement` or `trade` are refused, and nothing can approve them. |
| **MCP** | Ask the agent ("add the GitHub MCP server"; you approve the exact command each time, and only on this machine for local commands), or add, test, toggle and remove servers with `/mcp` in the chat or [`tern mcp`](#tern-mcp), or edit `[mcp.servers.<name>]` (stdio or streamable HTTP); changes apply without a restart, a server's pasted `mcpServers` JSON works too, and their tools appear as `mcp__<name>__<tool>`. Effects come from the server's `readOnlyHint` / `destructiveHint` (else `write`), and `effects = { tool = "read" }` overrides them. For a browser, see [Recipe: give tern a browser](recipes/browser.md). |
| **Luau connectors** | Small sandboxed scripts in `~/.tern/connectors/*.luau` that declare which hosts and secrets they may use, and an `effect` per tool (`write` if left out). The agent can write and test its own, and activates one only with your approval of its capabilities. `tern connectors` lists them. |
| **Skills** | Folders in `~/.tern/skills/` with a `SKILL.md` (agentskills format). |
| **Telegram** | Chat from your phone, approve tool calls with buttons, get scheduled results. |
| **Schedules** | "Every weekday at 8am, summarize my unread GitHub notifications" becomes a cron schedule whose results are delivered to the chat that asked. Runs with nothing to say stay silent, results wait out your quiet hours, and each schedule can be capped per day. See [Schedules](#schedules). |
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
| [`tern gateway pair`](#tern-gateway-pair) | Pair a Telegram account with the bot. |
| [`tern sessions`](#tern-sessions) | List or show conversations. |
| [`tern runs`](#tern-runs) | List runs or print a run's step journal. |
| [`tern connectors`](#tern-connectors) | List or validate Luau connectors. |
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

- **Scheduler**: checks for due schedules every 30 seconds. A schedule missed while the daemon was down runs once on the next start, then continues from now (no backfill). It also enforces each schedule's caps and sends results held for quiet hours (see [Schedules](#schedules)).
- **Gateways**: Telegram, if `[gateway.telegram]` is enabled (see [Telegram](#telegram)). The daemon refuses to start if the bot token secret isn't set.
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

Prints an 8-character one-time code on stdout. Send it to your Telegram bot within 10 minutes to pair that Telegram account. Only paired accounts can talk to the bot or answer its approval buttons. Works whether or not the daemon is running, but the bot only answers while the daemon runs.

### `tern sessions`

```sh
tern sessions [list]      # the 50 most recently updated: id, time, source, message count, title
tern sessions show <id>   # one conversation's messages
```

A session is a conversation; its source is where it started (`cli`, `tui`, `api` for the daemon's socket, `telegram`, `cron`).

### `tern runs`

```sh
tern runs list            # the 20 most recent runs: id, status, start time, model
tern runs show <run-id>   # run details and its step journal
```

A run is one prompt driven to a final answer within a session. Its journal records each model call and tool result; `runs show` is the quickest way to see what a run did, and where an unfinished one stopped.

### `tern connectors`

```sh
tern connectors [list]     # active connectors and drafts, with capabilities and missing secrets
tern connectors test <name>
```

`list` loads every `.luau` file in `~/.tern/connectors/` (active) and `~/.tern/connectors/drafts/` (written by the agent, not yet activated) and prints each one's description, declared hosts and secrets, its tools with their effect (`- lua__mail__search [read]: … (untrusted)`), and any secrets that aren't set. Files that fail to load are marked `✗` with the error. Active connectors that keep the pre-effects approval default are marked `grandfathered`.

`test <name>` validates `<name>.luau` (active first, then drafts) and lists the tools it exposes, with their effect and flags (untrusted, read via POST, idempotent, requires approval, refused). It exits non-zero if the connector doesn't exist or doesn't load. Neither command writes anything.

A connector tool declares what it does:

```lua
search = { effect = "read", untrusted = true, ... },   -- never asks; returns third-party text
send   = { effect = "write", ... },                    -- the default: asks unless you chose "always"
purge  = { effect = "destructive", ... },              -- asks every time
query  = { effect = "read", read_via_post = true, ... }, -- a read that POSTs (GraphQL, search APIs)
pay    = { money_movement = true, ... },               -- refused; also `trade = true`
```

A `read` tool can only send GET and HEAD unless it sets `read_via_post`; anything else fails before the request goes out. Connectors that were already active when tern gained effects keep their old behaviour (tools without an `effect` don't ask) until you activate them again or edit them.

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
```

`add` and `import` start the server and list its tools before saving (`--no-test` skips that), and ask for missing secrets without echoing them. Values in a pasted snippet that look like keys are stored in `secrets.toml`, never `config.toml`. In the chat, `/mcp` opens the same as a panel (`/mcp add`, `/mcp paste`, `/mcp restart|enable|disable NAME`).

To browse the web, add a browser MCP server; [Recipe: give tern a browser](recipes/browser.md) has the commands, effects and cautions.

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

Checks, in order: the data directory; `config.toml` and `secrets.toml` (including its 0600 permissions); each provider (that its secret is set and, unless `--offline`, that its endpoint answers with that key) and `default_model`; the database's schema version and integrity; whether the daemon is running and what manages it (launchd, systemd, or nothing); schedules paused by a daily cap or whose USD cap can't apply (no known price), and deliveries held for quiet hours; `user.md` and `memory.md` against their caps (over a cap is a warning: only the first part is in the prompt); the sandbox backend (whether the OS sandbox is available for `host`, Docker on `PATH` for `docker`); the Telegram gateway; and each enabled MCP server: its secrets, and that it starts and lists its tools (with `--offline`, only that its command is on `PATH`). Each problem comes with a hint for fixing it.

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
| `default_model` | none | `provider:model` used when `--model` / `/model` isn't given. |
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
| `memory.user_md_tokens` | `1500` | Cap on `user.md` as injected into the prompt (tokens ≈ chars / 4). |
| `memory.memory_md_tokens` | `1500` | Cap on `memory.md` (core facts) as injected. |
| `memory.flush` | `true` | Before older context is summarized, give the agent one silent turn to save durable facts with `memory_save`. |
| `sandbox.backend` | `host` | `host` or `docker`. |
| `sandbox.shell_timeout_secs` | `120` | Per shell command. |
| `sandbox.network` | `true` | IP networking for shell commands (both backends). |
| `sandbox.confine` | `auto` | Host backend: `auto` (sandbox when the OS can, else warn), `required` (refuse to run unconfined), `off`. |
| `sandbox.read_paths` | `["~/.gitconfig", "~/.config/git"]` | Host backend: extra read-only paths. |
| `sandbox.write_paths` | `[]` | Host backend: extra writable paths. |
| `sandbox.docker_image` | `debian:stable-slim` | Image for the `docker` backend. |
| `sandbox.docker_memory` / `docker_cpus` / `docker_pids` | `2g` / `2.0` / `512` | Container limits; empty or 0 for none. |
| `gateway.telegram.enabled` | `false` | |
| `gateway.telegram.bot_token_secret` | | Name of the secret holding the bot token. |
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
| `mcp.servers.<name>.effects` | `{}` | Effect per tool, by the server's tool name (`"*"` for the rest): `{ search_issues = "read", delete_repo = "destructive" }`. Overrides the server's `readOnlyHint` / `destructiveHint`; tools without either are `write`. |
| `secret_request_timeout_secs` | `900` | How long the agent's prompt for a secret waits for you. |

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

## Memory

tern remembers in two places:

- **`user.md` and `memory.md`** in `~/.tern`: what every conversation needs, in the system prompt each time. `user.md` is about you; `memory.md` holds core facts the agent keeps. You can edit both; the agent changes them with `profile_edit` (read, append, or replace to consolidate). Each has a cap, 1500 tokens by default (`[memory]` in config). A write that would go over is refused and the agent is asked to consolidate: merge, drop what's stale, and move details to long-term memory. A file you edit past its cap is cut in the prompt with a note asking the agent to consolidate, and `tern doctor` warns. Sub-agents see both files but can't change them.
- **Long-term memory**: facts saved with `memory_save`, found with `memory_search`, never in the prompt by themselves.

**Flush before compaction.** When a long conversation is about to be summarized, the agent first gets one silent model call over the part being summarized, with only `memory_save`, to keep durable facts (preferences, decisions, people, commitments). It's journaled, so a resumed run doesn't repeat it, and its tokens count toward the run's budget. If it fails, compaction goes ahead. Sub-agents and scheduled runs don't flush, nor does a run whose token budget can't cover it. `memory.flush = false` turns it off.

## Schedules

The agent creates schedules with `schedule_create` and changes them with `schedule_update`; the desktop app calls them routines. Each one runs its prompt in a session of its own and delivers the final answer to its `deliver_to` (a Telegram chat, or nowhere: the result stays in the session).

- **Nothing to say, nothing sent.** A scheduled run is told it may answer exactly `NO_REPLY`. Then nothing is delivered (no Telegram message, no desktop notification), and the run is recorded as completed and *quiet*: `GET /v1/sessions/{id}/runs` has `quiet: true`, `run_finished` events carry `quiet: true`, and the app shows "Last run quiet".
- **Quiet hours.** A result that finishes inside quiet hours (`[schedules] quiet_hours`, default 22:00–07:30 local; per schedule `quiet_hours = "HH:MM-HH:MM"`, `"none"` or `"default"`) is held, not the run: the run happens on time. Held results are kept in the database (a restart keeps them) and sent when the window ends, as one message per chat if there are several. A schedule marked `urgent` delivers at once.
- **Caps.** Per schedule, or as defaults under `[schedules]`: `runs_per_day`, `usd_per_day` and `tokens_per_run`.
  - Daily caps count from local midnight. When a run is due and a cap is reached, the schedule **pauses until the next day** and posts one notice to its delivery target. Resume it sooner from the app (the routine's menu), with `schedule_update` `resume: true`, or `PATCH /v1/schedules/{id}` `{"resume": true}`; counting then starts again from that moment. "Run now" isn't capped.
  - `usd_per_day` needs the model's price, from `providers.<name>.prices` or the provider's model list. Without one the USD cap is ignored; the log says so once, and `tern doctor` lists it.
  - A run that uses more than `tokens_per_run` is stopped and fails with the reason ("stopped: this run used N tokens, over its cap of M"), which is delivered like any failure.
- `tern doctor` reports schedules paused by a cap, USD caps that can't apply, and deliveries waiting for quiet hours to end.
- **Results are posted to a channel** (Home unless the schedule's `channel` says otherwise; `POST`/`PATCH /v1/schedules` take `channel`, a slug or id). A `NO_REPLY` run posts nothing. `deliver_to` stays the push to Telegram.

## Channels

Channels are topic timelines (`#finances`, `#launch`) over one shared memory; see [channels-design.md](channels-design.md). **Home** is the main channel, your direct line to the agent; it always exists and can't be archived.

- A channel's timeline is its **posts**: what you send there, the agent's answers, and routine results. Each post has an `author` to show and a stable `author_id` (`owner` for you, `tern` for the agent, `routine:<schedule id>`), which becomes a member or agent id later. Tool calls stay in the thread's session.
- Sending into a topic channel (`POST /v1/channels/{id}/posts` `{text}`) writes your post and starts a run in the channel's **main thread**; its answer is posted as a reply to yours. With `reply_to` it goes to a **side thread** branched from that post. A channel's `instructions` are added to the system prompt of its runs.
- Home's conversation is its messages, so sending there writes no post; Home's posts are what comes from elsewhere, such as routine results.
- **Home is one continuous chat** cut into sessions behind the scenes, so no context grows forever. A message starts a new session after `[home] idle_hours` (default 6) without one, or once a new day has begun (`day_starts`, default 04:00 local) and the last message is an hour old, or when you ask for a **New topic** (the desktop's button or ⌘N, `new_topic: true` on the API, `/new` in Telegram). Never in the middle of a run. The new session's first run starts with a summary of the last one, what's still open there, and its last two exchanges verbatim; the agent can look further back with `history_search`. The desktop shows the stream as one scroll with a divider at each cut (click it for what was carried over), and **History** lists earlier sessions, side chats and conversations from before channels. Telegram chats follow the same rules, each as its own stream.
- **Sending while tern is working doesn't stop it.** In Home, a channel, or a Telegram chat, the message goes to the run in progress, which reads it at its next step (Stop still stops). `POST /v1/runs` with a `session_id` still replaces the run, as before.
- **Side chats**: "Branch into side chat" on a message in Home opens a conversation beside the stream that quotes it; it shows as a card in the stream and in History, and its context stays separate. API: `POST /v1/runs` with `channel` and `branched_from: "message:<id>"`.
- **In the desktop app**, CHANNELS in the sidebar lists topic channels (bold with a dot when there are new posts, a spinner while tern works there; read state stays on this computer) and **+** makes one (a name, and optional instructions). A channel opens as a timeline: posts with who wrote them (◷ for routines), **Reply in thread** (a side thread in the right pane), **Open thread** (the run behind a post, tool steps and all), and a composer that posts to the channel. The gear edits the title and instructions or archives the channel. A routine's editor chooses the channel it posts to. **Approvals and secret requests are answered in Home** whichever channel asked; the channel shows "Waiting for you in Home".
- `GET /v1/channels/home/stream?before=&limit=` returns the stream's messages, posts and side chats interleaved, newest first, with a `cut` item where each session starts, and `next` to page back.
- API: `GET/POST /v1/channels`, `PATCH /v1/channels/{id}` (`title`, `instructions`, `archived`), `GET /v1/channels/{id}/posts?before=&limit=` (newest first), `POST /v1/channels/{id}/posts`, `GET /v1/sessions?channel=` (a channel's threads), and `channel` on `POST /v1/runs` for a new thread. `/v1/events` adds `{"post_created": {"post": …}}` and `{"channels_changed": {}}`, and run events carry `channel_id`.

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

By default `shell` runs on this machine after an approval prompt, with a scrubbed environment (no secrets), a timeout, and an OS sandbox (Seatbelt on macOS, Landlock on Linux): it can write only to the workspace and temp directories, can't read `~/.ssh`, `~/.tern` or the rest of your home directory, and loses IP networking with `network = false`. Add paths a toolchain needs:

```toml
[sandbox]
read_paths = ["~/.gitconfig", "~/.config/git", "~/.rustup"]
write_paths = ["~/.cargo/registry"]
network = true
```

To run it in a container instead:

```toml
[sandbox]
backend = "docker"
docker_image = "debian:stable-slim"
```

Commands then run in one long-lived container per workspace, with the workspace mounted at `/workspace`, files owned by you, no capabilities, and limits from `docker_memory`, `docker_cpus` and `docker_pids`. What each backend confines, per platform, is in [docs/sandbox-design.md](sandbox-design.md).

## Security model

The threat model, what isn't covered, and how to report a vulnerability are in [SECURITY.md](../SECURITY.md). In short:

- **Secrets** come from the environment or `~/.tern/secrets.toml`, which must be mode 0600. Every secret value tern reads is scrubbed from the journal, conversation history, and connector and MCP server logs, and none reach the shell's environment.
- **File tools** can't modify `~/.tern` and can't read `secrets.toml` or the database. Reads and writes outside the workspace need approval, and paths that escape the workspace through `..` or symlinks are refused.
- **`http_fetch`** refuses loopback, private, link-local, and cloud-metadata addresses, checking every redirect hop and pinning DNS between the check and the connection.
- **Luau connectors** run in sandboxed VMs (no files, processes, or `require`; 5 s and 64 MB per call) and can only reach the hosts and secrets they declare, including across redirects. An activated connector that changes, or that declares different capabilities than you approved, isn't loaded.
- **`shell`** runs in an OS sandbox (Seatbelt on macOS, Landlock on Linux) that confines writes to the workspace and keeps your home directory and `~/.tern` unreadable; see [Sandboxing](#sandboxing).
- **Effects** are enforced by tern, not the model: `destructive` calls ask every time and `money_movement` / `trade` tools are refused outright, even with `--yes`.
- **"Always allow"** is never offered for destructive tools, for commands that run other commands (`bash`, `env`, `sudo`, `find`, interpreters, …) or for activating connectors, and allowlisted shell commands can't chain (`;`, `|`, `$( )`, …).

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

Everything lives in `~/.tern` (`TERN_HOME` to move it): `config.toml`, `secrets.toml`, `tern.db` (SQLite, WAL), `persona.md`, `user.md` and `memory.md` (included in the system prompt; see [Memory](#memory)), `skills/`, `connectors/`, `logs/` (daily rotated; set `TERN_LOG=debug` for more).
