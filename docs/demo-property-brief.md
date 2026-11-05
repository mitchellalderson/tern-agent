# Demo script: home-service pre-visit brief (Maricopa County Assessor)

Status: draft, 2026-11-04. Internal. Replaces the Jobber-based Flow B in [vertical-demo-flows.md](vertical-demo-flows.md).
Target: a 75-second recording, and a live demo anyone in the room can steer by shouting an address. Assumes T3 (outbox)
and plain-language capability screens. It does not need OAuth.

## 1. Why this one

- **Public, read-only, deterministic.** Nothing to break, no customer data, no vendor sandbox to wait for. Assessor data
  changes slowly, so a take today looks the same tomorrow.
- **Obscure on purpose.** No catalog or MCP covers it. Mitch has already seen the agent build the connector on the first
  try, with no errors. That is the hardest part of the pitch, and it is already the strongest.
- **Visual.** The output is a one-page card per address (age, size, lot, pool, last sale). A viewer understands it
  without knowing anything about APIs.
- **No dependency on a field-service vendor's API plan.** The schedule can be a pasted list or a calendar.

Its weakness: it proves the connector, not the trust story. The accounting demo
([demo-accounting-firm.md](demo-accounting-firm.md)) proves trust. Film both; lead with this one for launch awareness.

## 2. What the API gives us (verify before filming)

From the Assessor's 2021 API documentation PDF and third-party wrappers, not from a live check:

- Parcel lookup `/parcel/{apn}`, property information `/parcel/{apn}/propertyinfo`, property address, and a
  `/search/property/?q=` search that pages 25 results at a time.
- A token, issued by request through the Assessor's contact form ("API Question/Token") and sent in a request header.
  No OAuth, so beat 2 is the secret card.
- One wrapper marks valuation, improvement, zoning and rental endpoints as "not currently supported". Those may be
  stale or live. **The script uses only fields the live response returns.** Do the preflight in §3 before writing a
  single caption that names a field.

Sources: [Assessor API documentation (PDF)](https://api.mcassessor.maricopa.gov/file/home/MC-Assessor-API-Documentation.pdf),
[mcaapi wrapper](https://pypi.org/project/mcaapi/).

Note that the docs are a **PDF**. Point tern at the PDF URL in beat 1; "it read a PDF and wrote the connector" is part
of the story.

## 3. Preflight

- [ ] Assessor API token in hand (request it first; it is emailed, not instant).
- [ ] Call the live API three times by hand. Write down which fields come back for a house, a condo and a vacant lot.
      The brief template in §5 uses only those.
- [ ] Fresh `~/.tern`, so no connector exists. Record the real first-time connector build, not a cached one.
- [ ] Six addresses, chosen as in §4. Run all six by hand against the API first.
- [ ] SMS or email to a sink you control. No demo message reaches a real customer.
- [ ] Phone paired for approvals. Three full dry runs, including the `kill -9` beat.

## 4. The six addresses

The company is fictional: **Saguaro Roofing & Repair**. Tomorrow's schedule is six jobs.

| # | Address type | Why it is there |
|---|---|---|
| 1 | Single-family home, built 1990s | The ordinary case |
| 2 | Single-family home, built 2010s | Contrast: newer roof, different advice |
| 3 | Home with a pool | The brief mentions it; shows it read the record |
| 4 | Condo or townhome | Several parcels or an HOA; tern notes the roof may not be the owner's job |
| 5 | Vacant lot or non-residential parcel | tern says the record doesn't describe a house, and doesn't invent one |
| 6 | **An address in Pima County (Tucson)** | **The trap.** Not in this county's data. tern must say so and not guess |

**Use consenting addresses or public places for the recording**, such as your own home, a friend who agreed, or a
public building. The brief does not include owner names (see beat 4), but a street address on a screen is still a
person's home. Blur anything that isn't yours or approved.

## 5. The shot list

### Beat 1: name the tool (0:00–0:10)

- **Screen:** empty conversation. Paste the six addresses as tomorrow's schedule (one line each, with a window like
  "9–11am").
- **User types:** `These are tomorrow's jobs. Before the crew goes out, pull what the county has on each house. The API is the Maricopa County Assessor's, docs are here: <PDF URL>`
- **tern does:** shows the same short activity list as other demos ("reading the API guide", "drafting a connection",
  "testing it with a harmless request"). No code on screen.
- **Caption:** *No connector for this existed a minute ago.*

### Beat 2: the token (0:10–0:20)

- **tern:** "The Assessor issues a token by request. Paste it here. I can't see it after this." Secret card, one field,
  closes to `Saved. tern can use it, not read it.`
- **Test:** one lookup. "Connected: found 6,2xx results for 'Central Ave'" or similar; pick a harmless query.
- **Caption:** *The model never sees the token.*

### Beat 3: the permission card (0:20–0:28)

> **tern wants to connect to the Maricopa County Assessor**
> It will be able to: **look up** parcels and property details.
> It cannot: change anything. This service is read-only.
> It talks only to: `api.mcassessor.maricopa.gov`.

- **User taps:** Approve.
- This is the easiest permission card of any demo: a public read-only service. Say that in narration; it makes the
  later cards in other demos feel earned.

### Beat 4a: the briefs (0:28–0:48)

- **tern does:** looks up five addresses, one at a time, with a progress line: "3 of 6: 2210 W …".
- **Per-address card** (only fields the live API returns; below is the intended shape):

  > **1 · 9–11am · 4412 E Example Dr**
  > Built 1994 · 2,140 sq ft · lot 7,200 sq ft · pool: no · last sale: 2019
  > *County record. Verify on site.*

- **Data minimisation, on screen:** no owner name, no mailing address, no sale price. A caption says: *tern pulls only
  what the job needs.* This is a product decision, not decoration; see §7.
- **Address 5:** "Record shows a vacant parcel. No house to brief. Check the address with the customer."
- **Address 6:** "This address is in Pima County, outside the Assessor's data. I didn't guess; here's what I'd need."
- **Caption:** *It said so when it didn't know.*

### Beat 4b: approvals (0:48–1:05)

Two outbound actions, both held for approval. Neither contains the county data.

1. **Crew brief:** "Send tomorrow's job sheet with these notes to the crew lead?" Card on the phone, with the six
   cards as one document. Approve.
2. **Customer confirmations:** five texts, "Saguaro Roofing will be at your home between 9 and 11am tomorrow." Show them
   as a batch card, tap Approve all, then open one and edit before approving to show it can be changed.

- **Do not put county facts in a customer text.** "We see your house was built in 1994" reads as surveillance. The data
  is for the crew, not for the customer. Say this to anyone who asks why.
- **Deferred approvals moment:** leave one customer text unanswered. The crew brief goes out; the text waits in the
  outbox with a "waiting for you" label.
- **Caption:** *Waiting on you. The rest of the job keeps going.*

### Beat 5: break it, then the receipt (1:05–1:15)

- `kill -9` while texts are going out. Restart. Remaining texts complete.
- **Receipt:** "Sent 4 · Waiting 1 · Duplicates 0." Confirm in the sink inbox, not in tern's log.
- **Closing line:** `Every crew knew the house before knocking, and nobody typed an address into a county website.`

## 6. The audience-steered live version

This demo has a trick the accounting demo lacks: **anyone can name an address**. On a call or at a talk, after the
recorded flow, ask: "Give me an address in Phoenix." tern looks it up live. The reaction when it works on an
address you hadn't prepared is the strongest moment you have.

Rules: confirm the address is in Maricopa County first; the room can't use a Tucson address (but if they do, you get
the trap for free). Never read out an owner name, even if the API returns one.

## 7. Before this is a product, not a demo

Arizona law treats using public records for sale, resale or any expected monetary gain as a **commercial purpose**, and
the Assessor has a Commercial Purpose Public Record Request form and penalties for misuse. The data sharing page also
says the county does not guarantee accuracy. Sources:
[Data Sharing](https://mcassessor.maricopa.gov/page/data_sales/data_sharing),
[Commercial Purpose request form](https://api.mcassessor.maricopa.gov/file/data_sales/Commercial-Purpose-Public-Record-Request.pdf).
This is not legal advice, and I haven't read the API's own terms. Before showing it to prospects as something they could
use:

- Ask the Assessor, in writing, what the API token permits for a business using it for its own scheduled jobs.
- Read the API terms in the PDF and on the token-request page.
- Keep the product story to **one address tied to one job**. Do not demo, record or suggest bulk list-building from
  parcel data ("every pool in Gilbert built before 2000"), which is exactly the solicitation use the statute is
  aimed at.
- Keep the brief free of owner and mailing details, as in beat 4a.

## 8. Variants, same connector

| Variant | Routine | Outbound action | Note |
|---|---|---|---|
| **Home inspector** | "For tomorrow's inspections, pull the house age and size" | Email the inspector the brief | Same flow, different persona |
| **Real estate agent** | "Prep a listing brief for each seller I've signed" | Email the seller a one-page summary | Per-listing and client-linked, so lower risk; avoid "just sold" mailings to neighbors |
| **Property manager** | "Check the parcel details for my 12 doors against our records" | A note to the owner on mismatches | Valuation endpoints may not be live; verify |

## 9. Pass criteria

A viewer who hasn't seen tern can answer:

1. How did tern know about these houses?
2. Which address did it not look up, and why?
3. What did it send customers, and what did it keep out of the message?
4. What happened when it was killed?

If they miss 2, the Pima County line is too quiet. If they miss 3, the data-minimisation caption is too quick.
