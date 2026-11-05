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

**Sandboxing, macOS versus Linux.** With the default `sandbox.backend = "host"`, an approved `shell` command runs as
you, with a scrubbed environment, inside an OS sandbox: Seatbelt (`sandbox-exec`) on macOS, Landlock (plus seccomp
when networking is off) on Linux 5.13+. On both, a command can write only to the workspace, the temp directories and
the `sandbox.write_paths` you list, and can never read or write tern's data directory (`~/.tern`). Reads differ: on
macOS everything outside your home directory is readable and, inside it, only the workspace, `sandbox.read_paths`
(default `~/.gitconfig` and `~/.config/git`) and `PATH` entries; on Linux only system directories (`/usr`, `/etc`,
`/proc`, …) and those same paths are readable, so `~/.ssh`, `~/.aws` and the like are out of reach on both.
IP networking is off by default (Seatbelt rules on macOS; seccomp, and Landlock on 6.7+, on Linux): a command gets
it only when it asks (`network: true`), which you approve every time; `sandbox.network = true` turns it on for all.
macOS also denies Keychain lookups and signals to processes outside the sandbox; Linux sets `no_new_privs`, stops
ptrace and `/proc/<pid>/environ` reads of outside processes, and scopes signals on 6.12+. Each command runs in its own
process group, and timeout or cancel kills the whole group. If the OS can't sandbox (Linux without Landlock),
`sandbox.confine = "auto"` runs commands unconfined and warns once and in `tern doctor`; `"required"` refuses to run
them. With `sandbox.backend = "docker"`, commands run in one long-lived container per workspace, as your uid, with
only the workspace mounted (read-write) at `/workspace`, all capabilities dropped, `no-new-privileges`, limits of
2 GB memory, 2 CPUs and 512 processes by default, and `--network none` unless `sandbox.network = true` (per-command network isn't available there). On macOS the
container runs inside your Docker runtime's Linux VM (Docker Desktop or similar). Approvals still apply in both
backends. Details: [docs/sandbox-design.md](docs/sandbox-design.md).

**Python script grants.** One shell shape can skip the per-call prompt: `python3 <workspace file>.py <plain args>`,
with no shell syntax, interpreter options, wrappers or network, after you answer "this run", "this conversation" or
"always" for that exact script. The grant is keyed on the interpreter, the script's path and SHA-256, the arguments
and the workspace, so an edited script or different arguments ask again, and the file is re-hashed immediately
before it runs. It applies only when a canary has verified that the OS sandbox is active (a write outside the
workspace and an outbound connection both fail), the network is off, and the run is untainted; otherwise the card says
"Not auto-approved: ...". What it covers: arbitrary Python inside the sandbox. It can overwrite or delete any file in
the workspace (originals aren't protected from it yet), and it isn't limited by the grant's label in any other way.
A granted script runs `python3 -I` (no script directory on `sys.path`, no user site), so planted modules can't shadow the standard library and local helper modules next to it won't import; the interpreter must be a native program outside every place the sandbox can write (no workspace venv, no version-manager shim) and is run by its canonical path. The interpreter's own directories, `pyvenv.cfg` locations and the paths it reports it loads code from (asked inside the sandbox) must be outside every place the sandbox can write, no writable place may lie inside its prefix, and its `.pth` files may add no writable directory and run no code, and a `python3` that is a launcher (mise) or script is refused. A call decided by a grant runs its plan or nothing, and a restart between an approval and the run re-asks. Reading a data file
(a spreadsheet, a PDF) does not taint the run, so injected text in a data file can steer the agent's next script,
which then asks unless a grant covers that exact script. Docker, `confine = "off"` and an unverifiable sandbox never
get grants. Auto-runs are recorded in the ledger (`approved_by_grant`); `tern grants` lists and revokes "always"
script grants.

**What it does not cover.**
- **Prompt injection, beyond approvals.** Every tool has an effect, and a run that has read untrusted content (a web
  page, mail, a tool marked untrusted) is marked tainted: its writes ask whatever "always" says, it can reach new sites
  only after you say so, and unattended writes wait in the outbox. That narrows the damage; it doesn't remove it. Tools
  that don't ask (reading inside the workspace, memory, and connector and MCP tools whose effect is `read`) can still
  be chained, and a read from a site you already allowed can carry data out in a URL. Taint is tracked from the
  sources tern knows about: an MCP server or connector that returns third-party text without being marked untrusted
  isn't caught. See [docs/reference.md](docs/reference.md) (Taint, Outbox).
- **What you approve, inside the sandbox.** Once a shell command or an "always" pattern is approved, tern limits it
  only by the sandbox: it can change anything in the workspace, read what the sandbox allows, and use the network
  if it asked for it (or `sandbox.network = true`). Network access is all or nothing (no per-host egress rules). The host backend
  has no CPU, memory or process limits, can reach Unix sockets your user can (including Docker's, if you have
  access), and can leave background processes running after a command exits normally or by calling `setsid`. On
  Linux, `/proc` shows other processes' command lines.
- **MCP servers.** stdio servers run on the host as you, unsandboxed, with a scrubbed environment plus their declared
  secrets. Their tools don't ask unless you set `requires_approval`. Only add servers you trust.
- **Connectors you install by hand.** Files you copy into `~/.tern/connectors/` load without an approval record. A
  connector that declares `auth` never sees its credential (tern adds it to requests for the declared hosts), but an
  older one that reads `ctx.secret` can read the secrets it declares and send them to the hosts it declares; tern
  flags those.
- **The model provider and Telegram.** Your conversation, tool results and the system prompt go to the provider you
  configure; with Telegram on, chat passes through Telegram's servers. Redaction applies to what tern stores and logs,
  not to what the model sees during a live run: if a tool returns a secret value, the model receives it.
- **Your own account.** `secrets.toml` and the database are plain files protected by Unix permissions, not encrypted.
  Any process running as your user can read them and can drive the agent through the socket.
- **Remote access done badly.** `tls = "off"` is allowed only on loopback behind a proxy you declare with
  `trust_proxy`; exposing a core to the internet is your call, and an `owner` device can do anything you can.
- **The desktop app** is a client of the same API with no extra privileges; it is not separately hardened or
  notarized yet (release builds are ad-hoc signed until a Developer ID is set up).
- **Supply chain.** Release archives are checksummed by `install.sh` and carry GitHub build provenance attestations
  (`gh attestation verify`), but are not signed with a release key.

The design notes behind these choices, including an independent review of the trust boundaries and its fixes, are in
[DECISIONS.md](DECISIONS.md) ("M4", "M6: Security review fixes" and "D1").
