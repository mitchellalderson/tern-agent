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

## Docker backend

Unchanged in shape (one long-lived container per workspace, your uid, workspace at `/workspace`), plus:

- `--cap-drop ALL` and `--security-opt no-new-privileges`.
- Resource limits: `docker_memory` (default `"2g"`), `docker_cpus` (default `2.0`), `docker_pids` (default `512`).
- `network = false` starts the container with `--network none`.
- The container is labelled with a fingerprint of its settings; changing the image, limits or network replaces it.

## Config

```toml
[sandbox]
backend = "host"            # "host" | "docker"
shell_timeout_secs = 120
network = true              # both backends
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
