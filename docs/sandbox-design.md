# Shell sandbox

What confines the commands the `shell` tool runs. Approvals still come first; this is what holds when an approved
command does something you didn't expect. Luau connectors, MCP servers and the file tools are out of scope here (see
[SECURITY.md](../SECURITY.md)).

## Host backend (default)

Every command runs as `sh -c <command>` in the workspace, with the scrubbed environment as before, under an OS sandbox:

| | macOS (Seatbelt, `sandbox-exec`) | Linux (Landlock + seccomp, kernel 5.13+) |
| --- | --- | --- |
| Write | workspace, temp dirs, `write_paths`, `/dev/null` and ttys | workspace, `/tmp`, `/var/tmp`, `write_paths`, `/dev/null` and ttys |
| Read | everything except your home directory; in home only the workspace, `read_paths` and `PATH` entries | only system dirs (`/usr`, `/bin`, `/lib*`, `/etc`, `/opt`, `/proc`, `/sys`, `/dev`, `/run`, `/nix`, `/snap`), the workspace, `read_paths`, `write_paths` and `PATH` entries |
| Never | tern's data dir (`~/.tern`), read or write, whatever else allows it | tern's data dir: any grant that contains it is dropped, with a warning |
| Network off (`network = false`) | IP sockets denied (Unix sockets still work) | `socket(AF_INET/AF_INET6)` and `io_uring_setup` fail (seccomp), plus Landlock's TCP rules on 6.7+ |
| Signals | only to processes in the same sandbox | only inside the sandbox on 6.12+ (Landlock scopes) |
| Other | no Keychain (`securityd` lookups denied); setuid binaries can't run | `no_new_privs`; can't ptrace or read `/proc/<pid>/environ` of processes outside the sandbox |

Reading the home directory is the line on macOS because that is where the secrets are (`~/.ssh`, `~/.aws`, browser
profiles, `~/.tern`). Linux uses an allowlist because Landlock can only grant, never carve out. The defaults for
`read_paths` are `~/.gitconfig` and `~/.config/git`, because git refuses to run when it can't read its config.

On Linux the command is started through the `tern` binary itself (`tern __sandbox-exec …`), which applies Landlock
and seccomp and then `exec`s `sh`. That keeps `unsafe` out of the workspace: no `pre_exec` hooks, just the `landlock`
and `seccompiler` crates in a process that is about to exec.

### When the OS can't sandbox

`confine` picks what happens when the sandbox isn't available (Linux without Landlock, a seccomp filter that can't be
built for this CPU, `sandbox-exec` missing):

- `"auto"` (default): run unconfined, log a warning once, and say so in `tern doctor`.
- `"required"`: refuse to run the command.
- `"off"`: never confine (the behaviour before this change).

A partially enforced Landlock ruleset (an older kernel that lacks a newer ABI's rights) still runs; `tern doctor`
reports the ABI level.

### Process lifetime

Each command gets its own process group. On timeout or cancel tern kills the whole group, so `sleep 100 &` dies with
it. A process that leaves the group on purpose (`setsid`, double-fork daemons) can still outlive the command. When a
command exits normally, background processes it started keep running (on purpose: `npm run dev &` is a common ask).

### Not covered

- Reading outside home on macOS (`/etc`, `/Applications`, other users' world-readable files).
- Network egress filtering by host: `network` is all or nothing.
- Resource limits (CPU, memory, process count) on the host backend; use Docker for that.
- Unix sockets, including Docker's. If your user can reach the Docker socket, a command can too. Turning `network`
  off doesn't change that.
- `/proc` on Linux shows other processes' command lines (not their environments).
- Processes that escape the process group (above).

## Script grants: allow one python script for a run

A data-analysis agent runs `python3 something.py` dozens of times; asking for every one is noise that trains people
to click "yes". One exact shape can be allowed instead, for this run, this conversation, or always. Everything else
about `shell` asks exactly as before.

**What qualifies.** The command is, token for token, `python3 <file>.py <args>` (or `python`):

- it is tokenised on plain spaces over a small alphabet (letters, digits, `_ . / - + , : = @ %`): no quotes, `$`,
  `` ` ``, `;`, `|`, `&`, `<`, `>`, `(`, `)`, `~`, globs, braces, `#`, `!`, backslashes or newlines, so there is no
  shell syntax left for `sh` to interpret (a pipe or `2>&1` doesn't qualify; output is already captured and capped);
- no interpreter options (`-c`, `-m`, `-`, `-i`, `-x`, ...), no heredoc, no env prefix and no wrapper (`env`, `time`,
  `sudo`);
- the script is relative, has no `..`, ends in `.py`, resolves (symlinks followed) to a regular file inside the
  workspace (checked on the open handle; a fifo can't hang the check), and is at most 8 MB;
- `network` is absent or `false` for the call;
- `python3` resolves, on the absolute `PATH` entries, to a native program (Mach-O or ELF), not a script: asdf, pyenv
  and mise shims pick the real interpreter at run time from `.tool-versions`, `.python-version` or `mise.toml` in
  the workspace. Python finds `pyvenv.cfg`, site-packages and `.pth` files from the path it was *started* from, so
  the path as found, every directory and symlink hop between it and the canonical target, and the target itself must
  all be outside every place the sandbox can write (the workspace, temp dirs, `write_paths`): a `<workspace>/.venv`
  or a writable launcher directory is refused. What runs is the canonical interpreter path, never the path it was
  found at. The canonical file must be named `python*` (mise is one native launcher behind a `python3` symlink that
  reads the workspace). Python also reads `pyvenv.cfg` next to its binary or one level up, and loads the standard
  library, site-packages and `.pth` files from its prefix (`-I` still imports `site`), so the binary's directories
  (as found and canonical), their `pyvenv.cfg` paths, and everything the interpreter itself reports (`sys.prefix`,
  `sys.path`, `site.getsitepackages()`) must be outside the writable roots. The report comes from running the
  interpreter once per file (cached by path and mtime) outside the sandbox with a fixed `-I -S -c` program, from `/`
  with an empty environment, after the directory checks pass (`-S`: no `site`, no `.pth`). A writable stdlib
  directory would still execute in that one probe, so such a configuration should not list the interpreter's own
  directories in `write_paths`. All of this is checked again when the call runs.

A python command that doesn't qualify says why on its card ("Not auto-approved: inline script. Saved as a .py file
in the workspace and run as `python3 file.py`, it could be approved once for the whole run.").

**What a grant covers.** The key is a SHA-256 over the canonical interpreter path (and the path as found), the
canonical script path, the sha256 of the script's bytes, the arguments, and the workspace root. Change any of them
(edit the script, another argument, a copy of it under another name, another interpreter) and the key is different, so
the call asks again. The card shows the script path, a short hash and the arguments, and "Imports from other files
aren't covered".

**Isolated mode.** A grant runs `python3 -I <script>`: no script directory and no current directory on `sys.path`,
no user site, no `PYTHON*` variables. A `csv.py` or `json.py` planted next to an approved stdlib-only script is
therefore never imported, and the standard library can't be shadowed. Installed site-packages (pandas, openpyxl)
still import. The price: the script must be self-contained. `import helper` of a module next to it fails with
`ModuleNotFoundError`, and the card says so. (This replaces "imports from other files aren't covered": they
aren't allowed at all, rather than silently unreviewed.)

**Imports are not covered.** Only the entry file is hashed. `import helper` loads `helper.py` from the script's
folder (or any package on `sys.path`) as it is at run time, so a script that was approved can run different code if
a module it imports was edited. Hashing sibling files would not be sound (packages, `sys.path`, `.pth` files,
`__pycache__`), so tern doesn't pretend to. The same holds for data files the script reads: a script that reads a
spreadsheet or a PDF is not tainted by it.

**Plan, then run exactly that.** `policy` plans the call (keyed by run id and call id, like `connector_test`):
the resolved interpreter and script, the hash, and the key. `call` uses up exactly its own plan, re-reads the
script and the interpreter, and refuses (running nothing, with a message saying the file changed since it was
approved) if the hash, the canonical path or the interpreter differ. It then runs the planned interpreter and the
canonical script path, not the text that was typed. Plans are dropped when a call won't run (denied, refused,
cancelled) and when the run ends. A call that a grant decided (nobody was asked) must run exactly its plan: the tool
is told (`Tool::grant_applied`), and `Shell::call` refuses, running nothing, if it has no plan, the plan's key
isn't the approved one (a re-plan over changed bytes), or the typed command differs. It never falls through to
`sh -c <typed command>`. A human's answer keeps today's meaning: the typed command runs. A restart between the
journaled approval and the tool step doesn't run on the old answer: the replayed approval of a call that never ran
(whether or not a key can be planned now, e.g. while the probe is still landing) is refused ("approved before tern restarted ... make the
call again") and asks again, since the script may have changed meanwhile. A call with no plan never has a key, so it can't be auto-approved; it asks like
any command. The read-then-spawn window is milliseconds; a background process started by an earlier approved
command could still rewrite the file inside it, but such a process already runs arbitrary code in the same sandbox.

**The sandbox gate.** A grant is offered, and applied, only when the sandbox is confirmed active for this call:

- the OS sandbox is *verified*, not just detected (below);
- the sandbox has the network off (`sandbox.network = false`), so a script can't send what it reads anywhere (with
  the network on the output is also untrusted, which taints the run, which ignores grants anyway);
- the run is not tainted and not `readonly` (the usual rules for every grant).

Otherwise no grant is offered and the card says so: "Not auto-approved: the sandbox isn't confirmed active (...)".
A Docker backend can't be probed from the host, so it never verifies; `confine = "off"` never verifies.

**The canary.** `HostBackend::verify` runs a tiny command through the same wrapper production commands use
(`sandbox-exec` with the production profile on macOS, the Landlock helper on Linux) and checks, with the files and a
loopback listener on the tern side as the oracle:

1. a write inside the workspace works (positive control: this is what tells "blocked" from "nothing ran", and why a
   sandbox nested inside another one, which fails to start with exit 71, is *not verified*);
2. a write outside the workspace, in a folder the profile doesn't make writable (`$HOME`, else the workspace's parent),
   fails, and no file appears;
3. with the network off, an outbound connect to a listener on loopback is refused: the listener sees nothing, and the
   probe tool (`perl`, else `python3`) reports it tried and failed (it must also work unsandboxed, so a missing tool
   can't pass for a block).

The result is cached per (policy, workspace) for 10 minutes (30 seconds after a failure). The agent loop never
probes: `verify` only reads the cache and, when nothing is fresh, starts the probe on its own thread and answers "not
verified: still checking the sandbox; ask again shortly" (not cached) until it lands; the daemon warms it at start (every agent shares the one configured workspace, so one entry; a second workspace or
policy would get one in-flight probe per key, never one per call),
and the cache lock is never held across a probe. `tern doctor`, `tern security audit` and `/v1/info` use the
blocking `verify_now`; a config change is a different policy and probes again. It shows in `tern doctor`, `tern security
audit`, and `/v1/info` (`sandbox.verified`, `sandbox.detail`). Verified means the profile bites on this machine now;
it does not mean the profile is complete (see "Not covered").

**What it can still do.** Arbitrary code, inside the sandbox: change or delete any file in the workspace (originals
are not protected from the agent's scripts yet), read what the sandbox lets it read, write to the temp dirs. Every
auto-run is in the ledger as `approved_by_grant` with the key, script hash, interpreter and sandbox state.

## Docker backend

Unchanged in shape (one long-lived container per workspace, your uid, workspace at `/workspace`), plus:

- `--cap-drop ALL` and `--security-opt no-new-privileges`.
- Resource limits: `docker_memory` (default `"2g"`), `docker_cpus` (default `2.0`), `docker_pids` (default `512`).
- `network = false` starts the container with `--network none`.
- The container is labelled with a fingerprint of its settings; changing the image, limits or network replaces it.

## Network: off unless a command asks

`network = false` is the default. A command that needs the network (`git push`, `npm install`) is called with
`network: true` and gets it for that one command:

- it asks every time: the shell allowlist doesn't cover it (`git *` doesn't approve `git push` with network), and the
  prompt offers no "always". The prompt says `with network: <command>`;
- its output is untrusted, so it taints the run (it may carry whatever the far end sent). Output is untrusted
  whenever a command could reach the network: also with `network = true`, and on a host backend with no OS sandbox,
  where `network = false` isn't enforced;
- the next command is offline again.

This is what keeps shell commands from going around tern's tools: a script that calls an API directly skips the
connector's host allowlist, effect, taint and journal. With no network, data comes in only through tools, and
compute over it goes to the `python` tool (docs/python-tool-spec.md). The Docker backend can't change a running
container's network, so there a `network: true` command fails and says to set `network = true`.

Configs written before this change by `tern config init` say `network = true` explicitly and keep every command on
the network until that line is changed.

## Config

```toml
[sandbox]
backend = "host"            # "host" | "docker"
shell_timeout_secs = 120
network = false             # both backends; true gives every command network
confine = "auto"            # host only: "auto" | "required" | "off"
read_paths = ["~/.gitconfig", "~/.config/git"]   # host only, read-only
write_paths = []            # host only, e.g. ["~/.cargo/registry", "~/.npm"]
docker_image = "debian:stable-slim"
docker_memory = "2g"
docker_cpus = 2.0
docker_pids = 512
```

`~` expands to `$HOME`. Missing paths are skipped. `PATH` entries are readable automatically (except `$HOME` itself,
relative entries, and tern's data dir) so toolchains like `~/.cargo/bin` still resolve; a toolchain that also reads
another directory (`~/.rustup`) needs it in `read_paths`.
