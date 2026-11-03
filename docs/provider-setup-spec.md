# Provider, API key and model setup

Status: P0 (engine and API) built; P1 (TUI, CLI) and P2 (desktop) not started. Covers the engine, TUI, desktop app and CLI.

## 1. Problem

Today, getting tern to talk to a model takes four separate steps, and nothing tells you about the next one:

1. Hand-edit `config.toml` to add a `[providers.x]` table with the right `kind`, `base_url` and `api_key_secret`.
2. Put the key in `secrets.toml` (`/secrets` or the Secrets dialog). You have to know the secret's name, and the value is only shown as a character count.
3. Set `default_model = "provider:model"` with `tern config set`. You have to know the exact model id.
4. If the daemon is running, restart it (`RemoteBackend::reload` says so). The TUI's `/reload` only works when it runs the engine itself.

Nothing checks the key until the first message fails. Models are free text everywhere (`/model p:m`, the desktop `_ModelDialog`), and `check_model` only checks that the provider name exists. `tern doctor` already calls each provider's models endpoint, but only to check the key works; the model list it gets back is thrown away.

## 2. Goals

- **One guided flow**: choose a provider type, paste a key, pick a model. It works the same in the TUI, the desktop app and `tern setup`.
- **Check the key straight away** and explain failures in plain words.
- **Get the model list from the provider** whenever we can, show it as a searchable dropdown, and always allow free text.
- **No restart**: adding a provider or changing the default takes effect on the next run, daemon included.
- **Keys stay safe**: they never reach the transcript, input history, logs, URLs or the model.

**Scope**: only the two existing `ProviderKind`s, **Anthropic** and **OpenAI-compatible**. There is no catalog of named providers (OpenRouter, Groq, …): an OpenAI-compatible provider is a name, a base URL and an optional key.

Out of scope: per-provider presets, detecting local servers, OAuth, per-role models, and storing keys in the OS keychain.

## 3. The two provider types

| | Anthropic | OpenAI-compatible |
|---|---|---|
| Asks for | key | name, base URL, key (optional) |
| Default name | `anthropic` | the user's choice, suggested from the URL host (`api.openai.com` → `openai`, `openrouter.ai` → `openrouter`, `localhost:11434` → `ollama`, otherwise `custom`) |
| Base URL | `ProviderConfig::ANTHROPIC_BASE_URL`, editable under "Advanced" | required; placeholder `https://api.openai.com/v1` |
| Secret name | `ANTHROPIC_API_KEY` | `<NAME>_API_KEY` in upper snake case (`openrouter` → `OPENROUTER_API_KEY`); none when the key is left blank |
| Key link | console.anthropic.com/settings/keys | none (we can't know where it comes from) |
| Key format hint | starts with `sk-ant-` | none |

The base URL field gets one piece of help: if the URL doesn't end in `/v1` (or another version path) and `{url}/models` 404s, retry `{url}/v1/models` once. If that works, offer to change the URL to the `/v1` form. This catches the most common mistake without a catalog.

A local server needs no key (for example `http://localhost:11434/v1` for Ollama). Leaving the key blank is a normal choice, not an error.

No new config fields are needed. Existing configs work unchanged.

## 4. Getting the model list

### 4.1 Sources

`tern_providers::list_models(name, &ProviderConfig, key) -> Result<Vec<ModelInfo>, ListError>`:

| Kind | Request | Notes |
|---|---|---|
| `anthropic` | `GET {base}/v1/models?limit=1000`, follow `has_more` / `last_id` | `display_name`, `created_at`; read `max_input_tokens` when the response has it |
| `openai_compat` | `GET {base}/models`, bearer key if set | `id`, and `created` when present. Read the optional `name` and `context_length` fields when present (OpenRouter sends them), but never require them |

```rust
pub struct ModelInfo {
    pub id: String,              // what goes after "provider:"
    pub name: Option<String>,    // display name, when the provider gives one
    pub context_window: Option<u64>,
    pub created: Option<i64>,    // unix seconds, for "newest first"
}
```

`ListError` is a small enum that clients can turn into wording: `Unauthorized`, `Forbidden`, `NotFound` (no models endpoint), `RateLimited`, `Unreachable(String)`, `Unexpected(u16)`, `Malformed`.

`doctor::probe` is rewritten on top of `list_models`, so the endpoint, headers and error mapping live in one place.

### 4.2 Filtering and order

- For `openai_compat`, hide ids that are clearly not chat models: those containing `embed`, `tts`, `whisper`, `dall-e`, `moderation`, `transcribe` or `image`. This is one fixed list, applied to every OpenAI-compatible provider. The free-text row (§7) still lets the user pick a hidden model.
- Order: newest `created` first, then alphabetical. Anthropic returns models newest first already.
- If listing fails or returns an empty list, the picker shows free text only, with one line saying why.

### 4.3 Caching

- In memory per provider, kept for 10 minutes. `refresh=true` skips the cache.
- On disk at `$TERN_HOME/cache/models.json` (`{provider: {fetched_at, models}}`), so pickers open at once, and still work offline, with the last known list. Stale entries are served right away while a fresh list loads in the background; clients get a `models_changed` event when it lands.
- The cache is dropped for a provider when its key, base URL or config entry changes.

### 4.4 Model references get easier

- `check_model` stays strict about the provider. It adds a **soft** check against the cached list: a model that isn't listed is still allowed, and the reply carries `warning: "not in anthropic's model list"` for clients to show. It never blocks.
- **Provider can be left off**: `/model claude-…` (no `provider:`) resolves when exactly one configured provider lists that id. If several do, the error names them: `"gpt-4o" is offered by openai and openrouter; use openai:gpt-4o or openrouter:gpt-4o`. Resolution happens in the engine (`Runtime::resolve_model`), so every client gets it.

## 5. API keys

### 5.1 Entering a key

Every client uses the same steps:

1. Ask in plain words: "Paste your Anthropic API key". Anthropic also gets the line "Don't have one? Get a key ↗". For OpenAI-compatible: "API key (leave blank if your server doesn't need one)".
2. Accept a paste. Show the key masked, never as a bare character count: the first 6 characters, an ellipsis, and the last 4 (`sk-ant…a1B2`). The desktop app has a **Show** toggle; the TUI does not.
3. **Verify before saving**: call `list_models` with the candidate key. On success, save it and go straight to choosing a model with the list we just fetched. On failure, keep the field filled so the user can fix it, and show the error from §5.3.
4. Save the key under the secret name from §3, so the user never has to choose one. The secret name is shown only in the advanced Secrets view.

### 5.2 Cleaning up the paste

`tern_core::secrets::normalize_key`, applied before verifying:

- Trim whitespace and newlines, and remove surrounding quotes or backticks.
- Remove a leading `export NAME=` or `NAME=` (only for upper-case names with an underscore, so `=` padding in a key is kept), and a `Bearer ` prefix.
- A key with spaces inside is rejected: "That looks like more than one key. Paste just the key."
- Anthropic only: if the key doesn't start with `sk-ant-`, warn but still verify ("Anthropic keys usually start with sk-ant-").

### 5.3 Error wording

| Result | Message |
|---|---|
| 401 | "{name} rejected that key. Check you copied all of it." |
| 403 | "The key works, but this account can't list models. Check its permissions or billing." |
| 429 | Counts as **valid**: "Key works (rate limited right now)." Save it; the picker falls back to free text. |
| 404 on models | Counts as valid if the server answered (after the `/v1` retry from §3). The picker shows free text only. |
| Unreachable | "Couldn't reach {host}: {cause}." For a loopback host, add: "Is the server running?" |

### 5.4 Environment variables win

Secrets are read from the environment before `secrets.toml`. If the secret name is already set in the environment:

- Offer "**Use the key from your environment** (`ANTHROPIC_API_KEY`, `sk-ant…a1B2`)" and verify that key; no paste needed.
- If the user pastes a different key anyway, warn: "`ANTHROPIC_API_KEY` in your environment overrides keys saved here. Unset it, or the saved key won't be used." `SecretStatus` already records the source, so clients can show this.

## 6. Flows

### 6.1 Setup flow (shared shape)

```
Anthropic ─────────────▶ Key ──────────────────────▶ Verify ─▶ Choose model ─▶ Done
OpenAI-compatible ─▶ Base URL + name ─▶ Key (optional) ─┘                │
                                                                         └─ "Make this the default" (on by
                                                                            default when there is no usable default)
```

- **Configured providers** are listed with a status (`ready`, `key missing`, `key rejected`, `unreachable`). Choosing one opens **Change key / Choose default model / Remove**.
- **Anthropic** can be added only once without renaming. Adding a second asks for a name.
- **Name collisions** for OpenAI-compatible providers: the suggested name gets `-2`.
- **Removing** a provider asks for confirmation, and also offers to delete its key, unless something else uses that secret (`Config::secret_uses`).

### 6.2 First run

"No usable model" means `default_model` is unset, its provider isn't configured, or that provider's key is missing. When there is no usable model:

- **TUI**: opens the setup panel at start, instead of letting the first message fail. Esc closes it, and the status bar shows `no model — /setup`.
- **Desktop**: the conversation area shows "Connect a model to get started", with the two provider choices inline.
- **`tern config init`**: now ends with `Next: run \`tern setup\``.
- **`tern doctor`**: when there are no providers, the hint becomes `run \`tern setup\``.

## 7. Clients

### 7.1 TUI

- **`/setup`** (alias **`/providers`**) opens a panel built like `SecretsPanel`, with modes `List → Kind → Endpoint (URL, name) → Key → Verifying → Model`. ↑↓/jk to move, Enter to choose, Tab to move between fields, Esc to go back one step, q to close from the list.
- **Key mode** uses the existing masked input, but shows `sk-ant…a1B2` instead of a character count. Bracketed paste must be on, so newlines inside a paste don't submit early.
- **`/model` with no argument** opens a picker instead of only printing the model:
  - Models grouped by provider; the current model and recent models (the last 5 used, across sessions) at the top.
  - Typing filters with a fuzzy match on id and display name. The last row is always `Use "<typed text>"`, so free text is one Enter away.
  - Enter sets the model for this session. `ctrl+d` sets it as the default as well.
  - A provider whose list failed shows one dim row: `openrouter — couldn't list models (key rejected) · /setup to fix`.
- **`/model <text>`** keeps working, with provider-less resolution (§4.4). Tab completes from the cached lists.
- **Remote TUI**: `RemoteBackend` implements the new methods over the API (§9), so the attached TUI has the same panels. Its `reload` calls `POST /v1/config/reload` instead of telling the user to restart the daemon.

`Backend` trait additions (both the embedded and remote backends implement these):

```rust
fn providers(&self) -> BackendResult<Vec<ProviderStatus>>;
fn models(&self, provider: Option<&str>, refresh: bool) -> BackendResult<Vec<ProviderModels>>;
fn verify_provider(&self, draft: &ProviderDraft, key: Option<&str>) -> BackendResult<Verified>;
fn add_provider(&self, draft: &ProviderDraft, key: Option<&str>, default_model: Option<&str>) -> BackendResult<ProviderStatus>;
fn remove_provider(&self, name: &str, remove_secret: bool) -> BackendResult<()>;
fn set_default_model(&self, model: &str) -> BackendResult<()>;
```

`ProviderDraft` is `{ kind, name, base_url, use_env_key }`. Verification makes a network call, so the TUI runs it as an `Effect` off the input loop and shows a spinner, like runs.

### 7.2 Desktop app

- **Model chip** (conversation header): replace `_ModelDialog`'s text field with a popover combobox:
  - Search field on top, focused when it opens.
  - Sections per provider, each row showing the display name (when there is one), the id in mono, and the context window when known. Recent models are pinned at the top.
  - The last row is `Use "<typed text>"` for free text. The footer has **Manage providers…** and a refresh icon.
  - A soft `warning` from `check_model` shows as a note under the chip.
- **Settings → Models** (the gui-spec §4.5 tab): the provider list with status chips, **Add provider**, the default-model combobox (same widget), and per provider **Change key**, **Refresh models** and **Remove**. ⌘, opens Settings on this tab; the current Secrets dialog becomes an "Advanced" **Secrets** tab.
- **Add provider sheet**:
  - Two cards: **Anthropic** ("Claude models") and **OpenAI-compatible** ("OpenAI, OpenRouter, Ollama, or any server with the OpenAI API").
  - OpenAI-compatible asks for the base URL, with the name filled in from it and editable.
  - Then the key step: one obscured `TextField` with a **Paste** button (which reads the clipboard only when clicked) and **Show**. It verifies automatically 400 ms after a paste, or on Enter, and shows the result inline: a check and "Key works — 42 models", or the §5.3 message.
  - The model step uses the same combobox, with a "Make default" switch.
- **Welcome state** (§6.2) uses the same sheet content inline. The sidebar's `no default model` line becomes a button that opens setup.

`tern_client` additions: `providers()`, `models({provider, refresh})`, `verifyProvider(draft, key)`, `addProvider(...)`, `removeProvider(...)`, `setDefaultModel(...)`, plus model classes in a new `providers.dart`.

### 7.3 CLI

- **`tern setup`**: the same flow, with plain line prompts and hidden key input. It works against the embedded runtime, or against the daemon when one is running.
- **`tern providers [list | add anthropic | add openai-compat --url URL [--name N] | rm NAME]`**, for scripts. `add` reads the key from stdin when stdin isn't a TTY, so `pbpaste | tern providers add anthropic` works and the key never appears in the process list.
- **`tern models [provider] [--refresh] [--json]`** prints the lists.

## 8. Security

- Keys travel only in request bodies, never in URLs or query strings. Over remote core they go only over TLS, as today.
- Keys that were verified but not saved are dropped after the request. Logs record only the secret name. A newly saved key joins the redaction list straight away, not only at the next start.
- Provider writes and `verify` need **Owner** scope (add `/v1/providers` to `OWNER_ONLY`). `GET /v1/models` stays **Read**, so chat-only devices can still pick models. `verify` is rate limited to 10 per minute per device.
- The TUI key field stays out of input history, as `SecretsPanel` already does. The desktop field turns off autocorrect and suggestions, and clears its controller on close.
- Base URL check: `http` is allowed only for loopback and private-range hosts. Anything else must be `https`, so a key is never sent in cleartext by mistake.

## 9. Engine changes

### 9.1 Config writes as a library

Move the `toml_edit` logic out of `config_cmd::set` into `tern_core::config::edit` (`set_value`, `upsert_provider`, `remove_provider`), keeping comments and layout. Every write goes through `Config::parse` validation before the atomic rename. The CLI, TUI and daemon all call this one code path.

### 9.2 Reloading providers without a restart

`Runtime` keeps the config and the provider factory behind an `ArcSwap` (or `RwLock`). `Runtime::reload_providers()` re-reads `config.toml` and swaps in new `providers` and `default_model`; runs that are already going keep the provider they started with. Every provider write calls it. Reloading tools, MCP and the gateway still needs a full reload, which is out of scope. `POST /v1/config/reload` reloads providers only, and its response says so.

### 9.3 API

| Method | Path | Scope | Purpose |
|---|---|---|---|
| GET | `/v1/providers` | Read | configured providers: kind, base URL, secret name, key source (env / file / missing), last list status |
| POST | `/v1/providers/verify` | Owner | `{kind, base_url?, key?, use_env_key?}` → `{ok, error?, suggested_base_url?, models[]}`. Saves nothing |
| POST | `/v1/providers` | Owner | `{kind, name?, base_url?, key?, use_env_key?, default_model?}`: verify, save the secret, write the config, reload |
| PATCH | `/v1/providers/{name}` | Owner | change key or base URL (verifies first) |
| DELETE | `/v1/providers/{name}?remove_secret=` | Owner | |
| GET | `/v1/models?provider=&refresh=` | Read | `[{provider, status, error?, fetched_at, models[]}]` |
| PUT | `/v1/config/default_model` | Owner | `{model}`, resolved and checked like `set_model` |
| POST | `/v1/config/reload` | Owner | reload providers |
| POST | `/v1/models/check` | Chat | unchanged path; now also returns `{resolved, warning?}` |

Events: `providers_changed` (any write) and `models_changed` (a fresh list has landed). Add `Info.setup_needed: bool`, so clients can show §6.2 without a second call.

## 10. Phases

| Phase | Scope | Done when |
|---|---|---|
| **P0 engine** | `list_models` and cache, `normalize_key`, the `/v1` URL retry, config edit library, provider hot reload, API (§9.3), `doctor` rewritten on `list_models` | Tests against a local mock HTTP server cover Anthropic pagination, 401/403/404/429, the openai_compat filter, the `/v1` retry, cache staleness and the env-key override. Adding a provider through the API makes the next run use it with no restart |
| **P1 TUI and CLI** | `/setup`, the `/model` picker, provider-less `/model`, first-run panel, remote backend parity, `tern setup / providers / models` | Starting from an empty `TERN_HOME`, `tern` → paste key → pick model → first reply, with no file edits; `App` state tests for every panel mode |
| **P2 desktop** | model combobox, Settings → Models, add-provider sheet, welcome state | The same empty-home test in the app; widget tests for the combobox's free-text row and the key-field states |
| **Later** | named presets (key links, prefixes), local-server detection, OAuth, per-model context windows in compaction, keychain storage | separate specs |

## 11. Open questions

1. **Default-model changes**: should changing the default also move existing sessions that still use the old default? The proposal is no: sessions keep their model, matching today's behaviour.
2. **Hidden models**: is a fixed filter list (§4.2) acceptable, or should the picker have a "show all" toggle instead of hiding anything?
