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

`tern run "<prompt>"` runs one prompt non-interactively and prints the answer on stdout (`--yes` auto-approves tools, for scripts).

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

### Chat commands

`/new` · `/model [provider:model]` · `/sessions` · `/resume <session or run id>` · `/tools` · `/stop` · `/reload` · `/usage` · `/help`. Enter sends; Shift+Enter, Alt+Enter, or Ctrl+J adds a newline; Ctrl+O expands tool output; Ctrl+C cancels (twice to quit).

## The daemon and Telegram

`tern daemon` runs the scheduler, gateways, and a local API on `~/.tern/tern.sock` (mode 0600). While it runs, `tern` attaches to it instead of starting its own runtime. Runs in progress when the daemon stops resume when it starts again.

To use Telegram:

1. Create a bot with [@BotFather](https://t.me/BotFather) and set `TELEGRAM_BOT_TOKEN` (env or `secrets.toml`).
2. Add to `config.toml`:
   ```toml
   [gateway.telegram]
   enabled = true
   bot_token_secret = "TELEGRAM_BOT_TOKEN"
   ```
3. Start `tern daemon`, run `tern gateway pair`, and send the printed code to your bot.

Only paired Telegram accounts can talk to the bot or answer its approval buttons.

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
