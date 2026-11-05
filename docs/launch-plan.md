# tern launch and validation plan

Status: draft, 2026-11-03. Internal. Companion to [positioning-talking-track.md](positioning-talking-track.md).
That document says what we claim; this one says how we find out whether anyone cares, and whether anyone pays,
while spending as little as possible before we know.

## 1. The two questions

1. **Interest.** Will technical people install a self-hosted personal agent that is one small binary, survives crashes,
   and writes its own sandboxed connectors? Answered by a public launch of the engine as it exists today.
2. **Money.** Will enough of them pay for always-on hosting and phone reach? Answered by a priced waitlist, before any
   hosting infrastructure is built.

The order matters. The hosted tier sells to people who already showed up. The ICP (technical founders, operations
leaders and go-to-market leaders at technical companies, revised 2026-11-05) is who the launch is aimed at. The
non-technical owner-operator audience from the positioning doc comes after both, once the desktop app, OAuth sign-in
and the outbox exist (Stages 2–4 of the tooling spec).

## 2. Principles

- **Ship what is built.** M0–M6, D1, S0–S5 and P0–P1 are a complete product for a technical user. Don't block the
  launch on the spec backlog; use the backlog as the public roadmap.
- **No telemetry.** It contradicts the pitch. Measure with GitHub release downloads, stars, issues, a chat group, and
  a waitlist. Accept that the numbers are coarse.
- **One demo, one story.** Every post, screenshot and README section shows the same thing: a routine that finds
  overdue invoices and asks for approval on your phone, surviving a kill mid-run. Breadth comes later.
- **Price the waitlist.** A free waitlist measures curiosity. A waitlist with "$5/mo relay, $15/mo hosted" on the form
  measures intent. Show the prices.
- **Kill criteria are written down before launch** (§7), so a quiet launch is a result, not a mood.

## 3. Phase 0: launch readiness (one to two weekends)

Everything here is required before the post. None of it is product work.

| Item | Why | Done when |
|---|---|---|
| **Employer side-project check** | Mitchell works at Inngest; the hosted control plane is meant to run on Inngest. Confirm the side-project and IP policy in writing before anything is public. | Written OK, or a clear boundary |
| **Name and repo** | The checkout is `ox_agent`, the product is `tern`. Check the name is free on GitHub, crates.io and as a domain. | Public repo `owner/tern`, domain registered |
| **LICENSE file** | `Cargo.toml` says `MIT OR Apache-2.0` but no LICENSE file exists. HN readers check within the first minute. | `LICENSE-MIT` and `LICENSE-APACHE` at the root |
| **install.sh pointed at the repo** | It still dies with "set TERN_REPO=owner/name". | the one-line curl install works on a clean macOS and Linux box |
| **A tagged release with binaries** | `release.yml` exists; it needs a tag and a checked download on all four targets. | `v0.1.0` with 4 assets and checksums |
| **README first screen** | The first 20 lines must say what it is, show the demo, and give the one-line install. Today they start with install, which is right, but there is no picture. | A GIF or screenshot above the fold |
| **SECURITY.md** | The pitch is "the safe small one". A reporting address and a one-paragraph threat model (what the sandbox covers, what it doesn't) make the claim credible. | File exists, links from README |
| **A public roadmap** | The spec backlog, trimmed to one page, in `ROADMAP.md`. Turns "it doesn't do X" issues into "it's on the roadmap". | File exists, links to the specs |
| **A place to talk** | GitHub Discussions is enough. A Discord invites moderation work too early. | Discussions on |
| **The waitlist** | One static page: what hosted tern is, the two prices, an email field. No backend beyond a form service. | Page live, counts visible to us |

Skip for launch: a website beyond the waitlist page, a logo, docs site, crates.io publishing, Homebrew tap. Add the
tap in week 2 if people ask.

## 4. Phase 1: the demo and the post (week 1)

### 4.1 The demo

Ninety seconds, recorded in the terminal and on a phone, no voiceover needed. Script:

1. `tern setup`, paste a key, pick a model. (Ten seconds. Shows there is no config file to write.)
2. "Every weekday at 8, list invoices over 30 days in <tool> and draft a follow-up for each." tern creates the
   routine and says so.
3. Run it now. tern reads, drafts, and the approval for the first send pops up on Telegram with Allow / Deny buttons.
4. Kill the daemon with `kill -9` while the run is mid-way. Restart. `tern runs show` prints the journal with
   `(replayed)` steps; the run finishes without a duplicate draft.
5. "Connect to <obscure tool>." tern writes a Luau connector into drafts, tests it, and shows the capability card
   (hosts, secrets). Approve. Done.

Steps 4 and 5 are the ones nobody else can show. If the demo has to be cut, cut 1 and 2.

Given the ICP, the demo's routine can be the Flow C version in [vertical-demo-flows.md](vertical-demo-flows.md)
(trial accounts that crossed a usage threshold, followed up in founder voice) instead of invoices; the beats are the
same. Whichever is chosen, the tool should be one with a plain HTTPS API and a free tier, so the connector in step 5 is
real and the viewer can repeat it. Pick one and keep the connector in `examples/`.

### 4.2 Where to post, and the angle for each

| Place | Angle | Title shape |
|---|---|---|
| Hacker News (Show HN) | Durability and smallness. HN rewards engineering honesty: say what it doesn't do. | "Show HN: tern, an 18 MB personal agent that survives being killed mid-task" |
| r/rust | The Rust story: static musl, rustls, no OpenSSL, SQLite journal, Luau sandbox, clippy-clean. | "A personal AI agent as one static Rust binary" |
| r/selfhosted | "Runs on your box, approves from your phone, no VM, no Node." Contrast with OpenClaw's exposure numbers. | "Self-hosted personal agent that isn't an exposed gateway" |
| Lobsters | Same as HN, shorter. | |
| X / Bluesky | The 90-second demo as a video, the kill-and-resume moment as the thumbnail. | |
| LinkedIn | The ops and go-to-market side of the ICP. Lead with the outcome ("it followed up with the five trial accounts that hit 100 events, and skipped the one that's already a customer"), the demo as native video, and the receipt screenshot. | "The agent for the business side of a technical company" |
| Communities for ops, RevOps, developer relations and founder-led sales | Share the demo and the connector moment, not the repo. Ask for the tool they are stuck with and offer to run it live. | |
| Rust and AI newsletters (This Week in Rust, TLDR AI) | Submit after the HN post, with the HN link. | |

Post HN on a Tuesday to Thursday morning, US Eastern. Have Phase 0 done at least two days before, so the install
command has been tested by someone other than the author.

### 4.3 What to have ready on launch day

- Answers to the predictable questions, in `docs/faq.md`: why not OpenClaw, **why not ZeroClaw or IronClaw** (the
  two Rust claws; the honest answer is the journal, host-sandboxed agent-written connectors with capability approval,
  and the same engine local or hosted), why Luau and not WASM (IronClaw chose WASM; expect this one), why Telegram and
  not Slack, why no browser, what the sandbox does on macOS versus Linux, is my data sent anywhere, how is this
  different from Claude Duet.
- An "Inngest" answer that is true and short: the core has no Inngest dependency; a future hosted control plane may
  use it. Don't lead with it and don't hide it.
- Time to respond to issues for 48 hours. First-day issues are the best signal of what real installs hit.

## 5. Phase 2: measuring interest (weeks 1–3)

Numbers are targets to set now and adjust after the first launch, not predictions. They are calibrated against the
category as of 2026-11-03 (research notes §2 and §6): OpenClaw has 394k stars, Hermes Agent 260k, ZeroClaw 33k.
ZeroClaw, the closest comparable (a Rust single-binary claw from an unknown team), drew about 3,400 stars within days
of its February 2026 launch, riding the OpenClaw wave. Hermes reached about 100k in seven weeks with a known lab
behind it. Treat ZeroClaw's first days as the ceiling for an unbranded solo launch into a now-mature category, not
the expectation.

| Signal | Source | Weak | Good | Strong |
|---|---|---|---|---|
| Stars, 7 days | GitHub | < 300 | 1,000 | 3,000+ (ZeroClaw's first days) |
| Release downloads, 14 days | GitHub release assets | < 150 | 500 | 2,000+ |
| Installs that got to a first run | Issues, discussions and replies mentioning a working setup (no telemetry, so this is a count by hand) | < 15 | 50 | 200+ |
| Telegram pairings | Same, by hand | < 5 | 15 | 60+ |
| Connectors written by users | PRs, gists, discussion posts | 0 | 3 | 10+ |
| Waitlist signups at the stated prices | Form | < 20 | 50 | 300+ |
| Inbound from ICP companies (technical founders, ops or GTM leaders) | Email, DMs, LinkedIn | 0 | 2 | 5+ |

Why stars moved up and the money rows didn't: the claw numbers show the self-hosted audience is far larger than the
original targets assumed, so a post that lands should clear 1,000 in a week. Nothing in the category gives evidence
about paying for a $5–20 tier, so the waitlist and inbound rows stay where they were; they are the test, not the
calibration.

What the rows mean:

- **Stars without downloads** means the story works and the install doesn't. Fix install friction before anything
  else.
- **Downloads without pairings** means people try the CLI and stop before the daemon. That is the argument for
  Stage 1's `tern service install` and one-click stop (T8a), and for the tray (T8b).
- **Connectors written by users** is the single best leading indicator. It means the moat is real for someone other
  than us.
- **Waitlist signups** are the only money signal this early. Treat 50 at a stated price as enough to build the relay
  (§6).

## 6. Phase 3: testing willingness to pay (weeks 3–8)

Build the smallest paid thing, not the hosted tier.

1. **tern Connect, about $5/mo** (deployment spec §6.5 and §8). A relay so a home core gets a public URL and push
   notifications without port forwarding. It is one small service, no per-tenant VMs, no model gateway, and it sells
   to exactly the people who installed the self-hosted version. Gate: 50 waitlist signups.
2. **tern Cloud, about $15–20/mo** (deployment spec §10). One core per tenant on microVMs with scale-to-zero. Needs
   D4–D5 and a control plane. Gate: 30 paying Connect customers, or 200 waitlist signups with a majority choosing
   "hosted" over "relay" on the form.
3. **Bundled models, metered.** Only after Cloud has customers who ask for it.

Convert the waitlist with a founding-member offer: a fixed price for a year, in exchange for a 20-minute call. The
calls are the research; the price is the validation.

Pricing stays placeholder until ten people have paid. Then adjust against the two reference points buyers will use:
$20/mo for ChatGPT or Claude (which now includes an agent) and $0 for running tern themselves.

## 7. Kill and continue criteria

Decided now so the launch result is read calmly.

- **Continue to Phase 3** if two of: 1,000 stars, 500 downloads, 50 priced waitlist signups, 3 user-written
  connectors. The connector and waitlist rows count double: either one alone, at "good", is enough to continue.
- **Relaunch once** if the install or demo visibly failed (issues say so) but the story got reach. Fix, wait four
  weeks, post again with "what changed".
- **Stop building the paid tier, keep the open-source project** if the launch gets reach but fewer than 20 priced
  signups after two posts. Interest without intent is a fine outcome for a side project; it just isn't a business.
- **Shelve** if two launches produce under 300 stars each. In a category where the fourth-place project has 33k,
  that means the story isn't landing, and more features won't fix it.

## 8. Product work that strengthens the launch

From the tooling spec's Stage 1 (quick wins, days each). Do these only if they fit before the post; none blocks it.

| Item | Why it helps the launch |
|---|---|
| **T8a**: `tern service install`, `tern start` / `stop`, "Quit tern completely" | Turns "run `tern daemon` in tmux" into a one-liner. Removes the top predictable install complaint. |
| **T0**: effects on every tool, money-movement deny | Lets the README say "reads are free, writes ask, money never moves" truthfully today. |
| **T1a**: `NO_REPLY` and quiet hours on schedules | Without it, the demo's daily routine pings even when there is nothing. Reviewers notice. |
| **An `examples/` folder** | Three connectors (the invoice tool, a weather API, a GitHub notifications watch) and two routines. Shows what "write your own" looks like without reading the spec. |

Do **not** do before launch: routines with watches (T1b), channels (T2), the outbox (T3), OAuth (T4a), the tray
(T8b), Postgres (D3), anything hosted (D4–D5). They are the roadmap, and the roadmap is part of the post.

## 9. Timeline

| Week | Work | Output |
|---|---|---|
| 0 | Phase 0 checklist; optional T8a; demo recorded | Public repo, v0.1.0, waitlist page |
| 1 | Post (HN, r/rust, r/selfhosted, X); respond to issues | First numbers |
| 2–3 | Fix install issues; v0.1.x; newsletter submissions; count the §5 table | Continue / relaunch / stop decision |
| 3–6 | If continuing: tern Connect (relay + push), founding-member calls | 10 paying users, pricing evidence |
| 6–8 | Stage 1 remainder, then Stage 2 (routines, taint, outbox) | v0.2.0 with the roadmap's first big items; second post |
| 8+ | Cloud decision on the §6 gates | |

## 10. Open items

- The invoice tool for the demo and the example connector: pick one with a free tier and a plain API.
- Whether to publish on crates.io at v0.1.0 (easy, but the binary is the product and `cargo install` builds slowly).
- The name check: `tern` on GitHub, crates.io and a `.dev` or `.sh` domain.
- Read the Show HN threads for ZeroClaw and IronClaw before posting (web search didn't surface them; use HN's own
  search). The first-day comments are the FAQ we'll get.
- Who else can run the install on a clean machine before the post.
- The exact waitlist copy and the two prices to show. Defaults: $5 relay, $15 hosted.
