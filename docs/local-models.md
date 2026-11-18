# Local models

tern can run with no cloud model. It drives [Ollama](https://ollama.com) over HTTP: tern detects it, installs it
with your consent, starts it, and manages its models. Research and the reasoning behind the choices:
[research/local-models.md](research/local-models.md).

Local is private and offline, and good for everyday tasks. It is not a replacement for the best cloud models on long,
multi-step work. A model that can't use tools can chat but can't run tasks; tern says so.

## How tern treats Ollama

- **Probe first, always reuse.** Whatever answers on `127.0.0.1:11434` is used, whoever started it. tern never starts a
  second server.
- **A server you run is used as is.** tern warns about its settings (context, version) but never changes them.
- **A server tern starts is managed.** `POST /v1/local/start` runs `ollama serve` with loopback-only
  `OLLAMA_HOST`, `OLLAMA_CONTEXT_LENGTH` sized to this Mac's memory (64k from 16 GB), `OLLAMA_KEEP_ALIVE=30m`, and on
  Macs under 32 GB a quantized cache. It stops with the daemon. `POST /v1/local/stop` stops only a server tern started.
- **No Ollama?** Link to ollama.com/download, or "Install for me" (below).

## First run and auto-start

First run in the desktop app is one prompt: **Set up with Ollama**, with a smaller "Use an API model instead" that opens
the existing provider setup unchanged. One press installs Ollama if needed (after the consent sentence above it), starts
it, downloads a starter model sized to this Mac (`local/starters.rs`, a data table; `status.starter`), and makes
`ollama:<model>` the default. Only a step the person must take shows anything; a failed start shows one line and a retry.

The terminal UI (`tern setup`, `/setup` on first run) does the same: "Set up with Ollama" is the main choice, `a` opens
the usual provider list ("Use an API model instead"). It needs the background agent (`tern start`), which manages Ollama
over its Unix socket; without it, first run shows the provider list as before.

tern never prompts to start Ollama. `Local::ensure_running` reuses a server that answers, else starts the installed one
(loopback only, managed). It runs at daemon start when an Ollama provider is configured, before each run on an Ollama
model, and during first-run setup. Existing configs keep their providers; only a new install gets the Ollama default.

## Install for me

Only after you press the button, under a sentence saying what it does. The daemon refuses without `{"consent": true}`.

1. Ask GitHub's API for Ollama's latest release (https, `api.github.com`).
2. Download `Ollama-darwin.zip` from GitHub's own hosts only. Its SHA-256 must equal the `digest` GitHub lists. No
   digest, no install.
3. Unpack with `ditto`; require `codesign --verify --deep --strict`, Gatekeeper's `spctl --assess` (signed and
   notarized), and a Developer ID signer whose name says Ollama.
4. Move it to `~/Applications/Ollama.app`. A failed check installs nothing and leaves nothing behind.

## API

| Method | Path | |
|---|---|---|
| GET | `/v1/local/status` | `{engine, installed, running, version, too_old, managed, url, models_dir, disk_free, ram_bytes, context, context_source, warnings, install_page, install: {source, destination, summary, available}, installing: {state: idle｜working｜done｜failed, phase, completed, total, signer, message}}` |
| GET | `/v1/local/models` | installed models: `{name, size, digest, family, parameter_size, quantization, tools, thinking, max_context, loaded}` |
| DELETE | `/v1/local/models/{name}` | remove an installed model (the name may contain `/` and `:`) |
| GET | `/v1/local/pulls` | downloads, running and recent: `{id, model, phase: starting｜downloading｜verifying｜done｜failed｜cancelled, completed, total, error}` |
| POST | `/v1/local/pull` | `{model}`: start a download; answers the download |
| POST | `/v1/local/pulls/{id}/cancel` | stop a download (pulling it again resumes) |
| POST | `/v1/local/start` | start the engine unless one already answers |
| POST | `/v1/local/stop` | stop an engine tern started |
| POST | `/v1/local/install` | `{consent: true}`: install in the background; progress in `status.installing` |

Every change sends `local_changed` on `/v1/events`; re-read the status. `health.features` has `local_models`.

Every change sends `local_changed`; a download also sends `local_pull` (`{id, model, phase, completed, total, error}`)
as it goes. When one ends, the `ollama` provider's model list is refreshed (`models_changed`).

## Models per tier

Three places use a model: **agent** (runs tasks with tools: `default_model`), **fast** (summaries and short writing:
`fast_model`) and **decision** (quick choices: `decision_model`). Small models do the last two well, so every Mac gets a
useful set even when the agent model is modest. `local/catalog.rs` is the recommended model per tier by memory, as a data
table (tags are unverified: check each with a real tool-use run before relying on it). The same table gives first run its
starter (the agent pick).

- `GET /v1/local/catalog`: `{ram_bytes, tiers: [{tier, model, size_gb, blurb, note, installed, current}], setting_up, error}`.
- `POST /v1/local/tiers` `{tier, model}`: use an installed model for a tier. A model that can't use tools is refused
  for agent and decision, and allowed for fast. If no Ollama provider is configured yet, it is added.
- `POST /v1/local/setup`: download what's missing from the recommended set (one at a time), then use each for its tier.
  It stops at the first failure and says why in `error`.
- CLI: `tern local recommended`, `tern local use TIER MODEL`, `tern local setup` (these need the daemon).

The desktop Local models dialog shows the recommended row per tier (Download, Use for, In use), "Set up all three",
and a "Use for Agent / Fast / Decision" menu on each installed model.

## Downloads

The daemon owns them, so they go on when a window closes. One at a time. Before starting, Ollama must be running and the
disk must have at least 10 GB to spare. Once the layers' sizes are known, a model that won't fit stops with a plain
message instead of filling the disk. Progress sums all layers into one bar. Cancel stops it; pulling the same model again
resumes the part already there.

## CLI

```
tern local status [--json]
tern local list [--json]
tern local pull MODEL [--json]     # progress bar; Ctrl-C cancels
tern local rm MODEL
tern local start | stop            # needs the daemon, so tern can stop what it started
```

Through the running daemon when there is one (a window sees the download), else straight to Ollama.

For tests and unusual setups, `TERN_LOCAL_URL` points tern at another Ollama address, `TERN_LOCAL_DISK_FREE` (bytes) stands in for the disk, and `TERN_LOCAL_APPS` (a
`:`-separated list) replaces the folders searched for `Ollama.app`.

## The engine trait

`crates/tern/src/local/mod.rs` defines `Engine`: detect, status, list, pull, delete, start, stop. `local/ollama.rs` is the first implementation. MLX or `llama-server` would be a second one, not a change to
the daemon.
