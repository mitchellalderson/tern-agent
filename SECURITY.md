# Security

## Reporting a vulnerability

Email **security@TODO-DOMAIN** <!-- TODO(owner): the domain isn't chosen yet; set a real address before the repo is public. -->
with a description, the tern version (`tern --version`), your OS, and steps to reproduce. Please don't open a public
issue for anything that lets someone run commands, read files or secrets, or bypass an approval. If GitHub private
vulnerability reporting is enabled on the repository, that works too.

tern is a side project maintained by one person. Expect an acknowledgement within a few days, and a fix or a plan
before anything is disclosed. Only the latest release is supported.

`tern report` writes a bug report with secrets replaced by `[REDACTED]`; review it before attaching it anywhere.

## Threat model

tern is an agent that runs a language model in a loop with tools, on your machine, as your user. The model is not
trusted: anything it reads (a web page, a file, an API response, an MCP server's output) can contain instructions,
and tern assumes the model may follow them. The safety layers are in the host process, not in the prompt.

**What it does today.**
`shell` always asks first (allow once, always for a pattern such as `git *`, or deny); every answer is journaled.
"Always" patterns never match a command containing shell metacharacters (`` ; | & $ ` > < ( ) `` and newlines), and
"always" is never offered for commands that run other commands (shells, `env`, `sudo`, `xargs`, `find`, `ssh`,
`docker`, interpreters) or for activating connectors. The file tools work freely inside the workspace
(`~/tern-workspace` by default) and ask before reading or writing anywhere else. They refuse paths that escape the
workspace through `..`, symlinks or dangling symlinks, can never write inside `~/.tern`, and can never read
`secrets.toml` or the database. `http_fetch` (GET only) refuses loopback, private, link-local, CGNAT and
cloud-metadata addresses, resolves DNS itself, pins the connection to the checked address, and checks every redirect
hop. Secrets live in the environment or in `~/.tern/secrets.toml`, which tern refuses to load unless it is mode 0600.
They are not passed to `shell` (it gets a scrubbed environment), stdio MCP servers get only the secrets they declare,
and every secret value tern knows about is replaced with `[REDACTED]` in the journal, the stored conversation, logs,
connector and MCP server output that goes to the log, and `tern report`. Luau connectors run in a separate VM each,
with no file, process or module access, 5 seconds and 64 MB per call, and can reach only the hosts and read only the
secrets they declare, including across redirects. A connector the agent writes stays in `drafts/` until you approve
it on a card that lists its hosts, secrets and tools; tern records the hash of the approved source and declared
capabilities, and won't load an activated connector whose file or capabilities later change. The daemon's local API
is a Unix socket at mode 0600. Remote access over TCP is off by default; when enabled, it requires TLS (a self-signed
certificate that clients pin by SHA-256, or your own), a device token from a one-time pairing code (10 minutes, single
use, rate-limited, only a hash of the token is stored, revocable at once), and a scope (`read`, `chat` or `owner`);
config, secrets and devices are owner-only, and new endpoints are owner-only until listed. Telegram only answers
accounts paired with a one-time code, and an approval button counts only in the chat it was sent to.

**Sandboxing, macOS versus Linux.** There is no OS-level sandbox on either platform: tern doesn't use Seatbelt,
Landlock, seccomp or namespaces, so the behaviour is the same on both. With the default `sandbox.backend = "host"`,
an approved `shell` command runs as you, with your files and network, and only the environment is scrubbed. On
timeout or cancel tern kills the command's shell, but processes it started in the background can outlive it. With
`sandbox.backend = "docker"`, commands run in one long-lived container per workspace, as your uid, with only the
workspace mounted (read-write) at `/workspace`; the container has Docker's default bridge network and no CPU or memory
limits. On macOS the container runs inside your Docker runtime's Linux VM (Docker Desktop or similar). Approvals still apply in both backends.

**What it does not cover.**
- **Prompt injection, beyond approvals.** There is no taint tracking yet: a run that read a hostile page is treated
  like any other. Tools that don't ask (reading inside the workspace, `http_fetch`, memory, schedules, and connector
  and MCP tools unless configured with `requires_approval`) can be chained without you seeing it, so for example
  workspace contents can leave in an `http_fetch` URL. Effects, taint and an outbox for writes are on the
  [roadmap](ROADMAP.md) (T0, T3).
- **What you approve.** Once a shell command or an "always" pattern is approved, tern doesn't limit what it does.
- **MCP servers.** stdio servers run on the host as you, unsandboxed, with a scrubbed environment plus their declared
  secrets. Their tools don't ask unless you set `requires_approval`. Only add servers you trust.
- **Connectors you install by hand.** Files you copy into `~/.tern/connectors/` load without an approval record. A
  connector can read the secrets it declares and send them to the hosts it declares; host-side credential injection
  is roadmap (T4).
- **The model provider and Telegram.** Your conversation, tool results and the system prompt go to the provider you
  configure; with Telegram on, chat passes through Telegram's servers. Redaction applies to what tern stores and logs,
  not to what the model sees during a live run: if a tool returns a secret value, the model receives it.
- **Your own account.** `secrets.toml` and the database are plain files protected by Unix permissions, not encrypted.
  Any process running as your user can read them and can drive the agent through the socket.
- **Remote access done badly.** `tls = "off"` is allowed only on loopback behind a proxy you declare with
  `trust_proxy`; exposing a core to the internet is your call, and an `owner` device can do anything you can.
- **The desktop app** is a client of the same API with no extra privileges; it is not separately hardened or
  notarized yet.
- **Supply chain.** Release archives are checksummed by `install.sh` but not signed.

The design notes behind these choices, including an independent review of the trust boundaries and its fixes, are in
[DECISIONS.md](DECISIONS.md) ("M4", "M6: Security review fixes" and "D1").
