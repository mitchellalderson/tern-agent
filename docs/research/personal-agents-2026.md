# Personal agents, November 2026: what the category ships

Status: research notes, 2026-11-03. This is the evidence base for [../personal-agent-tooling-spec.md](../personal-agent-tooling-spec.md).
Most of it comes from secondary press and project READMEs. **[unverified]** marks claims from a single weak source, or
where sources disagree. Footprint numbers are the projects' own claims.

## 1. The hosted assistants

| Product | Launched | How you reach it | Where it runs | Proactive | Connectors | Safety UX | Price |
|---|---|---|---|---|---|---|---|
| **Claude Duet** (Anthropic) | research preview, 2026-10-21 | iOS, Android, claude.ai (no desktop app) | cloud VM + browser | morning brief, follow-ups on quiet email threads, checks accounts every few hours | Claude connectors, skills and memory; cloud browser as fallback | asks before send/pay/sign/delete; **finishes the rest of the job and brings the approval back later**; an out-of-model action checker; passwords go into a separate form; trading and transfers are blocked even with approval | included in Pro and Max limits |
| **Grok Bot** (xAI) | beta, 2026-08-11 | macOS, Windows, iPhone companion | one user-scoped cloud VM shared by all your named Bots | **Routines**: cron (≥5 min) or events (Slack, GitHub, Linear, Sentry, PagerDuty, email to the Bot's address, webhook with a bearer key) | Google, Microsoft 365, Salesforce, remote MCP | none notable documented | $200/mo, or bundled |
| **Meta Muse** | 2026-09-08, US | app, web, WhatsApp | per-user cloud VM with a browser you can watch | background work; an **initiative dial**; it decides on its own whether a result is worth a notification | opt-in per app; API credentials or browser fallback | **Sentinel**: a separate isolated agent enforces read vs write permissions; approval cards; activity log | free, $20, $100 |
| **ChatGPT** | Pulse 2025-09, shut down 2026-06; Scheduled tasks 2026-06-18 | app, push | cloud; Atlas browser | scheduled and event-style tasks ("when a package arrives") | Apps + custom remote MCP | Atlas "watch mode", user can take over, no saved passwords | per plan, active tasks capped **[unverified caps]** |
| **Google CC / Gemini Agent** | CC 2025-12 (Labs); Mariner shut down 2026-05-04 | **email**: "Your Day Ahead", and you reply to act | cloud | daily brief; Goal Scheduled Actions (beta) **[weakly sourced]** | Gmail, Calendar, Drive | confirms purchases and posts | AI Ultra |
| **Poke** | 2025; acquired by Cognition 2026-07 **[secondary]** | iMessage, SMS, RCS, Telegram, WhatsApp; no app | cloud | inbox- and calendar-driven | Gmail, Outlook, GitHub, Notion, Linear, custom MCP; shareable "Recipes" | Maximum Privacy mode | $0, $19, $199 |
| **Lindy** | assistant pivot 2026-02 | iMessage/SMS | cloud | morning brief, meeting prep, watchers | no-code triggers (legacy) | | $30–200 **[conflicting]** |
| **Manus** (Meta) | | web; Browser Operator extension uses your own logged-in browser | cloud VM | Scheduled Tasks 2.0; email-in tasks | Workspace, Shopify, MCP | | credits |

Lesson from OpenAI: Pulse, a bespoke proactive feed, was replaced by plain scheduled tasks. **A briefing is a scheduled
prompt plus connectors.** The product value is in delivery, triggers and judgment about when to interrupt, not in a
special feed.

Claude Duet's **channels**, as far as can be verified (the documentation is thin):
- There is one persistent **main thread**. The agent posts there only when work finishes or needs a decision.
- You can start separate **topic channels** (for example "finances" or "hiring"). They are Slack-like.
- All channels **share one memory**: they organize conversations, they are not private compartments. One reviewer
  says Duet "splits recurring topics into their own channels", which suggests it creates or routes channels itself
  **[unverified]**.
- Next to the channels sit a **Tasks** tab (what it's working on, what needs you; only the user marks a task
  complete) and a **Make** tab (editable outputs linked to their sources).

Not found: who creates a channel, whether a channel can have its own instructions or connectors, how background work
picks a channel, and per-channel notification settings.

Name clash: **Claude Code Channels** is unrelated. There a channel is an MCP server that pushes events (Telegram,
Discord) into a running Claude Code session. That is closer to tern's gateway than to Duet's topic channels.

Reception:
- Duet: "first personal agent I'd call a pro tool" (Every). Lenny's Newsletter found it polished but too slow and not
  proactive enough, and the reviewer forgot to use it.
- Muse: dominated by trust problems. Amazon blocked it over credential handling, shared chats leaked, and humans were
  found completing some calls.
- Grok Bot: no independent reviews found.

## 2. The open-source "claw" family

| Project | Lang | Footprint (claimed) | Extending it | Sandbox | Proactive | Notes |
|---|---|---|---|---|---|---|
| **OpenClaw** (ex Clawdbot/Moltbot) | TS / Node ≥24 | heavy | SKILL.md + plugin SDK via the **ClawHub** registry; `openclaw mcp serve` | host exec unless configured | heartbeat → automation jobs with `activeHours`, `NO_REPLY` sentinel, immediate wake | 20+ channels; companion "nodes" (voice, camera, Canvas); browser over CDP, real Chrome via DevTools MCP, or remote CDP |
| **ZeroClaw** | Rust | 8.8 MB binary, <5 MB RAM | crates, MCP | Landlock / bubblewrap / Seatbelt / Docker; autonomy levels; **cryptographic tool receipts** | **SOP engine**: cron, webhook, MQTT, GPIO triggers with approval gates and resumable runs | 30+ channels, provider fallback chains, GPIO |
| **IronClaw** (NEAR) | Rust | | the agent **builds WASM tools**, hot-loaded | WASM capabilities; host/path allowlists; **secrets injected at the host boundary, never visible to tools**; leak scanning; prompt-injection policy | routines (cron, events, webhooks), heartbeat, self-repair | channels are WASM components too |
| **NanoClaw** | TS | | "don't add features, add skills": code copied into your fork | per-group containers; credential gateway | **script gates skip the LLM when there's no work** | auditability praised |
| **PicoClaw** | Go | 10–20 MB RAM | SKILL.md from ClawHub; MCP | | cron | rule-based routing of simple turns to cheap models |
| **Hermes Agent** (Nous) | Python | | **writes and refines its own skills after complex tasks**; agentskills.io format | approvals, pairing, containers | natural-language cron to any channel | capped MEMORY.md/USER.md force consolidation; FTS5 over past sessions; `hermes import-agent` |
| **nanobot**, **NullClaw** (Zig, 678 KB), **MicroClaw** (Rust), **MimiClaw** (ESP32) | | | | | | proof that "small" is a selling point |

OpenClaw security, a warning to plan around:
- **ClawHavoc**, Jan–Feb 2026: 341 of the first 2,857 ClawHub skills were malicious, and later counts exceed 1,100.
- **CVE-2026-25253**: one-click RCE through WebSocket hijacking that worked even on a localhost-bound gateway.
- 80k–135k gateways exposed on Shodan.

The top complaint is **cost**: anecdotes of $50–$500/day, caused by resending full history, premium-model defaults
and agent loops.

## 3. Category matrix

**Table stakes in late 2026:**
- scheduled tasks with push or message delivery;
- a daily brief;
- Google/Microsoft connectors, with MCP for custom ones;
- memory the user can see and edit;
- approval before send/pay/post/delete;
- background jobs that notify when done;
- reach by phone, messaging-native included.

**Differentiators:**
- event triggers and watchers;
- notification judgment and an initiative dial;
- goals monitored over time;
- an activity and permission ledger;
- reply-to-act;
- **deferred approvals**;
- read/write permission split enforced outside the model;
- agent-authored tools and skills;
- topic channels with a Tasks view;
- browser and computer use.

**What tern has that none of the hosted ones do:**
- runs locally;
- one static binary of 14 MB, with an idle daemon at about 16 MB RSS;
- durable journaled runs that resume after a crash;
- connectors that are short sandboxed scripts the agent writes and you approve;
- self-hostable, with a native desktop app.

## 4. Multiplayer (several people, one agent)

Prompted by a creator video ("7 Grok Bot agents I use every day", https://www.youtube.com/watch?v=QBmgF1kJSK4; the
transcript wasn't retrievable). According to the user's summary, she wants several people in one chat with the agent.

- **Grok Bot shared Bots** (Cursor Team/Enterprise only):
  - Roles are owner, Manager and Collaborator. Sharing doesn't copy the Bot: everyone uses the same one.
  - Each person's 1:1 chat stays private, even from the owner.
  - In group chats and Slack the Bot acts for whoever mentioned it. Their usage is billed, their connected accounts
    are used (after asking), and only they can approve.
  - Team memory is shared, but what it learns about you stays private.
  - Each user gets a Firecracker microVM. Group and Slack chats use a separate shared computer.
  - Individual (non-team) users have no multiplayer.
- **ChatGPT group chats**: up to 20 people. Personal memory and custom instructions are off in groups; the group gets
  its own instructions. Agent mode, voice and Deep Research aren't available there.
- **Claude Duet**: single-user only (Lenny's Newsletter).
- **Claude Tag, Slackbot, Dust**: multiplayer by living in Slack channels. Superconductor's framing: shared sessions
  and shared learnings, but "joining a coworker's chat shouldn't give you their email".
- A 2026 arXiv scoping review (2609.14062) names cross-user contamination as the main risk of shared-state
  assistants.

Sources: https://docs.x.ai/grok-bot/shared-bots · https://docs.x.ai/grok-bot/teams-and-enterprises ·
https://help.openai.com/en/articles/12703475-group-chats · https://openai.com/index/group-chats-in-chatgpt/ ·
https://www.salesforceben.com/multiplayer-agents-the-next-wave-of-collaborative-agentic-ai/ ·
https://www.superconductor.com/blog/multiplayer-ai · https://arxiv.org/pdf/2609.14062

## Sources

- Claude Duet: https://support.claude.com/en/articles/17631495-get-started-with-claude-duet ·
  https://every.to/vibe-check/vibe-check-duet-anthropic-s-always-on-agent ·
  https://www.lennysnewsletter.com/p/claude-duet-is-beautiful-i-still · https://www.datacamp.com/blog/claude-duet ·
  https://9to5mac.com/2026/10/21/anthropic-takes-on-chatgpt-dots-and-meta-muse-with-claude-duet/
- Grok Bot: https://docs.x.ai/grok-bot/skills-routines-and-automations · https://x.ai/changelog/bot ·
  https://9to5mac.com/2026/10/09/grok-bot-just-got-its-own-email-address-heres-how-to-claim-yours/
- Meta Muse: https://en.wikipedia.org/wiki/Muse_(AI_agent) ·
  https://techcrunch.com/2026/09/08/meta-debuts-its-muse-ai-agent-will-consumers-trust-it/ ·
  https://www.testingcatalog.com/meta-introduces-muse-as-a-proactive-personal-agent.md
- OpenAI: https://gigazine.net/gsc_news/en/20260619-chatgpt-scheduled-tasks/ ·
  https://help.openai.com/en/articles/12628199-chatgpt-atlas-agent-mode
- Google: https://blog.google/innovation-and-ai/models-and-research/google-labs/cc-ai-agent/ ·
  https://9to5google.com/2026/01/28/chrome-gemini-auto-browse/
- Poke: https://rywalker.com/research/poke · Lindy: https://docs.lindy.ai/features/imessage-sms ·
  Manus: https://manus.im/blog/product
- OpenClaw: https://github.com/openclaw/openclaw · https://docs.openclaw.ai/concepts/memory ·
  https://docs.openclaw.ai/gateway/heartbeat · https://clawdocs.org/security/known-vulnerabilities
- ZeroClaw: https://github.com/zeroclaw-labs/zeroclaw · IronClaw: https://github.com/nearai/ironclaw ·
  NanoClaw: https://github.com/nanocoai/nanoclaw · PicoClaw: https://github.com/sipeed/picoclaw ·
  Hermes: https://github.com/NousResearch/hermes-agent · variants: https://clawdocs.org/guides/lightweight-variants
