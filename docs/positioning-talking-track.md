# tern positioning: talking track

Status: draft, 2026-11-03. Internal. This is what we say about tern, to whom, and why, with the evidence behind each
line. Appendix A marks every claim as **built** or **roadmap** so nobody oversells. Sources are in
[research/personal-agents-2026.md](research/personal-agents-2026.md) plus the links at the end.

Related: [personal-agent-tooling-spec.md](personal-agent-tooling-spec.md) (the features), [deployment-spec.md](deployment-spec.md)
(the hosted engine), [gui-spec.md](gui-spec.md) (the app).

## 1. The one-liner

> **tern is the always-on agent for technical founders, operations leaders and go-to-market leaders.** It connects to
> your whole stack, including your own product's API and the tools nobody built a connector for, picks up where it
> stopped after a crash or a sleep, and never acts on your behalf without a receipt.

Shorter: **"The agent for the business side of a technical company."**

Positioning statement (internal form):

> For technical founders, ops leaders and GTM leaders at devtool, infrastructure, data and AI companies, who run the
> business across a dozen API-first tools and have no engineer to spare, tern is an always-on agent that runs
> durably, anywhere (laptop, your server, or hosted), and connects to anything with an API. Unlike Claude Duet, OpenAI
> Dots, Meta Muse and Grok Bot, it is not tied to one model vendor, one app catalog or one cloud VM, and it treats
> every outbound action as something you can audit, defer or deny.

## 2. Why technical business-side leaders

Status: ICP revised 2026-11-05. Earlier drafts led with non-technical owner-operators (accountants, trades, property
managers). That segment is real but needs the desktop app, OAuth sign-in and the hosted tier to be easy. We now lead
with the people who can use tern **today** and who are the best fit for what it is.

**The ICP, in three roles, one company type.** The company is technical: a devtool, dev-infrastructure, data, security
or AI product, usually 2 to 100 people. The buyers are:

- **Technical founders** running the business side themselves (sales, support, finance, hiring) next to the product.
- **Operations leaders**: heads of ops, chiefs of staff, RevOps, business-systems and finance-ops people who own the
  glue between a dozen SaaS tools and have more requests than hours.
- **Go-to-market leaders in technical spaces**: heads of growth, developer relations, product marketing, sales
  engineering and founder-led-sales owners, whose job depends on joining product usage data to the CRM, the docs, the
  community and the inbox.

Why this is the right segment:

- **They can install it now.** Today's install is a terminal command and a provider key. This audience is comfortable
  with both, so we are not blocked on the desktop onboarding, OAuth sign-in cards or the hosted tier.
- **Their stack is API-first and long-tailed.** The most important tool is often their **own product's API**, which
  no catalog will ever have, plus a CRM, a billing system, a docs platform and a community tool. Auto-written,
  capability-approved connectors are the feature they care about most.
- **They are where the launch happens.** Hacker News, Rust and self-hosting communities, X, LinkedIn and technical
  newsletters reach exactly this group, and the launch plan is built on those channels.
- **They read security claims critically, so the claims have to hold.** They will check the journal, the ledger and
  the sandbox. "Reads are free, writes ask, money never moves" is a stronger line to them than to anyone else.
- **They buy and influence.** A founder decides alone; an ops or GTM leader brings a tool in by trial and expands it
  to a team. That is the path to Connect, then Cloud, then team plans.
- **The competitors point elsewhere.** Grok Bot ($30 to 200/mo, routines on GitHub, Linear, Sentry, PagerDuty) and
  OpenAI Dots (launched from Codex) aim at engineers writing and shipping code. The open-source claw family owns the
  hobbyist slot. Nobody is building the durable, vendor-neutral agent for the people who run the business side of a
  technical company. Claude Duet is individual Pro/Max only, with Team and Enterprise "later" and no published
  retention or admin terms. Meta Muse's small-business push is dominated by trust failures, and Dots' governance story
  is enterprise Agent 365.
- **The gaps don't hurt.** No bundled browser, no OS-level clicking, no 6,000-connector marketplace, no voice. These
  users want the same ten things done reliably every day, against APIs.

What changes in how we sell: lead with the connector moment on a tool they use and with durability and receipts, show
the CLI and the desktop app side by side, and let "runs where your data policy allows" speak to security-conscious
buyers instead of a regulated-profession pitch.

The earlier segments (accountants, trades, property managers, clinics) are deferred, not dropped. Revisit them after
the desktop app, OAuth sign-in and hosted tier are live; the demo flows in
[vertical-demo-flows.md](vertical-demo-flows.md) keep those scripts.

## 3. The niche, in one picture

The hosted assistants compete on **autonomy and breadth** (a cloud VM, a browser, thousands of connectors, one
vendor's best model). The claw family competes on **hackability**. Nobody competes on the combination of:

| | Duet | Dots | Muse | Grok Bot | OpenClaw family | **tern** |
|---|---|---|---|---|---|---|
| Runs on your own machine or server | no | no | no | no | yes | **yes** |
| Also offered hosted, same engine, export and leave | n/a | n/a | n/a | n/a | no | **yes** (roadmap) |
| Connects to a service with no pre-built connector | cloud browser | cloud browser | browser fallback | remote MCP | plugins / skills | **writes a sandboxed connector itself, you approve its capabilities** |
| Your choice of model vendor | no (Opus 5.5 only) | no (GPT-6 Astra) | no | no | yes | **yes** |
| Survives a crash mid-task without redoing side effects | not documented | not documented | not documented | not documented | mostly no | **yes** (journal) |
| Action waits for you while the rest of the job continues | yes | not documented | approval cards | not documented | no | **yes** (roadmap) |
| Prompt-injection defence enforced outside the model | action checker | read-only background | Sentinel | none documented | varies | **effects + taint** (roadmap) |
| Available to a business, not only an individual plan | no (Pro/Max only) | Business Premium | SMB tier | per seat | self-host | **self-host or hosted** |
| Footprint | cloud VM | cloud VM | cloud VM | cloud VM | Node ≥24, heavy | **one binary under 20 MB, idle under 30 MB RAM** |

The niche is the bottom-right of that table: **a durable, self-connecting, vendor-neutral business agent you can run
anywhere.** Say it as: *"The incumbents give you an assistant inside their cloud. tern gives you an engine you own,
with a hosted option when you'd rather not run it."*

## 4. Killer features: the talking points

Each one: what to say, why the listener cares, what backs it up. Order matters: lead with 4.1 and 4.2, which nobody
else can say.

### 4.1 It connects to anything, in minutes

**Say:** "Tell tern 'connect to our scheduling tool' and it reads the API docs, writes a small connector, tests it,
and shows you exactly what it will be allowed to touch: which hosts, which credentials. You approve, and it's live.
If the service supports sign-in, you click Sign in instead of hunting for an API key."

**Why they care:** every business has three tools the big catalogs don't cover. For the incumbents the answer is "use
the cloud browser", which is slow, brittle and invisible to you.

**Proof:** connectors are short sandboxed Luau scripts that declare their hosts and secrets; the agent drafts them into
`~/.tern/connectors/drafts/` and can only activate one with your approval of its capabilities (built, M4). OAuth-first
discovery, sign-in cards and multiple accounts per service are specced (T4). Credentials are injected at the host
boundary so a connector never sees the secret value (T4).

**Don't say:** "6,000 connectors." We have none pre-built beyond MCP; the pitch is that we don't need a catalog.

### 4.2 It finishes the job, even when things break

**Say:** "Every step tern takes is written to a journal before and after it runs. Kill it mid-task, reboot the
machine, lose the network: it picks up where it stopped and never sends the same email or creates the same invoice
twice."

**Why they care:** business tasks have side effects. An agent that retries blindly is worse than no agent. Operators
have been burned by Zapier loops and duplicate sends.

**Proof:** step journal in SQLite, `tern resume`, replayed steps marked `(replayed)`, the daemon resumes interrupted
runs on start (built, M1/M5). Non-idempotent tools are substituted rather than re-executed after a crash. The
research doc's observation: this is the property Inngest sells, delivered on a single host.

### 4.3 It's always on, and it's wherever you are

**Say:** "Routines run around the clock when tern lives on an always-on server (yours today, ours when hosting ships). On a laptop alone, a sleeping machine pauses them and they catch up on wake without repeating work. Approvals reach your phone. The same agent answers in the desktop
app and in Telegram, and it's one conversation, one memory. Run it on your own machine, on a server you control, or
let us host it. Moving between the three is a file copy."

**Why they care:** the morning brief, the invoice chase and the order watch are only useful if they happen without
you. Phone reach is table stakes; *the same engine local or hosted* is not something anyone else offers.

**Proof:** daemon with scheduler and Telegram gateway with approval buttons (built, M5). Remote core over TCP with TLS,
device pairing by QR code and scoped tokens (built, D1). Desktop app attaches to the daemon; closing it loses nothing
(built, G0–G2). Hosted tier, push relay, mobile app: roadmap (D4–D5). See §6.

### 4.4 Nothing goes out without a receipt

**Say:** "Reading is free; writing asks. If tern wants to send, post, pay or delete and you're not around, the action
waits in an outbox while the rest of the job continues, and you approve it from your phone in the morning. If a task
was started by an email or a web page, tern knows it might be a trick and asks before any write, no matter what
you've allowed before. Everything it did is in a ledger you can read."

**Why they care:** this is the Muse story in reverse. Trust is the whole purchase decision for a business agent, and
"the model decides whether it's safe" is not an answer an accountant accepts.

**Proof:** approvals for shell and out-of-workspace file access with once/always/deny, journaled (built). Effects
(`read`/`write`/`destructive`) enforced by the host, the outbox with deferred approvals, taint for runs started by
external content, scoped grants that never cover tainted writes, and a hash-chained activity ledger are specced (T3,
T6). Say "designed so that" until T3 ships.

### 4.5 Your model, your bill, your data

**Say:** "Use Anthropic, OpenAI-compatible endpoints, OpenRouter, or a model on your own hardware. Switch per task.
Watches that find nothing cost zero tokens. Your conversations live in a SQLite file you can copy, back up and
delete."

**Why they care:** the incumbents lock you to their model and meter you against their plan limits (Duet counts
background checks against your weekly cap; Grok Bot routines drain a weekly allowance with no per-bot cap). Businesses
with data-handling obligations need to answer "where is it stored" in one sentence.

**Proof:** provider abstraction with Anthropic and OpenAI-compatible kinds, `provider:model` switching (built). SQLite
store, secrets file at mode 0600, redacted bug reports (built). Gate scripts and watches that skip the model when
there's nothing new are specced (T1). Per-routine budgets are specced (T6).

### 4.6 Simple on purpose

**Say:** "One download. One window. Routines are sentences: 'Every weekday at 8, summarize unread invoices and flag
anything over 30 days.' No workflow canvas, no nodes, no YAML."

**Why they care:** the operator doesn't want to learn an automation product. The no-code competitors (Lindy, Zapier
agents) are canvases; the hosted assistants are chat plus a hidden VM.

**Proof:** one static binary, no Python, Node or Docker (built). Schedules from natural language, delivered to the
chat that asked (built). Desktop "New routine" sheet with presets and a live preview of the next three runs (built).
Menu-bar tray and one-click quit: roadmap (T8).

## 5. Competitive one-liners

What to say when each name comes up. Be generous about what they do well; the point is positioning, not disparagement.

- **Claude Duet.** "Duet is the best-designed of the hosted agents and we borrowed its channels and deferred-approval
  ideas. But it's an individual Pro/Max feature, US-only, no Team or Enterprise plan, no published retention or admin
  terms, and only Anthropic's model. tern is the same working style for a business, on any model, where you choose to
  run it." Duet's own help article warns it may misread a thread or misjudge urgency; our answer is the ledger and the
  outbox, not a better model.
- **OpenAI Dots.** "Dots are always-on agents with their own cloud computer, launched from Codex and ChatGPT for Pro
  and Business Premium. Background work is read-only against connected apps, and governance goes through Microsoft
  Agent 365. It's a strong product for a company already on ChatGPT Business and Microsoft. tern is for the business
  that isn't, or that wants the engine on its own terms." Dots cannot have their own email address or make calls at
  launch; neither can tern, so don't lead there.
- **Meta Muse.** "Muse is the most consumer-facing and the most trust-challenged. Its Sentinel idea, a separate
  enforcer for read versus write, is right, and tern does the same thing in the host rather than a second model. The
  difference is that your data isn't on Meta's VM." Reference the SMB expansion (QuickBooks, Shopify, Slack) to show we
  know the target overlaps.
- **Grok Bot.** "Grok Bot is routines plus a shared cloud computer, priced for engineers and wired to engineering
  tools. All your bots share one VM and one browser session. tern's routines are the same idea for business triggers,
  with a per-routine budget and a zero-token idle." No independent business reviews exist; don't speculate.
- **OpenClaw and the claws.** "If you want to hack on an agent, OpenClaw is the community. tern is what you run when
  you want it to just work and not get you on Shodan." ClawHavoc (hundreds of malicious skills), CVE-2026-25253 and
  80k+ exposed gateways are public; we have no open registry by design, and bundles are signed and hash-pinned
  (roadmap).
- **Lindy, Zapier agents, HubSpot Agent Hub.** "Workflow builders with an LLM step. tern is an agent with a journal.
  Different shape: you describe outcomes, you don't draw flows."

## 6. The hosted engine: how to talk about it

The pitch to the operator:

> "tern runs on your laptop for free. When you want it on while the laptop is closed, we host the engine for you:
> same binary, your own private instance, your data on its own volume, backups included. Your phone, your desktop app
> and Telegram all connect to it. Export everything with one click and run it at home again whenever you like."

What the hosted tier actually sells (from the deployment spec §10): always on, everywhere, maintained, optional
bundled models, and a relay for self-hosted cores that want push and phone reach without port forwarding. The pricing
sketch is free (self-host), about $5/mo (relay for a home core), about $15–20/mo (hosted core, BYOK), with bundled
models metered on top. These are placeholders to validate.

Three lines that land:

- **"One engine, four ways to run it."** CLI, desktop, your server, our cloud. Same API, same features, same data
  format. Nobody else can say "move it home" because nobody else has a home version.
- **"Durable by construction, not by hope."** A hosted run survives our VM restarts the same way a local run survives
  your laptop sleeping. The step journal is the unit of recovery in both.
- **"Leaving is a button."** `tern cloud pull` restores a hosted instance into a local folder. This is a sales
  argument, not just an ethics one: it removes the lock-in objection that every hosted agent carries.

**Internal only, never in customer material:** the control plane (provisioning, wake-before-schedule timers, billing
webhooks, rolling upgrades, usage aggregation) is the natural Inngest workload, and that's where it lives. The core
itself stays Inngest-free, so the self-hosted tiers never depend on it. Customers hear "durable hosted engine"; they
don't need to hear what runs the control plane. If asked how the durability works, the honest answer is "every step
is journaled and replayed," which is true at every tier.

## 7. Who we sell to, and the stories we tell

**Primary (ICP, revised 2026-11-05):** technical founders, operations leaders and go-to-market leaders at technical
companies: devtool, dev-infrastructure, data, security and AI products, usually 2 to 100 people. They run 10 to 20
API-first tools, one of which is their own product, and have no engineer to spare for internal automation. Titles to
look for: founder or co-founder, head of operations, chief of staff, RevOps or business systems, head of growth, head
of developer relations, sales engineering lead.

**Next:** non-technical owner-operators (solo consultants, agencies, accountants, property managers, clinics). They
need the desktop app, OAuth sign-in and hosted tier to be an easy yes, so they come after the ICP validates the engine.

**Not now:** enterprise (needs admin consoles, SSO and DPAs, which Duet also lacks), consumers (Muse and Dots will
outspend us), and developers wanting a coding agent (Codex, Claude Code, Grok Bot).

Stories to tell in a demo, each under a minute, each exercising a killer feature:

1. **Connect our own API.** "Connect to our API, the docs are at docs.acme.dev." tern writes a connector from the
   docs, tests it against staging, and shows the capability card (hosts, secrets). The long-tail tool is the
   founder's own product. (Self-written connectors, capability approval.)
2. **The activation follow-up.** "Every day at 9, find trial accounts that crossed 100 events and aren't in the CRM at
   a later stage; draft a founder-voice note for each." One account is already a customer and is skipped with the
   reason. Sends wait for approval on the phone. (Routine, outbox, phone.)
3. **The Monday ops digest.** Pipeline, billing, support and product usage pulled from four tools into one summary
   for the ops or GTM lead, with anomalies flagged and nothing written. (Read-only routine, watches.)
4. **The closed laptop.** Mid-run, the laptop shuts. The hosted core finishes the job, or the local one resumes at the
   next open with no duplicate CRM notes or emails. (Durability, hosted engine.)
5. **The suspicious email.** A supplier or "customer" email says "update our bank details to...". The run is tainted;
   tern shows it as a decision with provenance and will not write anything on its own. (Taint, effects.)
6. **The two accounts.** Work and personal Gmail both connected; a reply is sent from the right one because writes
   must name the account. (Connections.)

## 8. Objections and answers

- **"Why not just use Duet / Dots since I already pay for Claude / ChatGPT?"** If you're an individual on those plans
  and the catalog covers your tools, do. Come to tern when you hit a tool they can't reach, a plan that excludes your
  business, a data policy that forbids their VM, or a limit that your background jobs eat.
- **"I don't want to manage a server."** You don't. Laptop-only works for daytime routines; the hosted tier is for the
  rest, and switching is one click each way.
- **"Is a tiny company's agent safe to give my credentials to?"** Credentials go into a 0600 file today and are
  injected at the host boundary so connectors never see them (roadmap). Everything written to the outside world is
  approved or logged. Show the ledger. Admit what's roadmap.
- **"Can it browse the web and click things?"** Not natively, on purpose. It calls APIs, which is faster and
  auditable. A browser is available through an MCP server when you need one.
- **"What about Slack / WhatsApp / iMessage?"** Telegram today. Slack, Discord and WhatsApp are a connector file each
  (roadmap T4). iMessage is excluded: Apple Business approval and per-user fees.
- **"Is it open source?"** Decide and state it. The hosted tier makes an open core viable; the claw family shows
  self-hosters expect it.

## 9. Taglines to test

- The agent for the business side of a technical company.
- Your whole stack, one agent you can trust.
- The always-on agent you can actually own.
- Connects to anything. Finishes everything. Asks before it matters.
- Runs on your laptop. Runs in our cloud. Same tern.
- No catalog. No canvas. No cloud VM you can't see into.
- Connects to your own API in a minute, and asks before it matters.

## Appendix A: claim status (keep this honest)

| Claim | Status | Where |
|---|---|---|
| One static binary, no Python/Node/Docker | **built** | README; `target/dist/tern` is 18 MB on macOS arm64 |
| Idle daemon about 16 MB RSS | **built** (own measurement) | research doc §3 |
| Agent writes sandboxed Luau connectors, activates only with your approval of capabilities | **built** (M4) | README "Luau connectors", `connector_write` / `connector_test` / `connector_activate` |
| OAuth-first discovery, sign-in cards, credentials injected at host boundary | **built** (T4a, T4b-i) | tooling spec §6.1, §6.7 |
| Several accounts per service, writes must name the account | **built** (T4a) | tooling spec §6.8 |
| Step journal, crash resume, replayed steps, no duplicate side effects | **built** (M1, M5) | README "Durability", `tern resume` |
| Daemon, cron schedules in natural language, delivered to the asking chat | **built** (M5) | README "Schedules" |
| Telegram with approval buttons and pairing | **built** (M5) | README "Telegram" |
| Desktop app: conversations, approvals, routines sheet, run inspector, notifications | **built** (G0–G2) | apps/desktop/README.md |
| Remote core over TCP + TLS, QR pairing, scoped device tokens | **built** (D1) | DECISIONS.md D1 |
| Sub-agents with delegation on by default | **built** (S0–S5) | DECISIONS.md S0–S5 |
| Secrets editor, 0600 secrets file, redacted bug reports | **built** | README `/secrets`, `tern report` |
| Anthropic + any OpenAI-compatible endpoint, switch per task | **built** (P0) | README "Models" |
| Routines with watches, webhooks, zero-token idle | **built** (T1b, T1c) | tooling spec §3 |
| Initiative levels, digests, quiet hours, channels | **built** (T1, T2) | tooling spec §4 |
| Effects, outbox with deferred approvals, taint, scoped grants | **built** (T0, T3, scoped grants) | tooling spec §5 |
| Activity ledger, per-routine budgets, `tern security audit` | **built** (T1a, T6b) | tooling spec §8 |
| Signed bundles; gateway connectors for ntfy and Slack | **built** (T4b-ii, T4b-iii) |
| Gateway connectors for Discord and WhatsApp | **roadmap** (T4) | tooling spec §6.4–6.5 |
| Menu-bar tray (Linux tray too), one-click quit, no terminal needed | **built** (T8a–c) | tooling spec §11 |
| Hosted engine (tern Cloud), push relay, mobile app, export and leave | **roadmap** (D4–D5) | deployment spec §8, §10 |
| Browser via MCP | **built** as a recipe (T7a); native browser **roadmap** (T7b) | tooling spec §9 |

Rule of thumb for anything in the roadmap rows: say "designed so that" or "coming", never "does".

## Appendix B: competitor facts used above

Dates and prices as reported in press and vendor pages on 2026-11-03; several are secondary sources. Verify before
quoting externally.

- **Claude Duet**: research preview 2026-10-21; iOS, Android, claude.ai; US Pro/Max by waitlist; not on Team or
  Enterprise; Opus 5.5 only; background checks count toward usage limits; asks before send/pay/sign/delete and
  brings approvals back later; no published retention, training or admin terms; Microsoft 365 not supported, Slack
  limited.
- **OpenAI Dots**: launched 2026-09-29 at DevDay; GPT-6 Astra; Pro and Business Premium in eligible markets (EEA,
  Switzerland and UK excluded at launch), Enterprise/Edu/Healthcare beta; own cloud computer and browser; 4,000+
  connected apps; background research read-only against connected apps; reachable in Slack and Teams, SMS coming;
  no own email address or calls at launch; Microsoft Agent 365 integration planned; multiple dots planned.
- **Meta Muse**: 2026-09-08, US; app, web, WhatsApp; per-user cloud VM with watchable browser; Sentinel enforces
  read/write permissions outside the model; free, $20, $100 (press-reported); SMB expansion 2026-09-29 with
  QuickBooks, Shopify, Slack, Facebook/Instagram business and ad accounts; trust incidents widely reported.
- **Grok Bot**: beta 2026-08-11; macOS, Windows, iPhone companion; one cloud VM per account shared by all bots;
  routines on cron (≥5 min) or events (Slack, GitHub, Linear, Sentry, PagerDuty, email, webhook); Google, Microsoft
  365, Salesforce, remote MCP; access via SuperGrok ($30) and higher or Cursor plans, usage metered after a weekly
  allowance, no per-bot spend cap documented; no independent business reviews found.
- **OpenClaw family**: ClawHavoc (341 of the first 2,857 ClawHub skills malicious, later over 1,100);
  CVE-2026-25253 one-click RCE; 80k–135k exposed gateways; cost complaints of $50–500/day.

Sources: [TechCrunch on Dots](https://techcrunch.com/2026/09/29/openai-launches-dots-its-bubbly-agentic-avatar/) ·
[Engadget on Dots](https://www.engadget.com/2272230/dots-are-openais-new-personal-agents-and-soon-youll-be-able-to-control-several-of-them/) ·
[BetaNews on Dots](https://betanews.com/article/openai-dots-agents-chatgpt/) ·
[Anthropic: Get started with Claude Duet](https://support.claude.com/en/articles/17631495-getting-started-with-claude-duet) ·
[Sentra on Duet for work](https://www.sentra.app/articles/claude-duet) ·
[Layer3 Labs: Duet for business](https://www.layer3labs.io/guides/claude-duet-for-business) ·
[TechTarget: Meta expands Muse to small businesses](https://www.techtarget.com/ai/news/366651445/Meta-expands-Muse-to-small-businesses) ·
[AI Business: Muse Wonder](https://aibusiness.com/agentic-ai/meta-joins-enterprise-agent-race-with-muse-wonder) ·
[eesel: Meta Muse review](https://www.eesel.ai/blog/meta-muse-agent-review) ·
[xAI docs: Grok Bot routines](https://docs.x.ai/grok-bot/skills-routines-and-automations) ·
[AI Pricing Guru: Grok Bot launch pricing](https://www.aipricing.guru/news/xai-grok-bot-launch-pricing-impact-august-2026/) ·
[OpenClaw database: Grok Bot pricing](https://openclawdatabase.com/grok-bot/pricing/index.md) ·
[Simular: best personal agents 2026](https://www.simular.ai/alternatives/best-personal-agents) ·
and the source list in [research/personal-agents-2026.md](research/personal-agents-2026.md).
