# Changelog

All notable changes to tern are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Versions with a hyphen (`-alpha.N`,
`-beta.N`, `-rc.N`) are GitHub pre-releases.

Entries go in `changelog.d/` (see its README); the release gathers them here.

## [Unreleased]

## [0.1.0-alpha.8] - 2026-11-18

### Added

- **Agents and channels from chat.** An agent can create, edit and delete agents (`agent_create`, `agent_edit`, `agent_delete`) and channels (`channel_create`, `channel_edit`, `channel_delete`). A new agent writes its starting role from its name. You approve each change, and deletes always ask. Power only narrows: an agent never gives another a tool, connector or autonomy ceiling it doesn't have, and never changes an agent that can do more than it can. Tool names are forgiving (`Read File`, `read-file`); a refusal is final, so the model doesn't retry it.
- **New since you left**: Home, DMs and channels mark what arrived while you were away, and open there when there's a lot to catch up on.
- **Composio connector.** One API key, hundreds of apps: search tools, sign in through a card, run reads without asking, writes with a prompt, destructive tools every time. Payment apps are read-only. See `docs/recipes/composio.md`.
- **`offer_connection`.** Instead of "I can't", a card offers Composio sign-in, an MCP server or a built connector, once per app per conversation.
- **Connecting order in the prompt.** Composio, official MCP with OAuth, connector with OAuth, app-only OAuth, API key; never a pasted short-lived token.
- **Evidence on the activation card:** every test of the exact version (input, each HTTP call with status or "Refused", output or error, logs) and the tools no test touched.
- **Connector sign-in links** show as a card with the real domain and a live expiry. A tool returns `sign_in = {url, label, expires_at}`; tern checks it is https, has no embedded credentials and is on a declared host's domain, and the model never sees the URL.
- **Three models:** agent, fast (`fast_model`, background writing) and decision (`decision_model`, quick choices). Quick choices go through one typed `decide()` call, on OpenRouter's Decisions API when available, else the fast model with a retry; a refusal is never read as "no". Channel routing and the notification gate use it. Reasoning is off for quick calls.
- **Run on this Mac.** tern detects Ollama and always reuses the one that is running. Without it, the first-run screen links to the installer or downloads it for you, but only after you agree: the download is checked against GitHub's checksum and Apple's signature first. tern can start Ollama itself (loopback only, long context and keep-alive) and stops it with the daemon; an Ollama you run yourself is left as is. New `/v1/local/status`, `/models`, `/start`, `/stop` and `/install`.
- **A local model per tier.** The Local models dialog recommends an agent, fast and decision model for your Mac's memory, lets you use any installed model for a tier (a model that can't use tools can't be the agent or decision model), and can download and set up all three at once. `tern local recommended|use|setup`.
- **Local models** in the desktop app and the CLI. A dialog lists what's installed (tool support, in memory, and a warning when a model can't use tools or may not fit this Mac), downloads by name with progress and cancel, checks disk space first, and removes models. `tern local status|list|pull|rm|start|stop`. Downloads run in the daemon, so they go on when the window closes.
- **Memory tidy-up**: a weekly pass merges duplicate memories and flags contradictions, logged with Undo in Memory.
- **New topic?** hint in Home and DMs when the subject clearly changes: one quick yes/no per message, one hint per conversation, and it never switches by itself.
- **App-only OAuth (client credentials)** for connectors: set `app_only = true` on an `[auth.clients]` app that has a client secret; no sign-in, and tern fetches a new token when it expires.
- **Follow-ups**: "I'll check back Friday" in a reply becomes a tracked follow-up the agent wakes for. Shown above the composer and in Tasks, and you can cancel it. Time waits now last up to 30 days.
- **Proactive help**, each a dismissible card in the agent's DM with its own `[proactive]` switch (on by default) and a hard cap, and no model call when nothing changed:
  a daily check of routines, sign-ins and waits; a proposed fix for a routine that failed twice or reported nothing three times in a row (never applied silently);
  a "Make this a routine?" suggestion after the same ask three times in 14 days; and "While you were away" on the first desktop open of the day after 8 hours or more.
- **Always allowed** in desktop Settings › Permissions lists the standing grants and revokes them.
- **Remembering**: durable facts in your messages that the agent missed are saved (up to three each), with a card to forget them.
- **Better channel routing:** the router reads each agent's one-line description (written once from its role, editable in the agent sheet), never its persona, and sees the last three posts, so a reply to an agent's question goes back to it. "Sent to @x" now says why.
- **Reply when relevant** per agent (off: only when @mentioned), **One agent / Every relevant agent** per channel (up to 3), and an optional plain-English **reply rule**. The desktop app shows "Choosing who replies…".
- **Team tools for agents.** `delegate_team` splits a question across two or three teammates' lanes and brings their reports back together under one "Handed to Scout and Research" card. `brief_team` tells teammates about a change of plan: they read it at their next run, or are woken with `now` or `want_reply`. `inspect_work` lets an agent look at its own runs (steps, timings, failures, hand-offs, models, tokens). `read_report` reads on past a cut report.
- **A delegate's answer is a "Message from Scout" card** in the chat of the agent that asked it.

### Changed

- **The default agent is a Chief of Staff.** New installs name it Chief (`@chief`); an existing install keeps its name and persona. It plans, hands work to the teammate whose job it is, and tells the team when a plan changes.
- **A channel's lead is told who is in which lane.** It hands the parts of a question that spans lanes to those teammates with `delegate_team`, does its own, and answers once with credit. Simple questions stay with one agent.
- **Connector errors say what to do.** A rejected API key (401/403) tells the agent to ask for a replacement with `secret_request`; a rate limit says to wait. Failed spec fetches say what to check.
- **`tern setup` and `/setup` lead with Ollama too.** On first run the terminal setup offers "Set up with Ollama" (installs it if needed, starts it, downloads a starter model sized to your Mac, makes it the default), with `a` for "Use an API model instead", which opens the usual provider list unchanged. It needs the background agent.
- **First run leads with Ollama.** One prompt, "Set up with Ollama", installs it if needed, downloads a starter model sized to your Mac and makes it the default. "Use an API model instead" opens the usual provider setup. tern starts Ollama itself, with no prompt, at startup and before a run that uses it.
- `schedules.gate_model` is now `decision_model`. Existing config files keep working: the old key is read and moved on load.
- **"Always allow this" is learned, not on every card.** After you choose Allow once three times in 14 days for the same agent, tool and target, the approval card offers it and says why. Never for destructive calls, tainted runs, routine runs, or shell commands that remove, move, escalate or reach the network. `[proactive] learn_approvals = false` brings back "Always" on every card.
- **Routine results now notify on this computer.** A routine's `deliver_to` is a local target (`home`, `dm`, `channel:<id>`, or `none`; omitted means where the routine lives). Initiative levels, quiet hours, digests and held deliveries work as before: digests are posted as a note from tern, and notify/interrupt raise a desktop notification. Old gateway targets (`telegram:123`) become `home`.
- Reports that arrive together (`agent_wait`, `delegate_team`) share one 24,000-character budget instead of 8,000 each, so every report still arrives. A hand-off that stopped, failed or was stopped by the user now tells its caller what got done and what to do next.

### Deprecated

- **`capabilities.secrets` (the script reads the value) is deprecated.** The summary says so; use `capabilities.auth`, which attaches the credential host-side.

### Removed

- **Messaging gateways.** tern is local-only: Telegram, ntfy and Slack gateways are gone, with `tern gateway`, `tern agents telegram`, per-agent bots, pairing codes and the `[gateway]` config (ignored with a warning). Approvals happen in the desktop app and the TUI.
- **Remote access, accounts and hosted tern.** tern is local-only and single-player: the daemon listens only on its Unix socket. Removed the TCP/TLS listener, device pairing and tokens (`tern devices`, `tern --remote`), Clerk account sign-in (`tern account`), the `[api]` and `[account]` config sections, and the desktop app's server switcher and "where to run" choice. The update drops the device tables. Old `[api]` and `[account]` sections are ignored with a warning.
- **Webhook routines.** tern is local-only now, so nothing outside this machine can start a routine: `tern hooks`, the `[webhooks]` listener and `/v1/webhooks` are gone, and existing webhook routines are deleted by the update. Schedule and watch routines are unchanged. An old `[webhooks]` section in `config.toml` is ignored with a warning.

### Fixed

- **Channel catch-up no longer loses a trailing question:** the newest three posts are shown in full (up to 2,000 characters), older ones at 500, and run byproducts such as "memory saved" and "handed to" notes are left out.
- **Ollama** now reports what its models really get. tern reads each model's context and tool support from Ollama, sizes prompts to the context in use instead of assuming 32k (Ollama silently cut longer prompts), warns under 64k and for an Ollama that is too old, keeps the model loaded for 30 minutes, and no longer sends `tool_choice` (which Ollama ignores).
- **Answers written next to a tool call** are no longer lost behind a short afterword like "I saved the details".
- **Stopped runs stay stopped**; steps cut off by a stop show as interrupted, and leftovers are closed at startup.
- **Failed runs** keep a plain-sentence error instead of the raw provider text.
- **Taint and the autonomy ceiling** carry across the runs of a conversation.
- **The agent roster** is ordered by name, so the prompt stays cacheable.
- **"Working for…"** updates when a parent is stopped while a delegate works.

### Security

- **Connector hosts take no wildcards:** `capabilities.http` entries like `*.example.com` are refused; list each host.
- **OAuth sign-in state** is single-use and expires after 15 minutes.
- **Secrets are encrypted at rest.** Values in `secrets.toml` are sealed with AES-256-GCM, bound to their names; the key is in the macOS keychain (or `secrets.key` where there is none). Existing plaintext is migrated automatically.
- **Redaction catches echoed fragments.** A service that echoes part of a key (`ck_**3pNe`, or just its first 8 or last 4 characters) no longer leaks it to the agent, the journal or connector logs.

## [0.1.0-alpha.7] - 2026-11-13

Agents can write documents you read in a new Documents tab. The desktop app starts tern again on a fresh macOS install, and long chat messages no longer jump back while you read.

### Added

- **Documents.** Agents can write a document (`create_document`, `edit_document`, `read_document`, `list_documents`) as a real Markdown file in the workspace, with a revision for every change. A new **Documents** tab in the side panel shows them rendered, in tabs, with a history of revisions. Read-only for now. Links ask first, images never load, and a document written from web content is marked.

### Fixed

- **`tern start` on macOS** no longer waits out 15 s when launchd can't start tern (a quarantined, un-notarized app). It starts tern directly instead, and if that fails it says macOS blocked it rather than pointing at a log that doesn't exist.
- Chat: reading down to the end of a long message no longer jumps back to its start, and "Jump to latest" reaches the real bottom.

## [0.1.0-alpha.6] - 2026-11-10

Prompt caching now works on long sessions (lower cost), and compaction reuses the cached conversation.

### Changed

- **Compaction reads the conversation from the prompt cache** on Anthropic. The summary (and the memory flush before it) is now a fork of the agent's own request: same tools, system prompt and history, with the summarize instruction last, so the cached history is read at 0.1× instead of being sent again as a transcript at full price. Other providers, and prefixes under the model's minimum, keep the transcript request; so does any fork that fails.

### Fixed

- **Attached TUI** no longer sometimes drops a run's final answer when it arrives just after the run ends.
- **Prompt caching** now works across turns and runs on Anthropic. The conversation history is cached (it wasn't before), the current time no longer sits in the system prompt (it changed every run and broke the cache), and breakpoints are skipped on prompts under the model's minimum. `providers.<name>.cache_ttl = "1h"` keeps the system prompt and tools cached for an hour. Anthropic models via OpenRouter are cached too. Cache reads and writes are priced at Anthropic's rates (0.1×, 1.25×, 2×) unless you set them, and `/usage` and the status line show how much input came from the cache.
- **Cached tokens on OpenAI-compatible providers** are read from the response (OpenAI, OpenRouter, Gemini's compatible endpoint, DeepSeek, Moonshot) and no longer counted twice in input. `providers.<name>.prompt_cache_key` sends OpenAI's cache routing hint (on for api.openai.com by default).

## [0.1.0-alpha.5] - 2026-11-10

New installs start at `autonomous`, you can add many agents to a channel at once, and two chat and role fixes.

### Added

- **Add many agents to a channel at once.** "+ Agent" opens a searchable checklist with Select all and Clear, members shown checked and disabled, and an "Add 3 agents" button. New channels can pick their agents up-front. The daemon's `POST /v1/channels/{id}/agents` also takes `{agents: [..]}` and reports who joined, who was already in and who failed.

### Changed

- **New installs start at `autonomous`** (was `supervised`). Ordinary writes and sandboxed commands no longer ask by default; the OS sandbox stays on, an unsandboxed shell is still refused, and destructive calls, sends and anything after reading untrusted text still ask. Starter agents now ship at `autonomous` (look-only ones stay `readonly`). Set `autonomy = "supervised"` in `config.toml`, in Settings, or on an agent to be asked about every write. A `config.toml` with no `autonomy` line and every saved agent keep their level. `yolo` stays opt-in.
- **Fetching defaults documented**: `http_fetch` reads any public site unless `[fetch] allow_domains` is set; private, loopback and metadata addresses stay refused.

### Fixed

- **Chat holds its place for a long reply.** A tall new agent message now stays anchored at its start instead of jumping to its end, in channels and direct chat; a reader scrolled up is never moved.
- **Self-written roles** now work from a name alone: a new agent writes its starter role right after it is created, without waiting for a first message. Re-infer, and the role itself, also draw on the agent's work in channels (not only direct chats), and Re-infer always answers: progress, the new role, a note, or an error.

## [0.1.0-alpha.4] - 2026-11-10

Agents that write their own role, optional hosted accounts, and two fixes. alpha.3 was tagged but never published; its fix is here.

### Added

- **Create an agent with just a name.** It works out its own role, refines it over its first 3 messages, then keeps it. See and edit it under **Role** in the agent sheet, or **Re-infer** it. Agents with a persona are unchanged.
- **Hosted tern accounts (Clerk).** Off unless configured: an `[account]` section lets a server core accept its owner's sign-in, `tern account status` checks it, and the app gets a **Sign in** choice and a "Signed in as" row. Local use is unchanged. See `docs/recipes/clerk-setup.md`.

### Fixed

- **A delegation post no longer sticks at "running"** when several tasks finish at once.
- **Show more** no longer jumps the chat view. The text stays where you clicked.

## [0.1.0-alpha.2] - 2026-11-09

Fewer approval prompts, a calmer desktop app, and agents that can wait for something and continue.

### Added

- **Reasoning view**: when a model returns its reasoning (Anthropic thinking, OpenAI-compatible `reasoning_content` /
  `reasoning`), a chevron beside "Thinking" opens a quiet box that streams it live. The message keeps a collapsed
  "Thought for 12s" note. No chevron when there is none; redacted reasoning reads "Reasoning hidden". Turn it off with
  `show_reasoning = false`. Never sent to channels.
- **Uninstall from the app**: Settings › About › **Uninstall tern…** stops tern and removes its login items (and, if you tick it, deletes your data), so removing tern on macOS no longer needs a terminal.
- **Wait until**: an agent can say "watch this and continue here when it's done" (`wait_until`, with `wait_list` and `wait_cancel`). tern polls a connector read or a public JSON URL with no model call, then continues the same conversation with the result when it matches or times out. Shown as "Waiting: …" in Tasks and the conversation, with Cancel. Up to 5 per agent (`schedules.max_waits`). Recipe: waiting for a GitHub Actions run.

### Changed

- **`http_fetch` returns data only by default.** With a URL and no `want`, the result is title, meta, headings, tables,
  selector hits and links, with no page prose, so the calling run stays untainted and later writes don't ask. Ask for
  `want: ["text"]` (or a `question`, for `answer`) when you need the prose; that taints the run as before. The tool
  description and four starter personas say so.
- **Writes inside the agent's own workspace no longer ask just because the run is tainted.** `write_file` and
  `edit_file` whose target resolves inside the run's folder (canonical path, no `..`, no symlink on the way, not a
  protected file) go through after the run read untrusted content; each is recorded in the ledger with the taint
  (`tainted-write-in-workspace`). Shell, channel posts, connector and MCP writes, memory, network reads of new sites
  and anything outside the folder still ask. Turn it off with `[taint] workspace_writes = false`.
- **Read-only shell pipelines are a read.** A command that is only a pipeline of plain reading programs (`cat`, `head`,
  `tail`, `od`, `hexdump`, `wc`, `grep` without `-r`, `ls`, `stat`, `cut`, `tr`, `nl`, `tac`, `rev`), with no redirect,
  `$()`/backticks, variable, glob, chaining or `-f`, and every operand inside the agent's workspace (symlinks and `..`
  resolved, protected files excluded), is classified `read` in a verified sandbox: it no longer shows as "destructive ·
  shell" and doesn't ask, including in a tainted run. Anything else, including any doubt, asks as before.
- **Autonomy ceiling picker** in the agent sheet reads as a scale from low to high (Read only, Ask first, Code, Mostly on its own, Full), with the raw level names beside each, and "Use my global setting" set apart from the scale. Copy only: levels and config values are unchanged.
- **Changelog fragments**: add entries as files in `changelog.d/`, not in `CHANGELOG.md`; `cargo xtask changelog <version>` gathers them at release.
- The first-launch screen is a calm welcome: **Start on this Mac** first, **Connect to a remote server** second, and the socket path and OS error sit behind **Details**.
- Side panel Memory is quieter: a collapsed "Memory · N" row like Tools, with one scope menu in place of tabs.
- Your posts in Home and channels sit on the same soft `sand-deep` tint as your messages in a conversation, so a thread is easy to scan.
- **Higher token and context limits.** Each model's own window and output limit are used when known, instead of a flat
  128k / 8192. Fallbacks: 200k context, 32k output; local servers keep 32k context. New `max_tokens` setting. Compaction
  starts at 80% (was 75%); summaries get twice the room.

### Fixed

- Agent messages no longer show literal markup such as `**$ </p`: HTML tags are dropped, a bold or code span cut by a
  preview is closed, and Home pointers are cut at a word instead of mid-tag.
- **Stale queued edits:** an `edit_file` or `write_file` waiting in the outbox now remembers the file. If the file changed before you approve it, it is skipped with one line ("a.md changed since this was queued."), not run to fail. Stale items offer **Discard**. Edits to one file run in queue order, and a batch of skipped edits is one message.

## [0.1.0-alpha.1] - 2026-11-06

The first pre-release. Expect rough edges; the macOS app is ad-hoc signed and not notarized (first open needs
System Settings > Privacy & Security > **Open Anyway**), and the CLI tarballs are unsigned (each has a `.sha256` and a
build attestation).

### Added

- **One static binary** for macOS and Linux (x86_64 and aarch64): terminal chat, `tern run`, a background daemon
  (`tern start`, `tern service install`) with a local API, scheduler, and a menu-bar icon on macOS and a tray icon on
  Linux. `install.sh` installs it and verifies the checksum.
- **Crash-safe runs**: a step journal resumes interrupted runs without repeating completed tool calls.
- **Models**: Anthropic and any OpenAI-compatible endpoint, with presets for OpenAI, OpenRouter, Gemini, Groq, Together,
  Ollama and LM Studio, guided `tern setup`, and fallback chains when a model fails.
- **Agents of your own**: persona, model list, tool scope, private or shared memory, routines and a DM; 25 starter
  agents with conservative autonomy ceilings; agents hand work to each other, share topic channels and wake on
  `@mention`; sub-agents for delegation and fan-out.
- **Safety and control**: effects on every tool enforced by the host, approvals as cards (once, this run, this
  conversation, or always, for exactly what you were shown), an outbox for unattended writes, untrusted-content
  tracking, an OS sandbox and Docker option for `shell`, and a hash-chained activity ledger with `tern security audit`.
- **Routines**: schedules, cron and one-off triggers, webhooks, watches that remember where they left off, quiet hours
  and budgets; skill capture after a long clean run, as a draft you review.
- **Tools and compute**: files, shell, `http_fetch`, memory and conversation search, skills, MCP servers (managed from
  chat, CLI, TUI and desktop app, and `tern mcp serve`), a sandboxed `python` tool whose scripts call tern's tools, and
  agent-written Luau connectors with host-injected credentials, OpenAPI import and OAuth sign-in.
- **Bundles**: connectors, skills, routines and agents shared as signed, hash-pinned files whose capabilities are shown
  before install; `tern import openclaw`.
- **Telegram** with pairing and approval buttons, one bot per agent; ntfy and Slack gateways.
- **Remote core** over TCP with TLS, QR device pairing and scoped tokens.
- **Desktop app** (macOS; a Linux build is produced by the desktop-release workflow): conversations, channels,
  approvals, routines, run inspector, Tasks, provider, secret and settings editors, and a first-launch choice between
  this Mac and a paired server. The app and the core report their versions and warn when they differ.
- **Release pipeline**: tag-driven builds for four CLI targets and a macOS app, build provenance attestations,
  pre-release tags published as GitHub pre-releases; `scripts/release-dry-run.sh` checks a release before tagging.

[Unreleased]: https://github.com/mitchellalderson/tern-agent/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/mitchellalderson/tern-agent/releases/tag/v0.1.0-alpha.1
