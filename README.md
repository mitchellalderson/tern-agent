# tern

A personal AI agent shipped as **one static binary**: no Python, no Node, no Docker required. It runs in your terminal and on Telegram, runs scheduled tasks, and survives crashes: every model call and tool result is journaled, so a killed run resumes where it stopped without repeating completed work.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/tern/main/install.sh | TERN_REPO=OWNER/tern sh
```

The script picks the right build (Linux x86_64/aarch64, static musl; macOS Apple Silicon/Intel), verifies its SHA-256 checksum, and installs to `~/.local/bin` (`TERN_INSTALL_DIR` to change). Or build from source with Rust 1.89+:

```sh
cargo build --profile dist -p tern    # → target/dist/tern
```

## Quick start

```sh
tern config init                                           # creates ~/.tern with a commented config
tern config set providers.anthropic.kind anthropic
tern config set providers.anthropic.api_key_secret ANTHROPIC_API_KEY
tern config set default_model anthropic:claude-sonnet-5-5
export ANTHROPIC_API_KEY=sk-ant-...                        # or put it in ~/.tern/secrets.toml (mode 0600)
tern doctor                                                # checks everything and says what's missing
tern                                                       # chat
```

`tern run "<prompt>"` runs one prompt non-interactively and prints the answer on stdout (`--yes` auto-approves tools, for scripts). Every command is described under [Commands](#commands).

## What it can do

| | |
|---|---|
| **Models** | Anthropic, plus any OpenAI-compatible endpoint (OpenRouter, Ollama, vLLM, LM Studio). Models are `provider:model`; switch with `--model` or `/model`. |
| **Tools** | Files (`read_file`, `list_dir`, `search_files`, `write_file`, `edit_file`), `shell`, `http_fetch`, long-term memory, search over past conversations, skills, schedules. |
| **Approvals** | Shell commands, and reads or writes outside the workspace (`~/tern-workspace` by default), ask first: allow once, always (e.g. `git *`), or deny. |
| **MCP** | Configure servers under `[mcp.servers.<name>]` (stdio or streamable HTTP); their tools appear as `mcp__<name>__<tool>`. |
| **Luau connectors** | Small sandboxed scripts in `~/.tern/connectors/*.luau` that declare which hosts and secrets they may use. The agent can write and test its own, and activates one only with your approval of its capabilities. `tern connectors` lists them. |
| **Skills** | Folders in `~/.tern/skills/` with a `SKILL.md` (agentskills format). |
| **Telegram** | Chat from your phone, approve tool calls with buttons, get scheduled results. |
| **Schedules** | "Every weekday at 8am, summarize my unread GitHub notifications" becomes a cron schedule whose results are delivered to the chat that asked. |
| **Durability** | Kill tern mid-run and `tern resume <run>` (or the daemon, automatically) continues it without re-running completed tools. `tern runs show <run>` prints the step journal. |

## Commands

| Command | What it does |
|---|---|
| [`tern`](#tern) | Interactive chat in the terminal. |
| [`tern run`](#tern-run) | Run one prompt non-interactively and print the answer. |
| [`tern resume`](#tern-resume) | Continue an interrupted or failed run. |
| [`tern daemon`](#tern-daemon) | Run the scheduler, gateways, and local API in the foreground. |
| [`tern gateway pair`](#tern-gateway-pair) | Pair a Telegram account with the bot. |
| [`tern sessions`](#tern-sessions) | List or show conversations. |
| [`tern runs`](#tern-runs) | List runs or print a run's step journal. |
| [`tern connectors`](#tern-connectors) | List or validate Luau connectors. |
| [`tern config`](#tern-config) | Create, read, and edit `config.toml`. |
| [`tern doctor`](#tern-doctor) | Check configuration, providers, database, daemon, and integrations. |
| [`tern report`](#tern-report) | Write a redacted bug report. |

`tern help <command>` or `tern <command> --help` prints each command's options; `tern --version` prints the version. Commands exit non-zero on failure and print `error: …` on stderr.

### `tern`

With no subcommand, tern opens the chat TUI. If a daemon is running it attaches to it over `~/.tern/tern.sock`, so the conversation, schedules, and approvals are shared with Telegram; otherwise it starts its own runtime in-process.

Chat commands: `/new` · `/model [provider:model]` · `/sessions` · `/resume <session or run id>` · `/tools` · `/secrets [NAME]` · `/stop` · `/reload` · `/usage` · `/help`.

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

- **Scheduler**: checks for due schedules every 30 seconds. A schedule missed while the daemon was down runs once on the next start, then continues from now (no backfill).
- **Gateways**: Telegram, if `[gateway.telegram]` is enabled (see [Telegram](#telegram)). The daemon refuses to start if the bot token secret isn't set.
- **Local API** on `~/.tern/tern.sock` (mode 0600), which `tern` and the desktop app attach to.
- **Crash recovery**: runs in progress when the daemon stops are left `running` and resume on the next start, exactly as after a crash.

Only one daemon may use a data directory; a second one exits with the first one's pid. `tern doctor` reports whether it's running. To keep it running, start it from your service manager (systemd, launchd) or a terminal multiplexer; logs go to `~/.tern/logs/`.

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

`list` loads every `.luau` file in `~/.tern/connectors/` (active) and `~/.tern/connectors/drafts/` (written by the agent, not yet activated) and prints each one's description, declared hosts and secrets, and any secrets that aren't set. Files that fail to load are marked `✗` with the error.

`test <name>` validates `<name>.luau` (active first, then drafts) and lists the tools it exposes, with their idempotent and requires-approval flags. It exits non-zero if the connector doesn't exist or doesn't load. Neither command writes anything.

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

Checks, in order: the data directory; `config.toml` and `secrets.toml` (including its 0600 permissions); each provider (that its secret is set and, unless `--offline`, that its endpoint answers with that key) and `default_model`; the database's schema version and integrity; whether the daemon is running; the sandbox backend (Docker on `PATH` for `docker`); the Telegram gateway; and each MCP server's command or URL and secrets. Each problem comes with a hint for fixing it.

It exits `1` if anything failed, `0` otherwise (warnings are things not set up yet). It is read-only and doesn't create `~/.tern`.

### `tern report`

```sh
tern report [-o <path> | -o -] [--run <run-id>]
```

See [Troubleshooting](#troubleshooting).

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
| `sandbox.backend` | `host` | `host` or `docker`. |
| `sandbox.shell_timeout_secs` | `120` | Per shell command. |
| `sandbox.docker_image` | `debian:stable-slim` | Image for the `docker` backend. |
| `gateway.telegram.enabled` | `false` | |
| `gateway.telegram.bot_token_secret` | | Name of the secret holding the bot token. |
| `mcp.servers.<name>.transport` | | `stdio` or `http` (streamable HTTP). |
| `mcp.servers.<name>.command` / `args` | | Executable and arguments, for `stdio`. |
| `mcp.servers.<name>.url` | | Endpoint, for `http`. |
| `mcp.servers.<name>.env_secrets` | `[]` | Secrets passed to the server's environment. |
| `mcp.servers.<name>.idempotent` | `false` | Its tools are safe to re-run when resuming. |
| `mcp.servers.<name>.requires_approval` | `false` | Ask before each of its tool calls. |

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

Environment variables: `TERN_HOME` (data directory, default `~/.tern`) and `TERN_LOG` (log level, e.g. `debug`).

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

Restart `tern daemon`, then run `tern devices pair` (default scope `chat`: conversations and approvals, but not config, secrets or devices). It prints a QR code and a `tern://pair?…` link valid for 10 minutes. On another machine, `tern devices join '<link>'` prints a token; then `TERN_REMOTE_TOKEN=<token> tern --remote https://host:7420 --pin sha256:…` attaches the chat UI. `tern devices list` and `tern devices revoke <id>` manage devices; revoking signs the device out at once.

## Sandboxing

By default `shell` runs on this machine with a scrubbed environment (no secrets), a timeout, and an approval prompt. To run it in a container instead:

```toml
[sandbox]
backend = "docker"
docker_image = "debian:stable-slim"
```

Commands then run in one long-lived container per workspace, with the workspace mounted at `/workspace` and files owned by you.

## Security model

- **Secrets** come from the environment or `~/.tern/secrets.toml`, which must be mode 0600. Every secret value tern reads is scrubbed from the journal, conversation history, and connector and MCP server logs, and none reach the shell's environment.
- **File tools** can't modify `~/.tern` and can't read `secrets.toml` or the database. Reads and writes outside the workspace need approval, and paths that escape the workspace through `..` or symlinks are refused.
- **`http_fetch`** refuses loopback, private, link-local, and cloud-metadata addresses, checking every redirect hop and pinning DNS between the check and the connection.
- **Luau connectors** run in sandboxed VMs (no files, processes, or `require`; 5 s and 64 MB per call) and can only reach the hosts and secrets they declare, including across redirects. An activated connector that changes, or that declares different capabilities than you approved, isn't loaded.
- **"Always allow"** is never offered for commands that run other commands (`bash`, `env`, `sudo`, `find`, interpreters, …) or for activating connectors, and allowlisted shell commands can't chain (`;`, `|`, `$( )`, …).

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

Everything lives in `~/.tern` (`TERN_HOME` to move it): `config.toml`, `secrets.toml`, `tern.db` (SQLite, WAL), `persona.md` and `user.md` (included in the system prompt), `skills/`, `connectors/`, `logs/` (daily rotated; set `TERN_LOG=debug` for more).

## Development

```sh
cargo test --workspace                          # unit and end-to-end tests (mock model, no network)
python3 scripts/tui-smoke.py target/debug/tern  # drives the real TUI in a pseudo-terminal
cargo test -p tern-sandbox -- --ignored real_docker      # needs Docker
TERN_SMOKE_MODEL=anthropic:claude-sonnet-5-5 ANTHROPIC_API_KEY=... cargo test -p tern --test m1 -- --ignored real_provider
```

`cargo build --release` is the fast optimized build (parallel codegen); `--profile dist` is the smaller, slower one used for shipped binaries. End-to-end tests use a scripted mock provider (`TERN_PROVIDER=mock:<script.json>`), a stdlib-only Python MCP server, and a fake Telegram Bot API. Design decisions and their reasons are in [`DECISIONS.md`](DECISIONS.md).
