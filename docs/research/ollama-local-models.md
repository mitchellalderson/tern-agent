# All-in-one local models with Ollama

Research, 2026-11-18. No product code changed.

**Question.** tern is local and single-player. How hard is it to manage local models from the app, so tern runs with no cloud model at all?

**Short answer.** The first version is medium effort, not large. The Ollama connection already works. What's missing is the management layer: find or install Ollama, pull and delete models, and fix a few silent failures. Don't embed llama.cpp. Don't bundle Ollama in the DMG at first. Detect it, install it with one click if it's missing, and drive it over its HTTP API.

Some claims below come from blog posts, not Ollama's docs. They are marked *(secondary)*. Model names move fast (this ecosystem renames models monthly), so ship the recommended list as data that can be updated without a release.

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

## 2. Getting Ollama onto the machine

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

## 3. Daemon design

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
- `GET /v1/local/catalog`: the recommended list for this Mac (see §5), with fit labels.
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

## 4. UI design

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

**Choosing per tier.** Another session is adding fast and decision model settings. This UI should reuse the same chain editor ([model_chain_editor.dart](../../apps/desktop/lib/ui/model_chain_editor.dart)) and list local models in it with a "Local" badge. A "Use for…" menu on each installed model sets Agent, Fast or Decision. One-click "Set up all three for this Mac" picks a preset (see §5).

**Warnings, in plain words:**
- No tool support: "This model can't use tools, so it can't run tasks. It can still chat." Block it for the Agent tier and Decision tier. Allow it for chat-only.
- Context too small: "Ollama is using 4k of memory for context. tern needs 64k. Fix." Fix restarts the managed server, or shows the one `launchctl setenv` line when it's the user's server.
- Model too big for this Mac: "May be slow or crash. This Mac has 16 GB."
- Slow first reply: "Loading the model. About 10 s the first time."

I can't run Flutter locally (see CLAUDE.md), so UI work needs PR CI plus the owner's review in the app.

## 5. Which local models are realistic

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

## 6. Effort, risks and recommendation

| Phase | What | Size |
|---|---|---|
| **0. Make the existing preset honest** | Read `context_length` and `capabilities` from `/api/show`. Omit `tools` instead of sending `tool_choice: "none"` for Ollama. Stop assuming 32 768: use `/api/ps` or the configured context and warn when it's below 64k. Set a long `keep_alive` where possible. Doc note on 4k default | **S** |
| **1. Detect, install, start** | Status endpoint, detect/open installer, managed `ollama serve` with env, "Start Ollama" in UI, first-run "Run on this Mac" card | **M** |
| **2. Pull, list, remove** | Daemon endpoints and events, `tern local …` CLI, model browser dialog with progress, disk checks, installed list | **M** |
| **3. Tiers and guidance** | Recommended list by RAM (as data), "Use for…" per tier (after the other session's tier settings land), tool/thinking/fit badges, one-click preset per RAM size | **M** |
| **4. Quality** | Eval set to label models honestly, tune per RAM, handle two-model memory swapping, optional native `/api/chat` | **M–L** |
| **5. Optional** | Bundle Ollama in the DMG, or embed llama.cpp | **L** each. Not recommended now |

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

**Recommendation.** Do it, in this order: phase 0 now (it also fixes today's Ollama users), then 1 and 2 as one feature, then 3 after the fast/decision tier settings merge. Use Ollama as an external, user-visible dependency: detect it, install it with consent, drive it over HTTP. Don't bundle or embed in v1. Start with the decision and fast tiers on 16 GB Macs, where local models are good enough today, and be open that the agent tier on 8 GB is chat-only.

**Open questions for the owner**
1. Is "download and install Ollama for me" acceptable, or deep link only?
2. Should tern manage the Ollama server (set context, keep-alive), or only use one the user runs?
3. Is "no cloud at all" a first-run path, or one more option in Models?
