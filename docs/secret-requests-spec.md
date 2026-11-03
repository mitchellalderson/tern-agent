# Secret requests

Status: spec, not built. Covers the engine, TUI, desktop app and Telegram. Pairs with [mcp-management-spec.md](mcp-management-spec.md), whose add form uses the same secret field.

## 1. Problem

When the agent writes a connector that declares `capabilities.secrets = { "WEATHER_API_KEY" }`, the user has to find out the secret's name and then set it themselves:

- `connector_test` fails with `secret "WEATHER_API_KEY" is not set` plus `SECRETS_HELP`, which tells the model to explain environment variables, `~/.tern/secrets.toml` and `/reload`.
- The user then opens `/secrets` or the desktop Secrets dialog, types the name exactly, pastes the value, and tells the agent to try again.
- Many users just paste the key into the chat instead. It then sits in the transcript, the journal and the provider's logs.

The agent knows exactly which secret it needs and why. It should be able to ask for it, and the user should be able to type it into a field the agent can't read.

## 2. Goals

- **The agent can open a secret prompt** with a new `secret_request` tool. The run waits while the user fills it in, as it does for an approval.
- **The agent never sees a value.** The tool result says only `set`, `already_set`, `skipped` or `timed_out` for each name.
- **The same field everywhere**: an inline card in the desktop conversation, an inline prompt in the TUI, and a pointer from Telegram to an app that can take the value.
- **Nothing else changes**: `secrets.toml`, environment variables, `/secrets` and the Secrets dialog work as now. The prompt is another way to write the same file.

Out of scope: spotting keys the user pastes into the chat (see §9), OAuth flows, and OS keychain storage.

## 3. The `secret_request` tool

```json
{
  "name": "secret_request",
  "input": {
    "names": ["WEATHER_API_KEY"],
    "reason": "The weather connector sends this as a bearer token to api.weather.com.",
    "replace": false
  }
}
```

| Field | Rules |
|---|---|
| `names` | 1–5 names, each passing `secrets::check_name`. |
| `reason` | Required, at most 300 characters. Shown to the user as the agent's words (§5.1). |
| `replace` | Optional, default `false`. Without it, names that are already set aren't asked for. With it, the user is asked whether to replace them, for example after the provider rejected the key. |

**Result**, journaled and shown to the model:

```json
{"WEATHER_API_KEY": "set"}
```

Each name is one of `set`, `already_set`, `skipped` or `timed_out`. Nothing about the value is returned, not even its length or prefix. A result where something was skipped adds one line: "The user chose not to provide it. Don't ask for it in the chat."

**Behaviour**

- If every name is already set and `replace` is false, the tool returns `already_set` at once without asking.
- Policy: `requires_approval: false`, because the prompt is itself the user's decision, and `idempotent: true`. A replayed call after a crash asks again only for names that are still missing.
- `allow_always` doesn't apply, since no approval is involved.
- Sub-agents can call it. Their prompts are routed to the root session, as their approvals are, labelled with the sub-agent's name.

### 3.1 Prompt changes

- `SECRETS_HELP` becomes: "If a secret is missing, call `secret_request` with its name and why it's needed. The user enters it in a private field you never see. Never ask the user to paste a secret into the chat." It no longer mentions `/reload`, because secrets set through the API apply straight away.
- `connector_test` and `connector_activate` errors for a missing declared secret name the tool: `secret "WEATHER_API_KEY" is not set; call secret_request to ask the user for it`.
- The connector-writing guide in `tools.rs` gets the same line, so the usual order is: write the draft, `secret_request` for its secrets, then `connector_test`.

## 4. Engine

### 4.1 Routing

A `SecretPrompter` trait sits alongside `ApprovalHandler`:

```rust
pub struct SecretRequest {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub names: Vec<SecretAsk>,        // { name, already_set, source, used_by }
    pub reason: String,
    pub replace: bool,
    pub parent_run_id: Option<String>,
    pub agent_label: Option<String>,
}

#[async_trait]
pub trait SecretPrompter: Send + Sync {
    /// Resolves when the user answers. The values are written by whoever
    /// collected them; only the outcome per name comes back here.
    async fn ask(&self, req: &SecretRequest) -> BTreeMap<String, SecretOutcome>;
}
```

`used_by` comes from `Config::secret_uses` plus connector drafts and active connectors (`Connectors::secret_uses`, extended to drafts). This lets the prompt say what will read the value (§5.1).

The daemon's `Router` routes it like approvals:

- **API clients** (TUI attached or embedded, desktop): published on a channel and listed for late clients, as `ApiApprovals` does.
- **A run delivering to Telegram**: Telegram never collects the value, because it would stay in the chat history. The bot sends "tern needs `WEATHER_API_KEY` for the weather connector. Enter it in the tern app or with `/secrets WEATHER_API_KEY` in the TUI." The request also goes to API clients, so whichever device the user opens can answer it.

**When nobody is listening.** Approvals are denied straight away when no client is attached. Secret requests wait instead, because the user is likely to open the app in response: desktop notifications and the Telegram pointer are how they find out. The wait is capped at `secret_request_timeout_secs` (default 900); after that the result is `timed_out`. Cancelling the run cancels the prompt.

**Non-interactive runs** (`tern run` without a TTY, schedules with no client attached) use the same timeout. With `--yes` they don't wait at all: names already set return `already_set`, and the rest return `skipped` with "no one was available to enter it".

### 4.2 Writing the value

Clients answer with the values. The engine:

1. Checks each name was part of the request. Extra names are rejected, so a request for `FOO` can't be used to write `ANTHROPIC_API_KEY`.
2. Applies `normalize_key` (provider-setup-spec §5.2): trims whitespace and removes quotes, `NAME=` and `Bearer `.
3. Writes each value through the existing `Secrets::set` path, so it goes into `secrets.toml` with mode 0600 and joins the redaction list straight away.
4. Resolves the request with the outcomes only.

The values never touch the journal, events, logs or the request record. A name set in the environment is handled as in provider-setup-spec §5.4: the prompt says the environment value wins and offers to keep it.

### 4.3 Events and API

`AgentEvent::SecretRequest { run_id, id, names, reason, outcome: Option<BTreeMap<..>> }` is emitted when the request opens (with no outcome) and again when it resolves. The transcript shows it as a card, like `Approval`.

| Method | Path | Scope | Purpose |
|---|---|---|---|
| GET | `/v1/secret-requests` | Read | pending requests (resync for late clients) |
| POST | `/v1/secret-requests/{id}` | Owner | `{values: {NAME: value}, skip?: [NAME]}`; names left out count as skipped |

Answering needs Owner scope because it writes secrets, the same as `PUT /v1/secrets`. A Chat device sees the card with "Enter this on an owner device" and no fields. `/v1/secret-requests` goes in `OWNER_ONLY`, but `GET` should stay Read, so `required_scope` gets a method-aware exception for it. That also fits `/v1/mcp` and `/v1/providers`, which need the same split.

## 5. Clients

### 5.1 What every prompt shows

- **Who's asking**: "The agent is asking for a secret". For a sub-agent: "Sub-agent *research* is asking…".
- **Why**: the `reason`, quoted and labelled as coming from the agent, so a misleading reason can't pass as tern's own text.
- **What will read it**, from `used_by`: "Used by connector *weather* (draft) · reaches api.weather.com". If nothing declares the name: "Nothing uses this secret yet." That flags an odd request.
- **Per name**: a masked field, the current state (`not set`, `set in secrets.toml`, `set in environment`), and for `replace` a "Replace the current value" note.
- **Actions**: **Save** and **Skip**. Skip resolves at once, so the agent can carry on and explain.

### 5.2 Desktop

- An inline card in the conversation at the tool call's position, styled like the approval card. Each field has **Paste** (reads the clipboard only when clicked) and **Show**, with autocorrect and suggestions off. The controller is cleared when the card closes.
- After Save, the card collapses to "`WEATHER_API_KEY` saved" with a check. The value is never shown again.
- A native notification when the app isn't focused on that conversation, as for approvals. It has no inline field; clicking it opens the card.
- A pending request in another conversation shows a dot on that conversation in the sidebar.
- The Secrets dialog is unchanged and lists the new entry with "used by connector weather".

### 5.3 TUI

- An inline prompt in the transcript, like the approval prompt: `🔑 WEATHER_API_KEY  ········  Enter save · Tab next · Esc skip`. It uses the masked input from `SecretsPanel`, shows the `sk-ant-…a1B2` style mask after paste, and stays out of input history.
- With bracketed paste on, a pasted newline doesn't submit early.
- Attached to the daemon, `RemoteBackend` answers through §4.3. Embedded, the TUI implements `SecretPrompter` directly.
- `/secrets` is unchanged.

`Backend` additions: `secret_requests()` and `answer_secret_request(id, values, skip)`.

### 5.4 Telegram

Only the pointer message from §4.1. When the request resolves, the bot edits that message to "✓ `WEATHER_API_KEY` saved" or "skipped".

## 6. Security

- **The model can't read values.** Values aren't in the tool result, events, the journal or logs, and `redact` covers the new secret from the moment it's saved.
- **Asking doesn't grant access.** A secret is read only by what is already allowed to read it: a connector that declares it (and running a draft that declares secrets still needs approval, `ConnectorTest::policy`), an MCP server's `env_secrets`, or a provider. The shell sandbox still gets no secrets. A request for a name that nothing declares just stores a value nothing can use.
- **No overwriting by surprise.** An existing value is only offered for replacement with `replace: true`, and the user has to confirm it in the prompt (§5.1).
- **No extra names.** The answer endpoint writes only names in the request (§4.2).
- **Prompt-injection pressure**: a page the agent fetched could make it request `OPENAI_API_KEY` "to continue". The "what will read it" line and the agent-labelled reason are the defence: the user sees that nothing uses it, or that a draft connector would send it to an unfamiliar host.
- **Rate limit**: at most 3 open requests per run. Once a name has been skipped, asking for it again in the same run returns `skipped` without a prompt, so the agent can't nag.

## 7. Phases

| Phase | Scope | Done when |
|---|---|---|
| **S0 engine** | `secret_request` tool, `SecretPrompter` and the router, timeout and `--yes` behaviour, answer endpoint and checks, events, prompt and error wording, `used_by` for drafts | Tests: a mock-provider run calls `secret_request`, an API client answers, the next `connector_test` succeeds, and the value appears nowhere in the journal, events or logs (grep the test's `TERN_HOME`). Extra-name and Chat-scope answers are rejected. Skip, timeout and replay behave as in §3 |
| **S1 TUI** | inline prompt, remote parity | `App` state tests for save, skip, multi-name Tab, and paste with a newline |
| **S2 desktop** | conversation card, notification, sidebar dot | widget tests for the card's states (not set, env wins, replace, Chat-scope read-only) |
| **S3 Telegram** | pointer message and its edit on resolve | gateway test with the fake Bot API |

S0 and S1 are small and could land alongside MCP M0/M1, which touch the same secrets code.

## 8. Decisions needed

1. **Timeout**: is 15 minutes right for an unattended prompt, or should a request wait as long as the run is alive?
2. **Scope for answering**: is Owner right, or should a Chat device be able to fill in a name nothing else uses yet?

## 9. Later

- **Keys pasted into chat**: before sending a user message, notice text that looks like a key, and offer to save it as a secret and replace it in the message with `[secret WEATHER_API_KEY]`.
- **Connector activation**: when `connector_activate` is approved for a connector with missing secrets, show the secret fields in the same approval card.
