# All-in-one local models: Ollama, LM Studio, llama.cpp and MLX

Research, 2026-11-18. No product code changed.

**Question.** tern is local and single-player. How hard is it to manage local models from the app, so tern runs with no cloud model at all? Which engine should it use: Ollama, LM Studio, llama.cpp, MLX, or something else?

**Short answer.** Use Ollama as the engine, as an outside dependency: detect it, install it with consent, drive it over its HTTP API. The first version is medium effort, not large. The connection already works; what's missing is the management layer and a few silent-failure fixes (§1). MLX should not be the primary runtime in v1: Ollama already runs MLX by default on Macs with 32 GB or more, and falls back to llama.cpp below that. Don't bundle Ollama or embed an engine yet. Put a small engine trait in the daemon so MLX or `llama-server` can be added later (§2, §3).

Some claims below come from blog posts, not official docs. They are marked *(secondary)*. Model names move fast (this ecosystem renames models monthly), so ship the recommended list as data that can be updated without a release.

## 1. What works today, and what breaks

tern already has an `ollama` preset (`http://localhost:11434/v1`, no key) in [presets.rs](../../crates/tern-core/src/presets.rs). It uses the generic OpenAI-compatible provider ([openai_compat.rs](../../crates/tern-providers/src/openai_compat.rs)). The Models dialog lists models from `GET /v1/models` and skips the key step for local presets ([providers.dart](../../apps/desktop/lib/ui/providers.dart)).

| Area | State | Evidence and fix |
|---|---|---|
| Streaming | Works | `/v1/chat/completions` supports `stream` and `stream_options.include_usage`, the only option tern sends ([Ollama OpenAI compat](https://docs.ollama.com/api/openai-compatibility)). |
| Tool calling | Works for tool-capable models | `/v1` supports `tools`. Calls stream in over several chunks, and results come back as `role: "tool"` ([Ollama tool calling](https://docs.ollama.com/capabilities/tool-calling)). tern's decoder already handles streamed `tool_calls`. A model without tool support fails at request time. We can't tell it apart from other errors today (see "capability" below). |
| `tool_choice` | **Ignored** | Ollama doesn't support it ([compat page](https://docs.ollama.com/api/openai-compatibility)). tern sends `tool_choice: "none"` when `forbid_tool_calls` is set (`openai_compat.rs`, `request_body_for`). On Ollama the model may still call a tool. Fix: when the provider is Ollama, omit `tools` entirely instead. Small change. |
| Context window | **Silent truncation risk** | The default context is 4k tokens under 24 GiB of memory, 32k at 24–48 GiB, 256k above 48 GiB ([Ollama context length](https://docs.ollama.com/context-length)). Ollama says agents need at least 64k. `/v1` can't set `num_ctx` per request ([compat page](https://docs.ollama.com/api/openai-compatibility)). tern assumes 32 768 for a local server that doesn't say (`config.rs`, `context_window`). On a 16 GB Mac that is 8× what Ollama will use, so the prompt is cut without an error and the agent forgets its instructions. This is the biggest problem. |
| Context-length detection | Missing | `/v1/models` has no context length. `POST /api/show` returns `model_info["<arch>.context_length"]` and `capabilities` ([API reference](https://github.com/ollama/ollama/blob/main/docs/api.md)). tern doesn't call it. |
| Prompt caching | Partly | Ollama reuses its KV cache for a shared prompt prefix while the model stays loaded. tern's stable-prefix layout helps. I found no documented `cached_tokens` in `/v1` usage, so the cache hit rate in tern's ledger will read 0 (*unverified*, check against a running Ollama). `prompt_cache_key` is already off for non-OpenAI hosts. |
| Reasoning output | Probably works | tern reads `reasoning_content` and `reasoning` from deltas. Ollama's `/v1` accepts `reasoning_effort` and returns thinking for thinking models ([compat page](https://docs.ollama.com/api/openai-compatibility)). I couldn't confirm the exact delta field name. Check with one real call. |
| Model discovery | Works, thin | `/v1/models` gives ids only. `GET /api/tags` gives size, digest, family, parameter size and quantization. |
| Loading | Slow first call | `keep_alive` defaults to 5 minutes ([FAQ](https://docs.ollama.com/faq)). The first call after that reloads the model, which takes seconds to tens of seconds. tern's read timeout is 300 s (`http.rs`), which is enough. Set `keep_alive` longer so the agent doesn't pay this between turns. |
| Concurrency | One at a time | `OLLAMA_NUM_PARALLEL` defaults to 1 ([FAQ](https://docs.ollama.com/faq)), and memory use scales with parallel requests × context. Subagents and schedules that run together will queue behind each other on one model. |

**The key design decision is how to set `num_ctx`.** There are three ways:

1. Run `ollama serve` ourselves with `OLLAMA_CONTEXT_LENGTH=65536` (or a value picked from RAM). Simple, global, and it works with the plain `/v1` provider. Only works if tern starts the server. A server the user started themselves keeps its own setting.
2. Create a tern variant of each model with a Modelfile (`PARAMETER num_ctx N`, `ollama create tern-qwen…`). Works with any server. Costs a second manifest per model (small, the weights are shared) and adds a name the user didn't pick.
3. Switch the Ollama provider to the native `/api/chat`, which takes `options.num_ctx` per request and reports `eval_count`/`prompt_eval_count`. Most control, but it's a second provider implementation to maintain.

Recommendation: option 1 when tern starts the server, and when it didn't, read the loaded context with `/api/ps` and `/api/show` and warn in the UI ("Ollama is using 4k; tern needs 64k") with a one-click fix. Keep option 3 in reserve.

## 2. Engines compared

The owner asked whether Ollama is the right engine, and whether MLX should be primary. Short version of the table below: Ollama is the only option that comes with everything tern would otherwise build (model registry, pull progress, delete, tool-call parsing per model family, a signed app) under a license that allows us to ship it. Others win on speed or control but leave us a lot to build.

| | **Ollama** | **LM Studio** (`lms`, `llmster`, SDK) | **llama.cpp `llama-server`** | **mlx-lm server** (Python) | **mlx-swift-lm** (in-process) | **Jan** / **LocalAI** |
|---|---|---|---|---|---|---|
| License | MIT ([repo](https://github.com/ollama/ollama)) | App under LM Studio's terms: use is for personal or internal business purposes, and no distributing it or integrating it with other software except through published interfaces ([terms](https://lmstudio.ai/terms)). `lms` is reported as MIT *(secondary)* | MIT *(from memory; not re-checked in this pass)* | MIT ([repo](https://github.com/ml-explore/mlx-lm)) | MIT ([repo](https://github.com/ml-explore/mlx-swift-lm)) | Jan: Apache 2.0 with an attribution clause *(secondary)*. LocalAI: MIT, a Docker-style server *(secondary)* |
| Can we ship it in our DMG? | Yes (186 MB `ollama-darwin.tgz`, [release](https://api.github.com/repos/ollama/ollama/releases/latest)). Not needed yet | **No.** Detect only | Yes, and it is small to ship *(size not measured)* | Yes, but it needs a bundled Python | Yes, as a Swift library or helper | Apps, not engines. Not useful to bundle |
| OpenAI-compatible API | `/v1` with tools, streaming, usage. No `tool_choice` ([docs](https://docs.ollama.com/api/openai-compatibility)) | Chat completions, responses, embeddings, tool use, plus a native REST API ([docs](https://lmstudio.ai/docs/developer)) | `/v1/chat/completions`, models, embeddings ([README](https://raw.githubusercontent.com/ggml-org/llama.cpp/master/tools/server/README.md)) | `/v1/chat/completions` and `/v1/models`. Docs warn it is not for production ([server docs](https://raw.githubusercontent.com/ml-explore/mlx-lm/main/mlx_lm/SERVER.md)) | None. We would write the server | Jan: OpenAI-compatible on port 1337 *(secondary)* |
| Tool calling | Per-family parsers built in. Works for tool-capable models; `/api/show` says which | Own parsers. One reported bug with an MLX gpt-oss build ([issue](https://github.com/lmstudio-ai/lmstudio-js/issues/493)) | Needs `--jinja` and a tool-capable chat template. Some models need a template override; parallel calls only on some ([README](https://raw.githubusercontent.com/ggml-org/llama.cpp/master/tools/server/README.md)) | Parser is picked from the chat template. Reported gaps: Qwen 3.5/3.6 emit no `tool_calls` because auto-detection misses their templates, and gpt-oss doesn't stop at `<\|call\|>` ([issue](https://github.com/ml-explore/mlx-lm/issues/1293), [issue](https://github.com/ml-explore/mlx-lm/issues/613)). Status today not checked | Not documented for the core library ([README](https://github.com/ml-explore/mlx-swift-lm)). We would write parsers per family | Jan: sources conflict; MCP is supported *(secondary)* |
| Model download and management API | Best: `/api/pull` (streamed progress), `/api/tags`, `/api/delete`, `/api/ps`, `/api/show` | Load and unload over REST. Download only through the `lms get` CLI ([0.4.0 post](https://lmstudio.ai/blog/0.4.0)) | `-hf user/model[:quant]` downloads from Hugging Face on start. No pull-progress API, no delete, no catalog. Router mode loads from a models dir | `--model <hf repo>` downloads on first use. Lists only what is cached. No pull progress, no delete API | Hugging Face downloader integration; the rest is ours | Apps with their own UI; no use to us |
| Speed on Apple Silicon | v0.30 uses MLX by default on Macs with 32 GB or more, and falls back silently to llama.cpp Metal below that ([write-up, secondary](https://dev.to/jovan_chan_9500711396d4e6/ollama-v030-on-apple-silicon-what-the-stable-mlx-release-actually-changed-from-the-preview-25h7)) | Has both llama.cpp and MLX engines. No parallel requests on MLX as of 0.4.0 | Fast; wins on long prompts (see §3) | Fastest decode on recent chips (see §3) | Same MLX core as mlx-lm | Jan uses llama.cpp, and MLX *(secondary)* |
| What tern builds itself | Little: a manager over HTTP | Detect and a CLI/REST wrapper. Can't ship it, so users must install it | A lot: catalog, download with progress, delete, memory fitting, per-model templates, process lifecycle | A lot: Python packaging, catalog, downloads, delete, parser fixes, model switching | The most: server, parsers, downloads, lifecycle | n/a |

Verdict per engine:

- **Ollama.** Best fit as the primary. It is the only one with a complete management API, so tern only needs a thin manager.
- **LM Studio.** A good "bring your own" provider for people who already use it. The preset exists. We can't bundle it and shouldn't depend on it. Its CLI and REST API are the way to drive it if we add a second engine.
- **llama-server.** The right answer if Ollama's process, port and update cadence become a problem. It is the best embedded choice, but we'd rebuild what Ollama gives for free (the part that costs the most is catalog, downloads and per-model chat templates for tool calls).
- **mlx-lm and mlx-swift-lm.** See §3.
- **Jan and LocalAI.** Apps and servers aimed at end users or Docker. They duplicate what tern's own app is doing. Not recommended. I could not verify LocalAI on macOS at all.

## 3. MLX in depth

MLX is Apple's array framework for Apple Silicon. It has two relevant runtimes: `mlx-lm` (Python) and `mlx-swift-lm` (Swift). It is also what Ollama (v0.30) and LM Studio use for their Mac engines, so "use MLX" doesn't have to mean "run mlx-lm ourselves".

**Speed and memory.**

| Measure | Result | Source |
|---|---|---|
| Decode, MoE models (M4 Pro/Max) | MLX about 130 tok/s; Ollama's old llama.cpp path about 43; raw llama.cpp about 89 | [yage.ai](https://yage.ai/share/mlx-apple-silicon-en-20260331.html) *(community numbers, not controlled)* |
| Decode, dense models | MLX about 1.4–1.6× Ollama's old path, about 1.4–1.8× raw llama.cpp | same |
| Ollama 0.19 preview, Qwen3.5-35B-A3B on an M5 system | Prefill 1810 vs 1154 tok/s, decode 112 vs 58 (NVFP4 vs Q4_K_M on 0.18) | [Ollama blog](https://ollama.com/blog/mlx) |
| Prefill, M1 Max, about 650-token prompt | MLX 13 tok/s effective vs GGUF 20 (94% of MLX's time was prefill) | [yage.ai](https://yage.ai/share/mlx-apple-silicon-en-20260331.html) |
| Long context (30k+), M3 Ultra | MLX decode about 50% slower than llama.cpp with flash attention | same, community report |
| Memory for Qwen3-Coder-30B | 34.7 GB MLX vs 40 GB GGUF (about 13% less) | same |

Why this matters for tern: an agent loop sends a long prompt (instructions, tool list, history) on every turn and writes a short reply. Time is dominated by prefill and cache reuse, not by decode speed. MLX's decode lead matters less than it looks, and llama.cpp is competitive or ahead on long prompts and older chips. Newer chips (M5 neural accelerators) change this in MLX's favor, but I only found one source for that. Gemma models may run better on llama.cpp, Qwen better on MLX (*secondary*). Treat all of this as directional and measure on tern's own prompts before choosing.

**Server and tool calling (mlx-lm).** From the server source: OpenAI-style `tool_calls` (streamed, with `index`), a reasoning field on deltas, a prompt cache (LRU, 10 entries by default) with cached token counts in `usage.prompt_tokens_details`, and continuous batching when no draft model or KV quantization is used ([server.py](https://raw.githubusercontent.com/ml-explore/mlx-lm/main/mlx_lm/server.py)). That is closer to tern's needs than I expected. The weaknesses:
- It serves one model at a time and reloads when the request names another. That is a poor fit for agent plus fast plus decision models on one machine.
- Tool-call parsing depends on matching the model's chat template. The open issues above are about exactly the model families we'd recommend (Qwen, gpt-oss). I did not confirm whether they are fixed.
- The project says the server is not production-ready and only does basic security checks. Binding to loopback with a token is enough for us, but it is a signal about maturity.
- No pull progress, delete or catalog API. We'd build those over the Hugging Face hub: download with progress, the cache directory, delete, and a curated list of `mlx-community` models (quantized 4-bit conversions of popular models; `mlx_lm.convert -q` makes them).

**Python process or in-process Swift?**

| | Python `mlx_lm.server` | `mlx-swift-lm` |
|---|---|---|
| How tern uses it | Daemon spawns a child process and talks `/v1` | A small Swift helper binary (the daemon is Rust, the app is Flutter), or a server we write |
| Shipping in the DMG | Bundle a relocatable Python plus mlx, mlx-lm and their dependencies, and the Metal kernel library; sign every native file inside-out; notarize. [desktop-release.md](../desktop-release.md) today has no Python and no JIT entitlements, because tern's `python` tool is the pure-Rust Monty. A bundled CPython is a new moving part to sign and update. Size not measured | One signed Swift binary; MLX's Metal library still ships with it. Smaller than a Python tree *(estimate)* |
| Maturity | Mature models and quantizations; server is rough | v3.x with breaking changes and 78 open issues at the time I looked ([repo](https://github.com/ml-explore/mlx-swift-lm)). Tool calling not documented. Also bridges Apple's FoundationModels on the macOS 27 SDK, which I didn't evaluate |
| What we write | Catalog, download, delete, parser fixes, process lifecycle | All of that, plus the server and tool-call parsers |

Neither makes sense as the first engine. A Swift helper is the better long-term path if we want a fully in-app engine, because it avoids shipping Python.

**Quantization.** MLX models are usually 4-bit group quantized (mlx-community). Ollama's MLX path also supports NVFP4 and int4 ([Ollama blog](https://ollama.com/blog/mlx)). llama.cpp uses GGUF K-quants (Q4_K_M and others). Quality differences between these at 4-bit weren't measured in anything I found. `--kv-bits` in mlx-lm shrinks the cache but turns off batching.

**Should MLX be the primary local backend? No, not as a separate runtime in v1.**
- On Macs with 32 GB or more, Ollama already runs MLX by default, so those users get most of the MLX speed with Ollama's management API and tool parsers.
- On smaller Macs (8–16 GB), which are the likely majority, Ollama falls back silently to llama.cpp Metal. MLX in Ollama doesn't reach them yet. mlx-lm would, but only by shipping Python and building the whole management layer.
- Tool-call reliability is a bigger risk than 30% more decode speed, and the mlx-lm parser issues hit our likely model picks.
- **Primary: Ollama. Fallback and escape hatch: llama.cpp underneath it (automatic below 32 GB), and optionally `llama-server` if we ever need to own the process.** Keep the door open to MLX direct by putting a small engine trait in the daemon (detect, status, list, pull, delete, start/stop) with Ollama as the first implementation.
- **Revisit direct MLX** after phase 3, with an eval: run tern's tool-use tasks on Ollama (llama.cpp and MLX) and on mlx-lm or a Swift helper, same models, same prompts. If MLX direct wins clearly on small Macs and tool calls hold up, build it as a second engine.

## 4. Getting Ollama onto the machine

Facts: Ollama is MIT licensed ([repo](https://github.com/ollama/ollama)). The latest release is v0.40.16 (2026-11-09). The macOS assets are `Ollama.dmg` at 230 MB, `Ollama-darwin.zip` at 229 MB and `ollama-darwin.tgz` at 186 MB ([release API](https://api.github.com/repos/ollama/ollama/releases/latest)). The Mac app lives in the menu bar, updates itself and registers as a login item ([FAQ](https://docs.ollama.com/faq)). The server listens on 127.0.0.1:11434 by default. Models are stored in `~/.ollama/models` ([FAQ](https://docs.ollama.com/faq)). Homebrew has a formula (`brew install ollama`, CLI only, no menu bar app) and a cask *(secondary: [guide](https://gist.github.com/greenstevester/fc49b4e60a4fef9effc79066c1033ae5))*.

| Option | User effort | Our cost | Verdict |
|---|---|---|---|
| **Detect existing install** (probe `:11434/api/version`, look for `/Applications/Ollama.app` and `ollama` on PATH or `/opt/homebrew/bin`) | None | Small | Always do this first. |
| **Deep-link to the installer** (open `https://ollama.com/download`, then poll until the server answers) | Download, drag, open | Small | Ship in phase 1. It has no signing, size or update cost for us. |
| **Homebrew** (`brew install --cask ollama-app`, run by tern) | One click, if brew exists | Small–medium | Optional second path. Many users don't have brew. Running a package manager for the user needs their approval. Cask name unverified. |
| **Download and install for the user** (fetch `Ollama-darwin.zip` from GitHub, check its sha256, unzip into `~/Applications`, open) | One click | Medium | Best experience without bundling. Ollama's own app is already signed, so there is no signing work for us. We must verify the download and handle a failed Gatekeeper check. The same permission rule as any download applies. |
| **Bundle the `ollama` binary in tern's DMG** | None | Large | Adds 186 MB to a DMG that is small today. We would sign it inside-out with our Developer ID and notarize it ([desktop-release.md](../desktop-release.md) already signs per piece, never `--deep`). We'd own security updates and the release cadence (they ship often). It also collides with a user's own Ollama on port 11434, so we'd run ours on another port. Skip for now. |
| **Embed llama.cpp** (link or bundle `llama-server`) | None | Very large | We would re-own what Ollama gives for free: model downloads and registry, templates per model, tool-call parsing per model family, memory fitting, Metal tuning. Tool-call parsing is the hard part, and it changes with every model family. Skip. |

Recommendation: detect, then download-and-install with consent, with the deep link as the fallback. Never bundle in v1. If many users hit the install step, revisit bundling.

Alternative worth one spike: LM Studio also has an OpenAI-compatible server, and tern already has a preset. It is closed source and licensed for personal use, so we shouldn't redistribute it. Keep it as a "bring your own" option.

## 5. Daemon design

Ollama runs as its own process. The tern daemon should be a thin manager in front of it, so the Flutter app and the TUI/CLI don't each talk to Ollama.

**Ollama calls the daemon would make**

| Need | Ollama call |
|---|---|
| Is it up, which version | `GET /api/version` |
| Installed models | `GET /api/tags` (name, size, digest, details) |
| Loaded models, memory, expiry | `GET /api/ps` (`size_vram`, `expires_at`) |
| Tool support, thinking, vision, native context | `POST /api/show` (`capabilities`, `model_info`) |
| Download | `POST /api/pull`, streamed. Each line is `{status, digest, total, completed}`, and the last is `{"status":"success"}` |
| Remove | `DELETE /api/delete` with `{"model": …}` |
| Unload now | `POST /api/generate` with an empty prompt and `keep_alive: 0` |

(All from the [API reference](https://github.com/ollama/ollama/blob/main/docs/api.md).)

**New daemon endpoints** (names follow the existing `/v1/providers` style; final shape belongs in a spec):

- `GET /v1/local/status`: `{installed, running, version, ollama_managed, models_dir, disk_free, ram_bytes, context_in_use}`.
- `GET /v1/local/models`: installed models from `/api/tags` joined with `/api/show` (cached per digest) and `/api/ps`. Each row carries `tools: bool`, `thinking: bool`, `context`, `size`, `loaded`.
- `GET /v1/local/catalog`: the recommended list for this Mac (see §7), with fit labels.
- `POST /v1/local/pull` `{model}` returns a job id. `DELETE /v1/local/models/{name}`. `POST /v1/local/start`, `POST /v1/local/stop`.
- Pull progress goes out as events on the existing event stream (`local_pull` with `{model, completed, total, phase}`), the same way `providers_changed` and `models_changed` work today ([provider-setup-spec.md](../provider-setup-spec.md) §9). The daemon reads Ollama's progress stream once, so several windows can watch one pull. Pulls must survive the window closing and be cancellable (drop the connection; Ollama resumes partial layers on the next pull).
- On a finished pull, the daemon refreshes the provider's model list and emits `models_changed`.

**CLI.** `tern models` already exists conceptually for providers. Add `tern local status|list|pull|rm|start|stop`, with a progress bar on stdout and `--json`. Keep it under one command group so it doesn't clash with `tern models`.

**Starting `ollama serve`.** On first need (an Ollama-backed model is chosen, or the user clicks Start), the daemon runs `ollama serve` as a child with `OLLAMA_CONTEXT_LENGTH`, `OLLAMA_KEEP_ALIVE`, and, on 16 GB Macs, `OLLAMA_KV_CACHE_TYPE=q8_0` and flash attention (about half the cache memory with little loss, per the [FAQ](https://docs.ollama.com/faq)). Rules:
- If something already answers on 11434, use it and do not restart it. Show a warning if its context is too small.
- If the Ollama menu bar app is installed, prefer opening it. It owns the login item and its updates. Two servers on one port is the failure to avoid.
- Stop the child on daemon exit only if the daemon started it.

**Disk.** Models are 2–45 GB each. Show sizes from `/api/tags`, free space from the volume holding `~/.ollama/models`, and refuse a pull that wouldn't leave a safe margin (say 10 GB). Show total used by tern-recommended models.

**Capability checks in the agent loop.** Before a run, if the chosen model's `/api/show` has no `tools`, fail early with a plain message instead of a mid-run 400. Add `ProviderConfig`-level capability flags only if more providers need them.

**Config.** Keep one `[providers.ollama]` table, as today. The managed-server settings (`manage = true`, context length, keep-alive) belong in a small `[local]` table. Another session is editing provider and model-tier config, so land this after it to avoid a conflict.

## 6. UI design

Follows the tern design system (clean, one short line per option, detail in tooltips).

**First run.** A choice of two cards: "Use a cloud model" (today's flow) and "Run on this Mac". The second shows one of three states:
1. *Ollama not found.* One button: "Install Ollama". Progress, then continue.
2. *Ollama found, not running.* "Start Ollama".
3. *Running.* Go to the model browser.

**Model browser** (a dialog beside the existing Models dialog, or a tab in it):
- A header line: "16 GB Mac · 41 GB free".
- Recommended list for this Mac, one row each: name, size on disk, a one-line job ("Best all-round", "Fast, small"), and badges: Tools ✓, Thinking, Fits. Rows that would not run well are dimmed with "Needs 32 GB".
- Download button becomes a progress bar with speed and a cancel; it keeps running if the dialog closes, and the sidebar shows a small download chip.
- Installed section: size, "In memory" state, Remove (confirm with size freed), Unload.
- "Add by name" field for anything on ollama.com/library, checked with `/api/show` after the pull.

**Choosing per tier.** Another session is adding fast and decision model settings. This UI should reuse the same chain editor ([model_chain_editor.dart](../../apps/desktop/lib/ui/model_chain_editor.dart)) and list local models in it with a "Local" badge. A "Use for…" menu on each installed model sets Agent, Fast or Decision. One-click "Set up all three for this Mac" picks a preset (see §7).

**Warnings, in plain words:**
- No tool support: "This model can't use tools, so it can't run tasks. It can still chat." Block it for the Agent tier and Decision tier. Allow it for chat-only.
- Context too small: "Ollama is using 4k of memory for context. tern needs 64k. Fix." Fix restarts the managed server, or shows the one `launchctl setenv` line when it's the user's server.
- Model too big for this Mac: "May be slow or crash. This Mac has 16 GB."
- Slow first reply: "Loading the model. About 10 s the first time."

I can't run Flutter locally (see CLAUDE.md), so UI work needs PR CI plus the owner's review in the app.

## 7. Which local models are realistic

Caveats first. I did not benchmark anything. The numbers below combine Ollama's published sizes with blog roundups ([Morph](https://www.morphllm.com/best-ollama-models), [Threat Frontier](https://threatfrontier.com/articles/best-local-llm-16gb-vram-rtx-5060-ti-mac-mini-m4), [OpenClawDC](https://openclawdc.com/blog/best-local-llms-32gb-ram/), [ReMynd](https://remyndai.com/blog/best-local-llm-for-mac-2026/)) *(secondary, and they disagree)*. Model tags such as `qwen3.8` and `gemma4` came from those pages and Ollama's tools list ([ollama.com/search?c=tools](https://ollama.com/search?c=tools)). Treat every row as a starting list and verify tool-call quality with a tern eval before shipping it.

macOS gives the GPU only part of unified memory by default (about 24 GiB of 32 GB *(secondary, OpenClawDC)*), and the context cache grows on top of the weights. So the usable model size is roughly RAM minus 8 GB for macOS and apps, minus the cache.

| Mac RAM | Agent tier (tool loop) | Fast tier | Decision/routing tier | Notes |
|---|---|---|---|---|
| **8 GB** | Not recommended. Small models (about 3B) call tools unreliably over long runs. Allow chat-only and light tasks (`granite4.2:3b`, `qwen3.5` small). | `qwen3.5:0.8b`–`granite4.2:3b` | Yes, if the decision is a short yes/no or pick-one | After macOS, about 4 GB is left for the model and a 64k cache doesn't fit. Say so plainly in the UI. |
| **16 GB** | `gpt-oss:20b` (14 GB, tools, Apache 2.0) is the popular pick but leaves little for context. A 9B model such as `qwen3.5:9b` (6.6 GB, tools) is safer with a 32–64k window. | A 3–9B model | Yes | Pick context by memory: 32k on 16 GB. Check `ollama ps` shows 100% GPU. |
| **32 GB** | `qwen3.6:27b` / `qwen3.8:27b` (17–18 GB, tools) or `qwen3-coder:30b` (19 GB). `gpt-oss:20b` is praised for clean tool calls over long runs *(secondary)*. | 9B model, kept loaded | Yes, with the fast model | Two models loaded at once (agent plus fast) fits only if their sum stays under about 24 GiB. Otherwise Ollama swaps them and each switch costs a reload. |
| **64 GB** | `llama3.3:70b` (43 GB, tool calling) or a 27–30B model at higher quantization | 9B–27B, kept loaded | Yes | Can hold an agent model and a fast model together. |

**Does the decision/routing tier run well locally?** Yes, and it is the best first local use. Decisions are short prompts with a short answer, so a small model is fast and cheap. The risks: structured output must be reliable, so test it per model; and a model that is swapped in and out of memory adds seconds per decision. Keep the decision model small and pinned with a long `keep_alive`. The agent tier is the hard one: long tool loops are where small models drift, so show the "may be unreliable" note on 8 GB and for any model under about 9B.

**Quality gap.** Local 20–30B models are well behind frontier cloud models on long multi-step tool use. The pitch should be "private and offline, good for everyday tasks", not "same as Claude". A tern eval set (twenty tasks, pass/fail on tool use) would give honest per-model labels. That is the most valuable follow-up beyond the product work.

## 8. Effort, risks and recommendation

| Phase | What | Size |
|---|---|---|
| **0. Make the existing preset honest** | Read `context_length` and `capabilities` from `/api/show`. Omit `tools` instead of sending `tool_choice: "none"` for Ollama. Stop assuming 32 768: use `/api/ps` or the configured context and warn when it's below 64k. Set a long `keep_alive` where possible. Doc note on 4k default | **S** |
| **1. Detect, install, start** | Engine trait in the daemon (detect, status, list, pull, delete, start/stop; Ollama first), status endpoint, detect/open installer, managed `ollama serve` with env, "Start Ollama" in UI, first-run "Run on this Mac" card | **M** |
| **2. Pull, list, remove** | Daemon endpoints and events, `tern local …` CLI, model browser dialog with progress, disk checks, installed list | **M** |
| **3. Tiers and guidance** | Recommended list by RAM (as data), "Use for…" per tier (after the other session's tier settings land), tool/thinking/fit badges, one-click preset per RAM size | **M** |
| **4. Quality** | Eval set to label models honestly, tune per RAM, handle two-model memory swapping, optional native `/api/chat` | **M–L** |
| **5. Second engine (only if the eval says so)** | Engine trait is in phase 1. Add `llama-server` or an MLX helper (bundled Python `mlx_lm.server`, or a Swift helper on `mlx-swift-lm`): catalog, downloads, delete, tool-call parsing, lifecycle, signing | **L** each |
| **5b. Bundle Ollama in the DMG** | Sign and notarize the binary, own its updates, move it off port 11434 | **L**. Not recommended now |

Phases 0–3 together are roughly a few weeks of focused work for one person, mostly in the desktop app. The daemon and CLI part is small because Ollama does the heavy lifting. This is my estimate, not measured.

**Risks**
- **Silent context truncation** is the worst one. Phase 0 fixes it. Without it, local mode looks broken in a confusing way.
- **Tool-call quality** differs a lot by model and quantization. We can promise "runs", not "works as well as cloud". Mitigate with the eval set and honest labels.
- **Two servers on port 11434.** The user's own Ollama and ours can conflict. Always probe first and reuse.
- **Disk and memory pressure.** A pull of 20+ GB, or a model that doesn't fit, hurts the whole Mac. Check before pulling and warn before loading.
- **Model churn.** Tags are renamed or replaced often. Keep the catalog as data and show a clear error for a tag that no longer exists.
- **Downloading an installer for the user.** Must be consented, checksum-verified, and shown in plain words. If we skip it, the deep link covers us.
- **API drift.** Ollama is at 0.x and ships often. Pin the endpoints we use, and show a "version too old" message using `/api/version`.
- **Single-player scope.** `OLLAMA_HOST` is loopback by default. tern should keep it that way and never expose it, in line with the local-only decision.

**Recommendation.** Do it, in this order: phase 0 now (it also fixes today's Ollama users), then 1 and 2 as one feature, then 3 after the fast/decision tier settings merge. Use Ollama as an external, user-visible dependency: detect it, install it with consent, drive it over HTTP. Don't bundle or embed in v1. Keep LM Studio as a bring-your-own provider (we can't ship it). After phase 3, run the engine eval from §3 before building a direct MLX or `llama-server` engine. Start with the decision and fast tiers on 16 GB Macs, where local models are good enough today, and be open that the agent tier on 8 GB is chat-only.

**Open questions for the owner**
0. Is Ollama-as-dependency acceptable, or must tern ship its own engine? If it must, I'd pick a Swift helper on `mlx-swift-lm` or `llama-server`, not bundled Python, and the cost is phase 5 (L) done before the rest.
1. Is "download and install Ollama for me" acceptable, or deep link only?
2. Should tern manage the Ollama server (set context, keep-alive), or only use one the user runs?
3. Is "no cloud at all" a first-run path, or one more option in Models?
