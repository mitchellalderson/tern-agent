# FAQ

Short, honest answers to the questions people ask first. If something here is out of date with the code, the code
wins; please open an issue.

## What is tern, in one paragraph?

A personal agent you run yourself. One static binary for macOS or Linux holds the whole engine: chat in the terminal,
a daemon with a scheduler, Telegram, MCP, sandboxed Luau connectors, sub-agents, and a SQLite journal that records
every model call and tool result. Kill it mid-task and it resumes where it stopped without repeating side effects.
You bring a model key (Anthropic or any OpenAI-compatible endpoint, local ones included). There is also a desktop app
you build from source.

## What doesn't it do?

Said first, because you'll find out anyway:

- **No browser and no computer use.** It calls APIs. You can add a browser through an MCP server (below).
- **No connector catalog.** Nothing is pre-built beyond what MCP servers give you; the agent writes a connector when
  you need one.
- **Telegram is the only chat channel.** No Slack, Discord, WhatsApp or iMessage yet.
- **No mobile app and no push notifications.** Telegram is the phone interface.
- **No hosted version.** You run it. A hosted tier is an idea on the [roadmap](../ROADMAP.md), not a product.
- **Approvals block.** A run waits on its approval, and if there's no attached client or Telegram chat to ask, the
  answer is deny. An outbox, where the action waits while the rest of the run carries on, is roadmap (T3).
- **No taint tracking or read/write effects yet.** Prompt-injection defence today is approvals, the workspace
  boundary and the SSRF filter; see [SECURITY.md](../SECURITY.md) for what that leaves open.
- **No OAuth sign-in.** Connectors and MCP servers use API keys or tokens you provide.
- **No Windows build**, and no bundled model: you need an API key or a local model server on day one.
- **The desktop app is macOS-first and built from source.** No packaged, signed download yet.

## Why not OpenClaw?

If you want the biggest community, the most channels and plugins, and you're happy to run Node, use OpenClaw. tern is
for when you want something small that keeps working and that you can reason about:

- one binary, no runtime, an idle daemon in the low tens of MB of memory;
- a step journal, so a crash or a `kill -9` doesn't mean redoing or duplicating work;
- no open skill registry (OpenClaw's ClawHub had hundreds of malicious skills in early 2026); connectors are short
  scripts you approve one at a time, with their hosts and secrets on the approval card;
- local-only by default: the API is a Unix socket, and remote access needs TLS, device pairing and scoped tokens.

## Why not ZeroClaw or IronClaw?

They're the two Rust claws, and both are good. Honestly:

- **ZeroClaw** has an OS sandbox with more backends (Landlock, bubblewrap, Seatbelt, Docker), 30+ channels, and an
  SOP engine with resumable runs. tern sandboxes shell commands with Seatbelt, Landlock or Docker, but has none of
  that channel breadth.
- **IronClaw** has the agent build its own tools too (as WASM), injects credentials at the host boundary so tools never
  see them, and has routines with webhooks. tern's credential injection and webhooks are still roadmap.

What tern does differently:

- **The journal is the core, not a feature.** Every model call, tool call and approval in every run is recorded
  before and after it happens. Resume replays completed steps instead of re-running them, and a non-idempotent tool cut
  off mid-call isn't blindly retried.
- **Agent-written connectors are plain source you can read, sandboxed by the host, and pinned to what you approved.**
  The agent drafts a Luau file, tests it, and asks to activate it with a card listing hosts, secrets and tools. tern
  stores the hash and capabilities you approved and refuses to load the file if either changes.
- **One engine, local or remote.** The same daemon serves the terminal, the desktop app, Telegram and paired remote
  devices. A hosted version of the same engine is planned, not built.

## Why Luau and not WASM?

IronClaw chose WASM, and it's a reasonable choice. tern chose Luau because:

- **The agent writes source and tern runs it as is.** No compiler in the loop, no toolchain on the machine, and what
  you approve is a short script you can read, not a compiled module.
- **Luau was built to run untrusted scripts inside a host.** tern removes `require`, `load`, `loadstring`, `dofile`,
  `getfenv` and `setfenv`, gives each connector its own VM with no file or process access, stops it after 5 seconds
  (inside loops and pattern matching too) and caps it at 64 MB.
- **It's small.** The Luau VM is a C++ library linked into the one binary.

The trade-off: a WASM module has a memory-isolation boundary of its own and lets you write tools in any language. A
Luau VM runs inside tern's process, so a bug in the VM is a bug in tern. In both designs the important rules (which
hosts and secrets a tool may use) are enforced by the host, not by the script.

## Why Telegram and not Slack?

Telegram was the cheapest useful first channel: a bot is one token from BotFather, tern talks to it with long polling
(so it works from a laptop behind NAT with no public URL), it's free, it's on every phone, and inline buttons make
approvals one tap. Only accounts paired with a one-time code can talk to the bot.

Slack needs an app created and installed per workspace, and it is workspace-scoped, which fits teams more than one
person. Slack, Discord, ntfy and WhatsApp are planned as gateway connectors: a Luau file each rather than a Rust crate
(roadmap T4b, Stage 4). iMessage isn't planned; it needs Apple Business approval and per-user fees.

## Why no browser?

- APIs are faster, cheaper in tokens, and auditable: a connector's approval card can say exactly which hosts it
  reaches. A browser session can't.
- Every web page is untrusted input, and a browser that can click and submit is the largest prompt-injection surface
  an agent can have.
- Bundling Chromium would end "one small binary".

If you need one now, add a browser MCP server such as Playwright MCP or Chrome DevTools MCP under
`[mcp.servers.<name>]`; its tools show up like any other. A native browser over CDP, using your installed Chrome, is on
the roadmap behind a build flag. Chromium will never be bundled.

## What does the sandbox do on macOS versus Linux?

The same policy on both, enforced by each OS's own mechanism:

- **Default (`sandbox.backend = "host"`):** `shell` commands run on your machine as you, after you approve them, with a
  scrubbed environment (no secrets) and a timeout, inside an OS sandbox: **Seatbelt** on macOS, **Landlock** (plus
  seccomp for network-off) on Linux 5.13+. Writes are limited to the workspace and temp directories; `~/.tern` is
  never readable or writable; `~/.ssh` and the rest of your home directory are unreadable except the workspace,
  `~/.gitconfig` and whatever you add to `sandbox.read_paths`. The one difference: macOS lets commands read
  everything outside your home directory, while Linux allows only system directories (`/usr`, `/etc`, `/proc`, …).
  `sandbox.network = false` turns off IP networking on both. Timeout or cancel kills the command's whole process
  group. On a Linux kernel without Landlock, commands run unconfined with a warning (`tern doctor` says so), or not
  at all with `sandbox.confine = "required"`.
- **`sandbox.backend = "docker"`:** commands run in one long-lived container per workspace, as your uid, with only the
  workspace mounted, no capabilities, and memory, CPU and process limits (2 GB, 2 CPUs, 512 by default).
  `sandbox.network = false` gives it no network. On macOS that container lives in your Docker runtime's Linux VM.
- **Luau connectors** are sandboxed in-process the same way on every platform (previous answer).
- **File tools** are confined by tern itself: free inside the workspace, approval outside it, never `~/.tern`.

Tools that need more of your home directory (a `~/.cargo` registry, `~/.npm`) need it in `sandbox.read_paths` or
`sandbox.write_paths`. How it works: [docs/sandbox-design.md](sandbox-design.md).

MCP stdio servers run unsandboxed on the host. The full list of what's covered and what isn't is in
[SECURITY.md](../SECURITY.md).

## Is my data sent anywhere?

Only where you point it. There is no telemetry, no update check, no account, and no tern server.

- **Your model provider** receives the conversation, the system prompt (including `persona.md` and `user.md`) and
  tool results, including any memories or files the agent reads. That's how an LLM agent works; use a local model if that's a problem.
- **Telegram**, if you enable it: messages to and from the bot pass through Telegram's servers.
- **The hosts you or the agent call**: `http_fetch` URLs, the hosts a connector declares, MCP servers you configure.

Everything else stays in `~/.tern`: config, the SQLite database (conversations, journal, memories), and
`secrets.toml` (mode 0600, not encrypted). Copy, back up or delete the folder as you like. `tern report` writes a bug
report locally, with secrets redacted, and sends it nowhere.

## How is this different from Claude Duet?

Duet is Anthropic's hosted agent: a research preview since 2026-10-21, on Pro and Max plans in the US by waitlist, on
Anthropic's models, running in a cloud VM with a browser. It's well designed, and it has things tern doesn't yet:
topic channels over one memory, a Tasks view, and approvals that wait while the rest of the job continues. tern's
roadmap borrows those ideas (T2, T3).

tern is a different shape: it runs on your machine or server, with any model vendor, keeps a journal you can inspect
(`tern runs show`), and connects to services by writing a connector you approve rather than driving a browser. It has
no browser, no mobile app and no hosted option. If Duet covers your tools and you're on those plans, it's the easier
choice.

## Is this an Inngest thing?

<!-- TODO(owner): confirm this wording after the employer side-project check (launch-plan §3). -->
The author works at Inngest. tern's engine has no Inngest dependency: nothing in `Cargo.lock` comes from Inngest, and
it runs entirely offline apart from the services above. If a hosted tier is ever built, its control plane may run on
Inngest; the self-hosted engine would stay independent of it.

## What's the license?

MIT or Apache-2.0, at your option ([LICENSE-MIT](../LICENSE-MIT), [LICENSE-APACHE](../LICENSE-APACHE)).
