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
| POST | `/v1/local/start` | start the engine unless one already answers |
| POST | `/v1/local/stop` | stop an engine tern started |
| POST | `/v1/local/install` | `{consent: true}`: install in the background; progress in `status.installing` |

Every change sends `local_changed` on `/v1/events`; re-read the status. `health.features` has `local_models`.

For tests and unusual setups, `TERN_LOCAL_URL` points tern at another Ollama address and `TERN_LOCAL_APPS` (a
`:`-separated list) replaces the folders searched for `Ollama.app`.

## The engine trait

`crates/tern/src/local/mod.rs` defines `Engine`: detect, status, list, start, stop (pull and delete arrive with the model
browser). `local/ollama.rs` is the first implementation. MLX or `llama-server` would be a second one, not a change to
the daemon.
