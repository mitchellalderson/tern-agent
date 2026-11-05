# Vertical demo flows

Status: draft, 2026-11-04. Internal. Companion to [positioning-talking-track.md](positioning-talking-track.md) and
[launch-plan.md](launch-plan.md) §4.1. These demos assume the next work is done: the outbox and deferred approvals (T3),
OAuth sign-in (T4a), and plain-language capability screens. Where a flow needs anything else, it says so.

**Priority revised 2026-11-05.** The ICP is technical founders, operations leaders and go-to-market leaders at technical
companies (see [positioning-talking-track.md](positioning-talking-track.md) §2 and §7). Flow C (founder-led sales at a
devtool) is now the lead demo, and Flow D (an ops and go-to-market weekly digest) is new. Flows A and B target
non-technical owner-operators, which is the next segment; their scripts are kept and not filmed first.

Each flow is a 90-second recording and doubles as the script for a 20-minute live call with a prospect. The point of
each is to answer one question for one kind of buyer: *does it work with the tool I'm stuck with, and can I trust what
it does?*

## 1. The shared skeleton

Every vertical demo has the same five beats. Only the tool, the data and the routine change.

| # | Beat | What the viewer must see | Feature it proves |
|---|---|---|---|
| 1 | **Name the tool** | The user types the name of a tool no catalog covers. Nothing else. | Auto-connector |
| 2 | **Sign in, don't configure** | A "Sign in to X" card. No API key, no docs. If the tool has no OAuth: the agent asks for a key through the secret card and never shows it. | T4a, secret requests |
| 3 | **Read the permissions in English** | "tern will be able to: read your invoices and clients. It cannot send, change or delete anything." One tap to approve. | Capability approval |
| 4 | **The routine, then the outside action waits** | The routine runs and drafts. The one outbound action (an email, a text, a status change) appears as an approval on the phone. The rest of the job keeps going. | Outbox, deferred approvals |
| 5 | **Break it** | `kill -9` mid-run, restart, the run finishes and nothing is sent twice. Show the receipt. | Journal, receipts |

Rules for all three:

- **Read-only first.** The connector is approved read-only in beat 3. The first write appears only in beat 4, as a
  separate approval. A non-technical viewer should never see a single approval that grants both.
- **Show the receipt last.** End on the list of what was sent, to whom, when, and which step. It's the line that
  differentiates tern from every "trust me" assistant.
- **No jargon on screen.** No "Luau", "journal", "effects", "taint". The viewer sees "tern wrote a connection to X",
  "waiting for you", "picked up where it stopped".
- **Real tool, or an honest stand-in.** See §6.

## 2. Flow A: bookkeeping and accounting firm

**Buyer:** the firm owner or an ops lead at a 3-to-25-person practice. Pain: chasing clients for missing documents and
unpaid invoices across a stack that differs per client.

**Tools in the demo:** Karbon is the pick (public API, no OAuth, so beat 2 uses the secret card, not a sign-in). Canopy
is the backup (no accounts-receivable data over the API), TaxDome is unconfirmed. The full script, research and shot
list are in [demo-accounting-firm.md](demo-accounting-firm.md).

**Seeded state:** 14 clients, 3 with unpaid invoices over 30 days, 2 with outstanding "send us your bank statements"
tasks past due. One client is flagged "do not contact, call instead".

**Script**

1. *User:* "Connect to our practice management tool, it's called Karbon." (beat 1)
2. tern reads the API docs, drafts a connector, tests it against a sandbox call, and shows a sign-in card. User clicks
   Sign in. (beat 2)
3. Permission card: "Read clients, work items, invoices. Cannot send or change anything." Approve. (beat 3)
4. *User:* "Every Monday at 8, find clients with invoices over 30 days or missing documents, and draft a polite
   reminder for each." tern creates the routine and says so.
5. *User:* "Run it now." tern finds 5 clients, drafts 5 emails, skips the "do not contact" one and says why.
6. On the phone: 5 approval cards, each showing the draft and the client. User approves 2, edits 1, denies 1. The rest of
   the job (the summary for the partner) keeps going while two wait. (beat 4)
7. Mid-way through sending, `kill -9`. Restart. Remaining sends complete. The receipt shows 3 sent, 1 denied, 1 skipped,
   none duplicated. (beat 5)

**Money line:** "It chased five clients before you had coffee, and it skipped the one you told it to call."

**What this demo must not show:** tern changing a client's status or touching the ledger. It drafts and sends reminders
only.

**Needs:** a connector the agent can write against a real practice-management API; email sending through an existing
connector; "do not contact" read from a field or a note, not hard-coded.

## 3. Flow B: field-service operator

> **Superseded 2026-11-04** by [demo-property-brief.md](demo-property-brief.md): a home-service pre-visit brief built
> on the Maricopa County Assessor API. It needs no field-service vendor's API and is easier to show on screen. The
> Jobber version below is kept for reference.

**Buyer:** owner of a 5-to-30-person home-services or trades business (HVAC, plumbing, cleaning, landscaping). Pain:
quotes that never get followed up, unpaid invoices, schedule gaps, all in a field-service tool whose API nobody has
touched.

**Tools in the demo:** a field-service platform (Jobber, ServiceTitan, Housecall Pro or a regional one; **verify API
access and plan tier**, since some gate the API behind higher plans), SMS or email.

**Seeded state:** 20 open quotes, 6 older than a week and unanswered; 4 unpaid invoices; tomorrow's schedule has a
2-hour gap.

**Script**

1. *User:* "Connect to Jobber." Sign in. Read-only permission card. (beats 1–3)
2. *User:* "Which quotes have gone a week with no answer?" tern answers from live data, 6 quotes, with customer names and
   amounts. This is a read-only win before any write.
3. *User:* "Text each of them a friendly nudge, and tell me about any gaps tomorrow." tern drafts 6 texts and notes the
   2-hour gap with the nearest 3 open quotes that fit it.
4. Approval on the phone: the 6 texts come as one batch card (approve all, or open to edit one), because 6 separate
   pings is what makes people turn agents off. A text to a customer who replied yesterday is held with a note. (beat 4)
5. `kill -9` between texts 3 and 4, restart, the batch finishes from text 4. Receipt shows each text, time and phone
   number. (beat 5)

**Money line:** "Quotes that would have gone cold got a nudge by 9am, and you read a six-line summary."

**What this demo must not show:** pricing changes or discounts. Anything that commits the business to money stays a
human act.

**Needs:** batch approvals in the outbox (or the demo shows six cards and says so honestly); an "already replied" check
from the tool's data; SMS through a connector the agent can use.

## 4. Flow C: founder-led sales at a devtool startup (lead demo)

**Buyer:** a technical founder or head of growth at a devtool, infrastructure, data or AI company. The point is that
the long-tail tool is **their own product's API**, which no catalog will ever have.

**Tools in the demo:** the founder's own product API or database (the connector moment), a CRM (Attio or HubSpot), email,
Slack.

**Seeded state:** 40 trial accounts; 5 crossed an activation threshold this week; 3 are already in the CRM with a
stage; 1 is a customer already (a trap: the agent must not pitch it).

**Script**

1. *User:* "Connect to our API. Docs are at docs.acme.dev." tern writes a connector from the docs, tests it against a
   staging key supplied through the secret card, shows the permission card ("read accounts and usage events").
   (beats 1–3)
2. *User:* "Every day at 9, find trial accounts that hit 100 events and aren't in the CRM at a later stage, and draft a
   founder-voice note for each."
3. Run now: 5 accounts qualify; 1 is already a paying customer and is excluded with the reason; 4 drafts reference that
   account's actual first-week usage.
4. Phone approvals: 4 drafts. One approval is deferred (the user taps "later"); the CRM notes for the other three are
   written after approval, and the deferred one sits in the outbox, not lost. (beat 4)
5. `kill -9`, restart, no duplicate CRM notes. Receipt. (beat 5)

**Money line:** "The people who actually tried it hear from you the same day, without you living in a CRM."

**What this demo must not show:** automated cold outreach to unknown contacts. The flow is about warm, in-product
signals. That is both the honest version and the differentiated one.

**Needs:** the connector written from docs against a live staging API; CRM write via an existing connector or MCP.

## 4b. Flow D: the ops and go-to-market weekly digest

**Buyer:** a head of operations, RevOps or business-systems lead, or a head of growth or developer relations, at a
technical company. Pain: the Monday review is assembled by hand from the CRM, billing, support and product usage, and
the interesting numbers are never in the same place.

**Tools in the demo:** a CRM (Attio or HubSpot sandbox), a billing system with a test mode (Stripe), a support or
community tool with an API, and the product's own usage API (the connector moment). Slack or email for delivery.

**Seeded state:** 60 accounts across CRM and billing; 4 paying customers whose usage fell by half this week; 2 open
deals with no activity in 14 days; 3 support threads from the same account; one deliberate mismatch (a customer
marked churned in the CRM but still billed), which is the trap the agent must flag, not fix.

**Script**

1. *User:* "Connect to our usage API." tern writes the connector, shows the capability card (read accounts and
   events), user approves. The CRM, billing and support tools are already connected. (beats 1-3)
2. *User:* "Every Monday at 8, send me a one-page summary: customers whose usage dropped, deals gone quiet, support
   load by account, and anything that doesn't match between the CRM and billing."
3. Run now: the digest names the 4 customers, the 2 stale deals and the 3-thread account, and flags the churned-but-billed
   mismatch with both records shown. Everything it did is a read. (beat 4 is skipped by design)
4. *User:* "Draft a check-in for the four customers and put a task on each owner." Four drafts and four tasks wait
   as approvals on the phone; the mismatch stays a note, because no write was asked for. (beat 4)
5. `kill -9` between the third and fourth task, restart, the run finishes, no duplicate tasks. Receipt. (beat 5)

**Money line:** "Your Monday review was waiting when you opened your laptop, and the one thing that didn't add up was
on top."

**What this demo must not show:** writes to billing, or any change to a customer's plan or status. The digest reads;
the only writes are drafts and tasks, each approved.

**Needs:** a connector the agent writes against the product's usage API; read access to three tools; tasks written
through an existing connector or MCP server; the seed script must include the mismatch.

## 5. How the flows compare

| | C: devtool founder (lead) | D: ops / GTM digest (lead) | A: accounting (next segment) | B: field service (next segment) |
|---|---|---|---|---|
| Strongest on | the connector moment | receipts, a read-first digest | trust, receipts | batch approvals, speed to value |
| Weakest on | audience is small and already technical | needs read access to three tools | needs a tool with a usable API | API access varies by plan |
| Reach | HN, X, LinkedIn, your network | ops, RevOps and DevRel communities, LinkedIn | firm-owner communities, bookkeeping forums | trade-business groups, vendor communities |
| Risk | gets read as "yet another SDR bot" | a digest can look like a dashboard | confidentiality worries | SMS compliance, customer-facing mistakes |
| Film first? | **yes, as the fastest to ship** | **yes, second** | later | later |

C is the cheapest to produce and the best launch asset for founders; D tests whether ops and go-to-market leaders get
value from reads before any write. Shoot both. A and B wait for the next segment.

## 6. Honesty, seeding and pass criteria

**Real tool or stand-in.** Prefer a real vendor sandbox with fake data. If the vendor has no sandbox, run a small mock
server with that vendor's real API shape and say "demo data" on screen. Never film against a customer's real account.
Never present a mock as a real vendor's product; use the vendor's name only where the demo really connects.

**Seed data deterministically.** One script per flow creates the same clients, quotes or accounts every time, including
the trap (the do-not-contact client, the already-replied customer, the existing paying account). The trap is what
makes the demo credible: the viewer sees tern *not* do something.

**Claims.** Every beat that depends on something unbuilt is tagged in the shooting script so the video never shows a
screen that doesn't exist. If a demo fakes one step, the caption says so.

**Pass criteria**, per flow, before it goes in front of anyone:

- A person who has never seen tern can say, unprompted, what it just did to which clients.
- Nothing was sent that the viewer didn't approve, and they can find the receipt.
- The `kill -9` beat leaves no duplicate on the receipt, checked in the downstream tool and not only in tern's log.
- The permission card was understood: ask the viewer what tern could and could not do. Wrong answers mean the copy
  fails, not the viewer.

## 7. Using these on a call

Don't present. Ask for the tool name in advance, check its API docs, and run beats 1–3 live on their tool with
read-only access (beats 4–5 on seeded data). The moment it reads their real data correctly is the demo. If the connector
fails on their tool, that is the most valuable result of the call: log which tool, which API quirk, and why.
