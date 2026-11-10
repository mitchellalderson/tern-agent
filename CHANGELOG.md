# Changelog

All notable changes to tern are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Versions with a hyphen (`-alpha.N`,
`-beta.N`, `-rc.N`) are GitHub pre-releases.

Entries go in `changelog.d/` (see its README); the release gathers them here.

## [Unreleased]

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
