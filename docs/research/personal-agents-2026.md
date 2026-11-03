# Personal agents, November 2026: what the category ships

Status: research notes, 2026-11-03, revised the same day with OpenAI Dots, plan availability and the small-business
shelf. This is the evidence base for [../personal-agent-tooling-spec.md](../personal-agent-tooling-spec.md),
[../positioning-talking-track.md](../positioning-talking-track.md) and [../launch-plan.md](../launch-plan.md).
Most of it comes from secondary press and project READMEs. **[unverified]** marks claims from a single weak source, or
where sources disagree. Footprint numbers are the projects' own claims. §6 lists what the launch plan assumes and how
well each assumption is backed.

## 1. The hosted assistants

| Product | Launched | How you reach it | Where it runs | Proactive | Connectors | Safety UX | Price |
|---|---|---|---|---|---|---|---|
| **Claude Duet** (Anthropic) | research preview, 2026-10-21 | iOS, Android, claude.ai (no desktop app) | cloud VM + browser (you can't watch or take over the browser) | morning brief, follow-ups on quiet email threads, checks accounts on a schedule ("every few hours" appears only in secondary sources **[unverified]**) | Claude connectors, skills and memory (6,000+ marketplace connectors claimed); Gmail, Calendar, Drive, Slack (limited), QuickBooks, HubSpot, Docusign, Calendly cited; **Microsoft 365 not supported**; cloud browser as fallback | asks before send/pay/sign/delete; **finishes the rest of the job and brings the approval back later**; an out-of-model action checker; passwords go into a separate form; trading and transfers are blocked even with approval; Anthropic's help article warns it can misread a thread or misjudge urgency | included in Pro and Max limits; background checks count against them; Opus 5.5 only, not switchable |
| **OpenAI Dots** | 2026-09-29 (DevDay), rolling out | ChatGPT and Codex; Slack and Teams; SMS "coming soon" | one cloud computer and browser per dot | runs toward user-defined goals in the background; background research is **read-only** against connected apps | 4,000+ connected apps claimed; specialist dots get their own identity, credentials and tools | not documented beyond read-only background work; governance via **Microsoft Agent 365** (planned) | Pro and Business Premium in eligible markets (EEA, Switzerland, UK excluded at launch); Enterprise/Edu/Healthcare beta **[sources disagree]**; chatting with a dot doesn't count toward limits, tasks it starts in Codex or ChatGPT Work do |
| **Grok Bot** (xAI) | beta, 2026-08-11 | macOS, Windows, iPhone companion | one user-scoped cloud VM shared by all your named Bots (shared browser logins and files) | **Routines**: cron (≥5 min) or events (Slack, GitHub, Linear, Sentry, PagerDuty, email to the Bot's address, webhook with a bearer key) | Google, Microsoft 365, Salesforce, remote MCP | none notable documented | no standalone rate card; via SuperGrok ($30) and SuperGrok Plus ($100) or Cursor plans ($120/seat Teams Premium) with a weekly allowance, metered after; earlier reports said $200/mo **[conflicting]**; routines draw from the same allowance and there is no per-bot spend cap |
| **Meta Muse** | 2026-09-08, US; **small-business expansion 2026-09-29** | app, web, WhatsApp | per-user cloud VM with a browser you can watch | background work; an **initiative dial**; it decides on its own whether a result is worth a notification | opt-in per app; API credentials or browser fallback; SMB: QuickBooks, Shopify, Slack, Facebook/Instagram business accounts, Meta ad accounts | **Sentinel**: a separate isolated agent enforces read vs write permissions; approval cards; activity log; credentials vaulted outside the model | free, $20, $100 (press-reported); no SMB pricing found |
| **ChatGPT** (pre-Dots) | Pulse 2025-09, shut down 2026-06; Scheduled tasks 2026-06-18 | app, push | cloud; Atlas browser | scheduled and event-style tasks ("when a package arrives") | Apps + custom remote MCP | Atlas "watch mode", user can take over, no saved passwords | per plan, active tasks capped **[unverified caps]** |
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
  proactive enough, and the reviewer forgot to use it. Business-focused reviews note the absence of an admin console,
  seat management and retention terms, and that its memory is one person's, not a company record.
- Dots: launch coverage only. TechCrunch's framing: much of this was already possible through Codex and similar tools;
  dots bundle it with a persona.
- Muse: dominated by trust problems. Amazon blocked it over credential handling, shared chats leaked, humans were
  found completing some calls, and the consumer Muse gave a Marketplace buyer a seller's home address (Guardian, Meta
  investigating). On the SMB expansion, an Omdia analyst asked why a business would pick Muse over Microsoft or
  Google, and a Gartner analyst called agentic AI "ultimately a governance challenge".
- Grok Bot: no independent reviews found.

### 1.1 Who can actually buy each one

This is the premise of the business positioning, so it gets its own table. "Business" here means a company account,
not a person expensing a Pro plan.

| Product | Individual | Business or team | Governance published | Notes |
|---|---|---|---|---|
| Claude Duet | US Pro/Max, waitlist | **no** (Team and Enterprise "later", no date) | no retention, training or admin terms | a company can only use it through an owner's personal plan |
| OpenAI Dots | Pro | Business Premium; Enterprise/Edu/Healthcare beta **[sources disagree]** | via Microsoft Agent 365 (planned) | EEA, Switzerland and UK excluded at launch |
| Meta Muse | free / $20 / $100 | SMB tier since 2026-09-29, pricing not found | Sentinel, activity log; no admin terms found | aimed at Meta-platform retailers |
| Grok Bot | SuperGrok tiers | Cursor Teams/Enterprise (shared Bots, §4) | roles; no spend cap | priced per seat for engineers |
| Poke, Lindy | yes | Lindy has team plans | | messaging-first, no-code |
| OpenClaw family | self-host | self-host | none | your problem, and your Shodan entry |

Read across: in November 2026 there is no personal agent a 10-person firm can buy as a firm, with published data
terms, that isn't tied to one model vendor. That is the gap the positioning doc targets. The caveat for the launch
plan: Dots on Business Premium and Muse's SMB tier are both younger than two months and will close part of it.

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

Adoption, GitHub on 2026-11-03: OpenClaw **394k stars / 83k forks**, Hermes Agent **260k / 57k**, ZeroClaw **33k / 5k**.
Those are the largest numbers in the category by far, and they say two things for the launch plan: the self-hosted
segment is real and enormous, and it is crowded. A Rust single-binary agent is not novel on its own (ZeroClaw,
MicroClaw); the journal, the self-written sandboxed connectors and the local-or-hosted engine are what have to carry
the post.

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
- one static binary under 20 MB (13.9 MB at M6, 18 MB now with the remote core, sub-agents and provider setup), with
  an idle daemon at 12–16 MB RSS;
- durable journaled runs that resume after a crash without repeating side effects;
- connectors that are short sandboxed scripts the agent writes and you approve;
- any model vendor (Anthropic, or any OpenAI-compatible endpoint, including local servers);
- self-hostable, with a native desktop app, and the same engine offered hosted with export-and-leave (roadmap D5).

**What tern lacks that the hosted ones have** (so the launch post can say so first):
- a browser or computer use;
- a connector catalog (nothing pre-built beyond MCP; the pitch is that it isn't needed);
- a mobile app and push (D4–D5);
- channels, a Tasks view, deferred approvals, event watchers (Stages 2–3);
- a bundled model, so the user needs an API key on day one.

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

## 5. The small-business shelf

What an owner-operator is shown when they search for "AI agent for small business" in late 2026. Mostly vendor
roundups, so weigh accordingly. Relevant because the positioning doc puts tern on this shelf, not the personal-agent
one.

| Product | Shape | Price | Why it matters to us |
|---|---|---|---|
| **Meta Muse (SMB)** | hosted agent, Meta-platform first | consumer tiers; SMB not found | the only hosted personal agent with an explicit SMB tier; trust-damaged |
| **Lindy** | no-code workflow canvas with an assistant front | $30–200 **[conflicting]**, 7-day trial, no free tier | "draw a flow" is the shape operators are being sold; tern's "describe an outcome" is the contrast |
| **Zapier agents**, **HubSpot Agent Hub** | agents bolted onto an automation or CRM product | per existing plan | strong where the business already lives in that product; weak outside it |
| **Vellum** (assistant) | personal assistant "on your device or in the cloud", remembers how the business works | from $50/mo | the closest pitch to ours, at 2–3× our sketch price; evidence that local-or-cloud resonates |
| **Sai (Simular)** | agent that works web portals and spreadsheets | | vendor-ranked first in its own roundup; the "no API, use the browser" approach we don't take |
| **Poke** | messaging-only assistant, Recipes | $0 / $19 / $199 | shows $19 is an accepted price for a messaging-first agent |

Read across: the shelf is canvases, CRM add-ons and hosted VMs. Nothing on it is an engine the business owns, and
nothing on it runs on any model vendor. The price band buyers are anchored to is $20–50 a month.

## 6. What the launch plan assumes, and how well it is backed

| Assumption (launch plan §) | Evidence here | Strength | Gap |
|---|---|---|---|
| Self-hosters want a personal agent (§1, §4) | 394k / 260k / 33k stars on the three main claws; 80k–135k exposed gateways is also an installed-base proxy | **strong** | none; but the segment is crowded, so targets in launch plan §5 are modest for the category |
| "The safe small one" is a hook (§4.2) | ClawHavoc, CVE-2026-25253, Shodan counts; cost anecdotes of $50–500/day | **strong** | tern's own threat model isn't published yet (SECURITY.md is a Phase 0 item) |
| Durability is a differentiator (§4.1) | none of the hosted products document crash behaviour; claws are "mostly no", ZeroClaw has resumable runs | **medium** | no head-to-head; the demo's kill-and-resume is the evidence |
| Agent-written connectors are rare (§4.1) | IronClaw builds WASM tools, Hermes writes skills; nobody else combines "agent writes it, host sandboxes it, user approves capabilities" | **medium** | IronClaw is the closest; expect the comparison |
| Businesses can't buy the incumbents as businesses (positioning §2) | §1.1: Duet individual-only, no terms; Dots Business Premium but Microsoft-governed; Muse SMB young and trust-damaged | **medium, eroding** | Dots and Muse SMB are under two months old; re-check monthly |
| $5 relay / $15–20 hosted is a price people pay (§6) | anchors only: $20 Pro plans, Poke $19, Vellum $50, OpenClaw cost complaints | **weak** | this is what the priced waitlist tests; no direct evidence |
| Operators prefer "describe an outcome" to a canvas (positioning §4.6) | Lindy and Zapier sell canvases; OpenAI replaced Pulse with plain scheduled tasks | **weak** | opinion plus one data point; validate in founding-member calls |
| HN and r/rust will carry a Rust agent post (§4.2) | ZeroClaw and MicroClaw exist, so the audience has seen one | **weak** | no record of how those launches did; look up their Show HN threads before posting |

## Sources

- OpenAI Dots: https://techcrunch.com/2026/09/29/openai-launches-dots-its-bubbly-agentic-avatar/ ·
  https://www.engadget.com/2272230/dots-are-openais-new-personal-agents-and-soon-youll-be-able-to-control-several-of-them/ ·
  https://betanews.com/article/openai-dots-agents-chatgpt/
- Claude Duet (business angle): https://www.sentra.app/articles/claude-duet (competitor-written) ·
  https://www.layer3labs.io/guides/claude-duet-for-business ·
  https://support.claude.com/en/articles/17631495-getting-started-with-claude-duet
- Meta Muse SMB: https://www.techtarget.com/ai/news/366651445/Meta-expands-Muse-to-small-businesses ·
  https://aibusiness.com/agentic-ai/meta-expands-muse-to-small-businesses ·
  https://www.eesel.ai/blog/meta-muse-agent-review
- Grok Bot pricing: https://www.aipricing.guru/news/xai-grok-bot-launch-pricing-impact-august-2026/ ·
  https://openclawdatabase.com/grok-bot/pricing/index.md
- Small-business shelf: https://www.simular.ai/alternatives/best-personal-agents ·
  https://www.vellum.ai/blog/best-ai-assistant-for-small-business ·
  https://surething.io/blog/best-ai-agents-for-small-business-2026
- Star counts: the GitHub pages linked below, read 2026-11-03.
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
