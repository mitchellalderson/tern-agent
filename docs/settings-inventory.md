# Platform settings inventory

Every platform-wide setting tern has, where it can be edited, and where it
lives in the single Settings surface (desktop Settings window, TUI
`/settings`). **Agent settings are not here**: an agent's model, tools,
autonomy, persona and channels stay in the agent sheet (`/agents`, desktop
agent sheet) and are edited exactly as before.

Columns: **Where today** is where a user could change it before the unified
surface (`config` = only by editing `~/.tern/config.toml` or `tern config
set`; `desktop` = the desktop's old Configuration/Settings/Providers/Secrets
dialogs; `TUI` = a slash command). **Restart** means a saved change only
applies after tern restarts. **Tier**: *user* = shown in the first screens,
*adv* = tuning knob, shown under Advanced or behind "show advanced".
Defaults are `Config::default()`.

Settings in `config.toml` are described by the daemon itself
(`GET /v1/config/settings`: key, type, default, choices, range, restart,
security note, `group`, `advanced`), so the desktop and the TUI render the
same list and a new config field appears in both without UI changes.

## Groups in the Settings surface

| Section | Contents |
|---|---|
| General | workspace, quiet hours, initiative, Home, memory and skills switches; desktop: launch at login, close behaviour; where tern runs |
| Models & Providers | providers and API keys (the providers flow), default model and fallback chain, sub-agent default model |
| Secrets | the secrets editor (names and set/unset only; values are write-only) |
| Permissions & Sandbox | default autonomy, taint egress, sandbox, scripts, bundles, outbox; security audit |
| Limits & Budgets | iteration caps, spend/token budgets, sub-agent limits, timeouts, rate limits |
| Servers | desktop's saved servers; API listeners, webhooks, devices |
| MCP & gateways | MCP servers (own editor), Telegram/gateway connectors (own editor) |
| Advanced | everything marked *adv*, plus the raw file (config.toml path, reveal, reload) |
| About | app and core version, data location, config path, log location |

## `config.toml`: scalar settings (daemon API)

| Key | Type | Default | Where today | Restart | Secret | Group | Tier |
|---|---|---|---|---|---|---|---|
| `workspace` | text | `~/tern-workspace` | config, desktop | yes | no | general | user |
| `max_iterations` | int | 40 | config, desktop | yes | no | limits | user |
| `show_reasoning` | bool | true | config, desktop | yes | no | general | user |
| `secret_request_timeout_secs` | int | 900 | config, desktop | yes | no | advanced | adv |
| `autonomy` | choice readonly/supervised/autonomous/yolo | supervised | config, desktop (shown as text) | yes | no | permissions | user |
| `default_model` | model chain | unset | `/model`, `/setup`, desktop providers + config | no (live) | no | models | user |
| `context_window` | int | 128000 | config, desktop | yes | no | models | adv |
| `sandbox.backend` | choice host/docker | host | config, desktop | yes | no | permissions | user |
| `sandbox.shell_timeout_secs` | int | 120 | config, desktop | yes | no | limits | user |
| `sandbox.network` | bool | false | config, desktop | yes | no | permissions | user |
| `sandbox.confine` | choice auto/required/off | auto | config, desktop | yes | no | permissions | user |
| `sandbox.read_paths` | list | `~/.gitconfig`, `~/.config/git` | config, desktop | yes | no | permissions | user |
| `sandbox.write_paths` | list | `[]` | config, desktop | yes | no | permissions | user |
| `sandbox.docker_image` / `docker_memory` / `docker_cpus` / `docker_pids` | text/float/int | debian:stable-slim / 2g / 2 / 512 | config, desktop | yes | no | limits | adv |
| `subagents.enabled` | bool | true | config, desktop | yes | no | permissions | adv |
| `subagents.default_model` | model chain | unset | config, desktop | yes | no | models | user |
| `subagents.models` | table of chains | `{}` | config only | yes | no | advanced | adv (not editable in UI) |
| `subagents.max_depth` | int 1..3 | 1 | config, desktop | yes | no | limits | user |
| `subagents.max_concurrent` | int | 4 | config, desktop | yes | no | limits | user |
| `subagents.budget_tokens` | int | 400000 | config, desktop | yes | no | limits | user |
| `subagents.max_result_chars` | int >= 200 | 8000 | config, desktop | yes | no | limits | user |
| `subagents.max_children` / `max_concurrent_global` / `max_iterations` / `max_iterations_ceiling` / `budget_tokens_ceiling` / `ask_parent_timeout_secs` | int | 12 / 8 / 30 / 100 / 2000000 / 900 | config, desktop | yes | no | limits | adv |
| `schedules.quiet_hours` | text `HH:MM-HH:MM` | 22:00-07:30 | config, desktop | no | no | general | user |
| `schedules.runs_per_day` | int, optional | unset | config, desktop | no | no | limits | user |
| `schedules.usd_per_day` | float, optional | unset | config, desktop | no | no | limits | user |
| `schedules.tokens_per_run` | int, optional | unset | config, desktop | no | no | limits | user |
| `schedules.max_events_per_poll` | int | 20 | config, desktop | no | no | limits | adv |
| `schedules.initiative` | choice silent/digest/notify/interrupt | digest | config, desktop | no | no | general | user |
| `schedules.initiative_dial` | choice vacation/quieter/normal/louder | normal | config, desktop | no | no | general | user |
| `schedules.digest_times` | list | 08:30, 17:30 | config, desktop | no | no | general | user |
| `schedules.gate_model` | model chain | unset | config, desktop | no | no | models | adv |
| `memory.user_md_tokens` / `memory_md_tokens` | int | 1500 | config, desktop | yes | no | general | adv |
| `memory.flush` | bool | true | config, desktop | yes | no | general | user |
| `home.idle_hours` | float | 6 | config, desktop | no | no | general | user |
| `home.day_starts` | text | 04:00 | config, desktop | no | no | general | user |
| `skills.capture` | bool | true | config, desktop | yes | no | general | user |
| `skills.capture_min_tool_calls` | int | 8 | config, desktop | yes | no | general | adv |
| `outbox.wait_secs` / `expire_hours` | int | 120 / 72 | config, desktop | yes | no | permissions | adv |
| `taint.egress` | choice ask/allow | ask | config, desktop | no | no | permissions | user |
| `taint.workspace_writes` | bool | true | config, desktop | yes | no | permissions | user |
| `fetch.taint` | choice strict/legacy | strict | config, desktop | no | no | permissions | user |
| `fetch.allow_domains` / `deny_domains` | list | `[]` / `[]` | config, desktop | no | no | permissions | user |
| `fetch.model` | model | unset (cheapest configured) | config, desktop | no | no | models | adv |
| `fetch.ask_when_tainted` | bool | false | config, desktop | no | no | permissions | adv |
| `fetch.max_bytes` / `max_pages` / `max_redirects` / `timeout_secs` / `max_iterations` / `budget_tokens` / `spend_cap_usd` / `result_max_bytes` / `extract_text_bytes` / `cache_secs` / `max_calls_per_run` / `max_concurrent` | int/float | 2 MiB / 3 / 5 / 30 / 3 / 30000 / 0.02 / 8192 / 6144 / 900 / 40 / 4 | config, desktop | no | no | limits | adv |
| `connectors.rate` | text | 120/min | config, desktop | yes | no | limits | adv |
| `connectors.record_tests` / `openapi_tools` / `openapi_max_tools` | bool/int | true / 8 / 40 | config, desktop | yes | no | advanced | adv |
| `auth.sign_in_timeout_secs` / `max_connections` / `idle_timeout_secs` / `local_issuers` / `client_metadata_url` | int/list/text | 300 / 10 / 600 / `[]` / "" | config, desktop | yes | no | advanced | adv |
| `auth.clients` | table | `{}` | config only | yes | `client_secret_secret` is a secret *name* | advanced | adv (not editable in UI) |
| `bundles.require_signed` | bool | false | config, desktop | no | no | permissions | user |
| `bundles.allow_private_hosts` | bool | false | config, desktop | no | no | permissions | adv |
| `python.enabled` | bool | true | config, desktop | yes | no | permissions | adv |
| `python.allow_writes` | bool | false | config, desktop | yes | no | permissions | user |
| `python.max_*` (seconds, turn, wall, memory, host calls, parallel, asks, writes) | int | 30/10/600/256/200/4/3/10 | config, desktop | yes | no | limits | adv |
| `webhooks.listen` / `public_url` | list/text | `[]` / unset | config, desktop | yes | no | servers | user |
| `webhooks.tls` | choice | unset | config, desktop | yes | no | servers | adv |
| `webhooks.max_body_kb` / `per_minute` / `tolerance_secs` / `body_timeout_secs` | int | 256 / 30 / 300 / 30 | config, desktop | no | no | limits | adv |
| `webhooks.header_timeout_secs` / `max_connections` | int | 10 / 64 | config, desktop | yes | no | limits | adv |
| `api.listen` | list | `unix` | config, desktop | yes | no | servers | user |
| `api.public_url` | text | unset | config, desktop | yes | no | servers | user |
| `api.tls` / `tls_cert` / `tls_key` / `trust_proxy` | choice/text/bool | auto / unset / unset / false | config, desktop | yes | no | servers | adv |
| `api.remote_mcp_stdio` | bool | false | config, desktop | no | no | servers | adv |

## `config.toml`: tables with their own editors

| Setting | Where today | Restart | Secret | Surface |
|---|---|---|---|---|
| `[providers.*]` (kind, base_url, context_window, prices, `api_key_secret` name) | `/setup`, `tern providers`, desktop Providers dialog | no (reload) | key is a secret, stored in `secrets.toml`, write-only | Models & Providers |
| `[mcp.servers.*]`, `[mcp.serve]` | `/mcp`, `tern mcp`, desktop MCP | no | env/bearer/header values are secrets | MCP & gateways |
| `[gateway.telegram]`, `[gateway.connectors.*]` | `tern gateway pair`, desktop Connections, config | yes | bot token is a secret | MCP & gateways |
| `[agents.*]` model aliases / chains | agent sheet, config | yes | no | **agent settings: unchanged** |

## Not in `config.toml`

| Setting | Where stored | Where today | Restart | Secret | Surface |
|---|---|---|---|---|---|
| Secrets (API keys, tokens) | `~/.tern/secrets.toml` (0600) or env | `/secrets`, `tern providers add` (keys), desktop Secrets dialog | no | yes: names listed, values write-only, never returned | Secrets |
| Launch at login | OS login item via `tern service` | desktop Settings, `tern service install` | no | no | General (desktop) |
| Close window keeps tern running / quits | desktop `desktop.json` (`on_close`) | desktop Settings | no | no | General (desktop) |
| Tray icon | `tern tray` (macOS menu bar, follows login item) | `tern tray` | no | no | General (desktop, explained) |
| Saved servers and the active one | desktop `desktop.json`; tokens in the OS keychain | desktop Servers dialog | no | tokens: yes, never shown | Servers (desktop) |
| Where tern runs (this computer / a server) | `desktop.json` | desktop Servers dialog | reconnects | no | General / Servers |
| Data location (`TERN_HOME`, default `~/.tern`) | environment variable | env only | yes | no | About (shown, not editable) |
| Log level (`TERN_LOG`, default `info`) | environment variable | env only | yes | no | About (shown, not editable); logs are in `<data>/logs`, 14 daily files |
| Remote attach (`TERN_REMOTE_TOKEN`, `--remote`) | env / CLI flags | CLI | n/a | yes | not a setting |
| Devices (paired phones/laptops) | daemon DB | `tern devices`, desktop Security | no | tokens: yes | Servers & devices (listed) |
| Security audit | derived | `tern security`, desktop Settings | n/a | no | Permissions & Sandbox |
| Telemetry, usage reporting | none exist: tern sends no telemetry | n/a | n/a | n/a | About states this |
| Automatic updates | none exist: updates are a reinstall (`install.sh` or the desktop download) | n/a | n/a | n/a | About states the version |

## Rules the surface follows

- One read/write path for config: `GET/PUT/DELETE /v1/config/settings[/{key}]`.
  Writes edit `config.toml` in place (comments and layout kept), refuse a
  value the config would not load with, and write atomically; nothing is
  written on refusal.
- Secrets are never returned. `*_secret(s)` config keys only name a secret and
  refuse writes; values go through `/v1/secrets` (write-only, masked status).
- `restart_required` is per setting, and the view reports whether any saved
  setting is still waiting on a restart.
- Owner-scope devices only: chat and read devices get 403.
