# Learnings from innbot (tern on Inngest), applied to tern

innbot rebuilt tern on Inngest from `docs/inngest-rebuild-feature-spec.md`. This page maps what it learned
back to tern: what tern already has, what each workstream adds, and what doesn't apply.
Audit date: 2026-11-18, on `main` at `55193f3`.

**Status:** workstreams 1–9 shipped in v0.1.0-alpha.8 (see `docs/releases/v0.1.0-alpha.8.md`). Still open: P10
(stale-doc check), which waits for living documents.

**Rule behind all of it:** the host decides, not the model. Prompts steer; code enforces.

## Already in tern (no work)

- Effects: reads never ask, writes ask, destructive always asks, money refused (`tern-core/src/approval.rs`).
- Outbox after 120 s, runs once when approved later.
- Taint on posts, readers of tainted posts get tainted, tainted writes always ask.
- Webhook routines: HMAC or bearer, rate limits, replays dropped.
- `request_secret` with `replace`.
- Send-while-working reaches the next model call; late messages start a follow-up run.
- Parallel sub-agent mail is addressed by run id, so the innbot sibling-inbox bug can't happen.
- `agent_ask_parent`, event watches with a baseline first poll, MCP effects from hints, skills and save-as-skill offers.
- Streaming, "Thought for Ns", one activity line, context meter, New topic with carry-over, idle/day cuts.
- Silent memory flush before compaction.

## Workstreams

Each is its own session, worktree, branch and PR. Letters match the audit below.

| # | Workstream | Items | Depends on |
|---|---|---|---|
| 1 | Answer and run reliability | X1, R1, R2, R4, R5, R6, X6 | none |
| 2 | Secrets and connector errors | S4, S6, R7/K11 | none |
| 3 | Decision model and channel routing | C1–C3, B1–B6, X3 | none |
| 4 | Agents working together | A1–A6, A8–A10 | 3 for lane descriptions (A6) |
| 5 | Proactive, host-only | P1, P2, P5, P6, P7 | none |
| 6 | Proactive, model-gated | P3, P4, P9, P11 | 3 (`decide()`) |
| 7 | Connector hardening | K1, K4, K5, K7 | none |
| 8 | Composio and connect offers | K8, P8, K6 | 7 for sign-in cards (soft) |
| 9 | Catch-up UX | U3 | none |

### Not applying

**2026-11-18: tern is strictly local and single-player.** The daemon and the desktop app run on this computer only.
Remote cores, device pairing, accounts, hosted features, messaging gateways and webhook routines are being removed
(session `local-only`). Anything below about people, hosting or inbound access is out of scope for good, and the B1
"2+ people" rule is dropped.

- Inngest Agents, Connect, Realtime, Sandboxes, one-origin server, Clerk organisations, per-person tokens and
  shared hosted Composio project: platform or multi-tenant hosting, not tern's local single-owner model.
- Mobile web layout: tern ships a desktop app.
- P10 stale-doc check: tern has no living documents yet (documents viewer #44 is unmerged). Revisit after it lands.

## Audit

Status: HAS, PARTIAL, MISSING. Pointers are from the 2026-11-18 audit.

### Agents working together (workstream 4)
- A1 MISSING: default persona is generic (`tern-core/assets/persona.md`). Make the default agent a Chief of Staff.
- A2 PARTIAL: cross-agent ask posts "Worked for X" in the delegate's DM; no "Message from X" card in the asker's chat.
- A3 PARTIAL: reports capped at 8k per child, `agent_result` fetches more; no shared 24k fan-out budget.
- A4 MISSING: `brief_team` (read at next run by default; `now` or `want_reply` wakes).
- A5 PARTIAL: several `agent_spawn` + `agent_wait` works; no `delegate_team` tool or combined card.
- A6 PARTIAL: channel prompt lists members but no lanes, no "delegate their parts, answer once with credit".
- A8 PARTIAL: stopped vs failed is shown; no "what got done / what to do next".
- A9 PARTIAL: work and usage exist in the API and CLI; no `inspect_work` tool for the agent.
- A10 PARTIAL: routines from chat yes; agents and channels only from API/UI; no narrowing rule for created agents.

### Channels and routing (workstream 3)
- B1 PARTIAL: "Let agents decide" per channel, max 3. Missing per-agent "Reply when relevant", "One agent / Every
  relevant agent" mode, and "2+ people → mention only".
- B2 MISSING: plain-English reply rule per channel.
- B3 MISSING: router sees only the current message; give it the last 3 posts.
- B4 PARTIAL: router input is the persona's first line. Add a one-line description, generated once on role save.
- B5 PARTIAL: "Sent to @x" exists without the why; no "Choosing who replies…".
- B6 / X3 PARTIAL: digest cuts every post to its first 600 chars (loses the trailing question). Newest 3 in full up to
  2,000, older at 500; leave out run byproducts.

### Decision model (workstream 3)
- C1 MISSING: typed `decide()` (choice with probabilities, yes/no 0–1, score) on a decision model, e.g. OpenRouter
  `/api/alpha/decisions`; fallback to the fast model as one structured call on no key, error or 3 s timeout; a
  refusal or missing answer is never "no". Router and notification gate move onto it.
- C2 PARTIAL: `gate_model` exists. Make three named tiers: agent model, fast model (background writing), decision model.
- C3 MISSING: reasoning off for quick calls; cheap yes/no gates in front of extraction.

### Proactive (workstreams 5 and 6)
Every feature: a dismissible card, a switch that defaults on, a hard cap.
- P1 MISSING: suggest a routine after 3+ similar asks in 14 days.
- P2 MISSING: "While you were away" brief on first open of the day after 8 h+ away.
- P3 PARTIAL: manual New topic only; add a "New topic?" hint, never switch on its own.
- P4 PARTIAL: `wait_until` exists (max 24 h). Extract open loops ("I'll check back Friday") into waits; longer horizons.
- P5 MISSING: daily per-agent heartbeat, host checks only, no model call when nothing needs attention.
- P6 PARTIAL: caps pause routines. Add: 2 failures or 3 empty reports in a row → owner proposes a fix card, max 1/week.
- P7 PARTIAL: "Always allow" is on every card. Learn instead: offer it after 3 "Allow once" (same agent + tool +
  target, 14 days); never for destructive, money, tainted or routine runs; grants in desktop Settings.
- P9 MISSING: weekly memory tidy-up, reversible, logged, undo; unverified never overwrites verified.
- P11 PARTIAL: flush exists. Add a yes/no gate after each user message, then save up to 3 missed facts, deduplicated.

### Connectors (workstreams 7 and 8)
- K1 PARTIAL: manifest allows `*.example.com` wildcards and a legacy `secrets` list the script can read.
- K2 HAS (bridge attaches credentials, checks redirects, rate-limits).
- K3 PARTIAL: two built-ins (ntfy, slack).
- K4 PARTIAL: approval bound to a hash of source + capabilities; no evidence card (tests, each HTTP call, Refused
  hosts, output, untested tools).
- K5 PARTIAL: PKCE + device flow; add single-use 15-minute state and client-credentials OAuth.
- K6 PARTIAL: add the preference order: Composio, official MCP with OAuth, connector with OAuth, app-only OAuth, API
  key. Never ask for a pasted short-lived token.
- K7 PARTIAL: host-built sign-in card exists; add connector-returned `sign_in` with https, no embedded credentials,
  declared-domain checks and a live expiry.
- K8 MISSING: Composio connector (search_tools, list_connections, connect_app, run_read_tool, run_tool asks,
  run_destructive_tool always asks; payment toolkits read-only; HTML → text; strings capped at 4,000).
- P8 MISSING: `offer_connection` card instead of "I can't", once per app per conversation.

### Safety and reliability (workstreams 1 and 2)
- X1 BUG: `agent.rs` keeps only the last turn's text; an answer written next to a tool call is lost behind an
  afterword like "I saved the details". Post the substantive answer, afterword after it.
- R1 PARTIAL: nothing stops a stopped run flipping back to running; cancelled steps are `failed`, not `interrupted`.
- R2 PARTIAL: persisted run errors are raw; `friendly_call_error` is TUI-only.
- R4 MISSING: taint and ceiling don't carry across runs in a conversation.
- R5 PARTIAL: roster order depends on task words and edit time; make it stable for prompt caching.
- R6 PARTIAL: smoke set exists; aim for one ~40 s end-to-end check.
- X6 PARTIAL: verify "Working for…" pointers update when a run is stopped while a child runs.
- S4 MISSING: secrets are plaintext `secrets.toml` (0600). Encrypt at rest, AAD bound to the secret's name.
- S6 MISSING: redaction is exact-value only. Also catch masked forms (`ck_**3pNe`) and first 8 / last 4 on their own,
  in core and the Lua bridge.
- R7 / K11 PARTIAL: map connector errors to actionable hints (missing key → request it; rejected key → replace it).

### UX (workstream 9)
- U3 PARTIAL: unread dot only. Add "New since you left" and open there when there's a lot to catch up on.
