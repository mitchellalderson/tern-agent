# Changelog

All notable changes to tern are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Versions with a hyphen (`-alpha.N`,
`-beta.N`, `-rc.N`) are GitHub pre-releases.

## [Unreleased]

### Changed

- The first-launch screen is a calm welcome: **Start on this Mac** first, **Connect to a remote server** second, and the socket path and OS error sit behind **Details**.

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
