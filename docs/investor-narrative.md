# tern: investor narrative (one page, draft)

Status: draft, 2026-11-05. Items in [brackets] are blanks only the founder can fill. Every product claim is tagged
**built** or **next** so the page can be shared without overselling. Sources for the market claims are in
[research/personal-agents-2026.md](research/personal-agents-2026.md) and
[positioning-talking-track.md](positioning-talking-track.md).

## One line

**tern is the AI agent for technical founders, operations leaders and go-to-market leaders: it connects to
your whole stack, including your own product's API, picks up where it stopped after a crash or a sleep, and never acts
for you without a receipt.**

## The problem

The people who run the business side of a technical company (founders, ops leaders, heads of growth and developer
relations) lose hours a day joining product usage, CRM, billing, support and inbox across a dozen API-first tools.
The big assistants are built for individuals or enterprises, run in the vendor's cloud, lock you to one model, and cover only
the tools in their catalog. The open-source agents are hackable but are hobbyist tools: heavy to run, thin on trust.
When an agent can send email and create invoices, "it usually works" is not good enough.

## The product

One static binary (no Python, no Node, no Docker) with a desktop app, a terminal UI and Telegram, all talking to one
daemon. Several agents, each with its own persona, tools and memory, start from 23 starters (research, SEO audits, code
review, inbox, writing and more) and delegate to each other.

What makes it different, in the order we would say it:

1. **Connects to anything.** When there is no connector, the agent reads the API docs, writes a small sandboxed
   connector, tests it, and shows exactly which hosts and credentials it may touch before you approve. **built**
2. **Finishes the job.** Every step is journaled before and after it runs. Kill it mid-task and it resumes without
   sending the same email or creating the same invoice twice. **built**
3. **Nothing goes out without a receipt.** Risky steps wait for approval, scoped to the exact thing approved. Every
   action lands in a hash-chained ledger. Untrusted web content is tracked so it cannot quietly steer later actions.
   **built** (script-level grants and workspace confinement: **next**, in alpha hardening)
4. **Yours to run, or hosted.** Any model vendor, including local models. Run it on your machine, your server or ours,
   with the same engine and an export-and-leave path. Local and paired remote devices **built**; hosted accounts and
   billing **next**.

## Why now, why us

- Every major lab has launched an always-on assistant this year. The business side of technical companies is the
  segment they serve worst: the engineer-facing agents aim at coding, the business-facing ones are dominated by trust
  failures, and none offers self-hosting or a choice of model.
- Our architecture was built for that gap from day one: durable execution, effects checked outside the model,
  vendor neutrality. Those are hard to retrofit onto a cloud-VM assistant.
- Small and fast: a sub-20 MB binary, idle under 30 MB RAM, no telemetry. Credible for technical
  buyers, who check the footprint, the sandbox and the ledger before they trust a tool.

## Where we are

- Working software across macOS and Linux: daemon, TUI, desktop app (macOS), Telegram, scheduler, MCP, signed bundles.
- Alpha release in progress, local-only: coding-agent mode, per-workspace confinement, first-run polish. **next**
- Traction: [users / waitlist / active installs / retention: fill in honestly, even if small].

## Business model

Free, local and open for individuals. Paid hosted accounts (Sign in with Google, Stripe billing) for people who want
it running without running it, then team plans with shared agents and admin controls. **next**
[Pricing hypothesis: fill in.]

## Risks we know about

Distribution against well-funded labs; a security model that must keep holding as capabilities grow; an install path
that is still technical; hosted tier not yet built. Mitigation: a narrow ICP (technical founders, operations leaders and
go-to-market leaders at devtool, infrastructure, data and AI companies) who can install it today, proof through real
alpha users, and shipping the hosted tier on the same engine rather than a
rewrite.

## The ask

[Amount, use of funds, milestones for the next 12 months: fill in.]
