# Demo script: accounting firm

Status: draft, 2026-11-04. Internal. The detailed script for Flow A in [vertical-demo-flows.md](vertical-demo-flows.md).
Target: a 90-second recording, and the same beats run live on a call. Assumes T3 (outbox, deferred approvals), T4a and
plain-language capability screens are done.

## 1. Choose the tool first (research, 2026-11-04)

The connector moment needs a real practice-management API. What the public docs say:

| Tool | API | Auth | Catch | Verdict |
|---|---|---|---|---|
| **Karbon** | Yes. v3 at `api.karbonhq.com`; docs cover clients, work items, invoices and payments, custom fields, webhooks | **No OAuth.** Every request carries a Bearer GUID plus an `AccessKey` JWT, both issued from an API application in settings | v3 access is enabled on request and apps need registration approval; calls act for the whole account, not a user | **Use this.** Start the access request now; it is the long pole |
| **Canopy** | Yes, new external API platform announced 2026-09-30 | Not confirmed | Full API only on the Premium plan; accounts-receivable data is not available; CRM 1 accounts are ineligible | Backup. The missing AR data breaks an invoice chase |
| **TaxDome** | Unclear. One review says no public API; the terms mention one | Unknown | No public developer docs found | Skip until confirmed |

Sources: [Karbon developer docs](https://developers.karbonhq.com/guides/authentication/),
[Karbon API integration help](https://help.karbonhq.com/en/articles/4580888-karbon-api-integration),
[Canopy API considerations](https://support.getcanopy.com/en/articles/9376222-canopy-open-api-considerations),
[CPA Practice Advisor on Canopy's API](https://cpapracticeadvisor.com/2026/09/30/canopy-unveils-external-api-platform/190864).
These are secondary summaries of vendor pages; read the vendor docs before relying on any detail below.

**What this changes in the skeleton.** Beat 2 is not "Sign in to Karbon". It is the secret-request card, twice (the
bearer value and the access key), neither shown on screen after entry. That is a weaker consumer moment than OAuth, but
it is a better *trust* moment: the viewer sees tern ask for credentials without ever seeing them. Script it that way
and don't pretend it is a sign-in.

Because Karbon's calls are account-wide, the permission card must say so. See beat 3.

## 2. Preflight (do all of this before filming)

- [ ] Karbon developer account registered, v3 access approved, API application created in a **demo firm**, not a real one.
- [ ] Demo firm seeded by script (§3). Run the seed twice; the second run must change nothing.
- [ ] tern built from the branch with T3 and T4a; fresh `~/.tern` on a clean user so no earlier connector exists.
- [ ] Phone paired, Telegram approval cards visible, notifications on, screen recording on.
- [ ] Email sending set up to a **sink address you control**. No demo email goes to a real person.
- [ ] A dry run of the whole script, with the `kill -9` beat, three times in a row without a change.

## 3. Seed data

Fourteen clients in the demo firm. Fictional names, none resembling real firms.

| Client | State | Why it is in the demo |
|---|---|---|
| Harbor Lane Bakery | Invoice 47 days overdue | Plain case; draft quotes amount and days |
| Cedar & Pine Landscaping | Invoice 34 days overdue | Plain case |
| Marlowe Dental Group | Invoice 61 days overdue | The big number; draft stays polite |
| Ridgeview Property Mgmt | Missing bank statements task, 9 days past due | Document chase, not invoice |
| Pell Street Consulting | Missing bank statements task, 4 days past due | Document chase |
| **Okafor Holdings** | Invoice 52 days overdue, **note: "Do not email. Call Dana."** | The trap. tern must skip and say why |
| Eight other clients | Paid up, nothing due | Proves it doesn't spam everyone |

Five reminders get drafted, one client is skipped. The skip must come from the **note on the client**, read at run time.
Do not hard-code the name anywhere in the prompt.

## 4. The shot list

Times are for the 90-second cut. On a live call, the same beats run longer and beat 1–3 run on the prospect's tool.

### Beat 1: name the tool (0:00–0:08)

- **Screen:** an empty tern conversation, desktop app, phone beside it.
- **User types:** `Connect to Karbon.`
- **tern does:** says it will read Karbon's API docs; the screen shows a short list of what it is doing ("reading the API
  guide", "drafting a connection", "testing it with a harmless request"). No code is shown.
- **Caption:** *No Karbon connector existed a minute ago.*

### Beat 2: credentials, without seeing them (0:08–0:22)

- **tern does:** "Karbon needs two keys from your API settings. I can't see them; paste them here." A secret card opens
  with two fields. The user pastes both. The card closes and shows only `Saved. tern can use these, not read them.`
- **Test step:** tern makes one read-only call. It shows "Connected: found 14 clients."
- **Caption:** *The model never sees the keys.*
- **If the access key is wrong:** do not cut around it. Leave one take in the recording where tern says the key was
  rejected and asks again. It is the most believable 5 seconds in the video.

### Beat 3: the permission card (0:22–0:32)

Plain language, one screen, no hosts or scopes jargon:

> **tern wants to connect to Karbon**
> It will be able to: **read** your clients, work items, invoices and notes.
> It cannot: send anything, change anything, or delete anything.
> It talks only to: `api.karbonhq.com`.
> *Karbon's keys give tern access to the whole firm, not one person. tern only uses what is listed above.*

- **User taps:** Approve.
- **The last line is the honest one.** Karbon's key is account-wide, so the card says so. Do not cut it for
  tidiness; a viewer who later learns the key was broader than the card implied will not trust anything else.
- **Caption:** *Read-only. Writes are a separate approval.*

### Beat 4a: the routine (0:32–0:42)

- **User types:** `Every Monday at 8, find clients with invoices over 30 days overdue or missing documents past due. Draft a polite reminder for each, in our usual tone. Skip anyone we've said not to email.`
- **tern does:** creates the routine and says so in one line: "Done. Every Monday at 8. Run it now?"
- **User:** `Yes.`

### Beat 4b: the run (0:42–0:58)

- **Screen:** a short activity list: "Checked 14 clients. 6 need follow-up. 1 skipped (see below). 5 drafts ready."
- **tern says:** "I skipped **Okafor Holdings**: the client note says *Do not email. Call Dana.* Want me to add it to your call list instead?" (Shows the skip with the reason, from live data.)
- **Phone:** five approval cards arrive, each with client, amount or document, and the draft text.
  - Approve two as drafted. Edit one (change a sentence on screen). Deny one.
  - **While two cards are still waiting**, the activity list keeps moving and tern writes the partner summary. This is
    the deferred-approvals moment: show the summary appearing while the cards sit unanswered.
- **Caption:** *Waiting on you. The rest of the job keeps going.*

### Beat 5: break it, then the receipt (0:58–1:30)

- **Terminal split-screen:** run `kill -9` on the daemon while the third send is in flight. Show the process die.
- **Restart:** `tern start`. The run resumes. The activity list marks the earlier steps as picked up, not redone.
- **Receipt:** a plain list.

  > Sent 3 · Denied 1 · Skipped 1 (client note) · Duplicates 0
  > Harbor Lane Bakery: reminder sent 8:02, approved by you, step 4
  > …

- **Proof of no duplicates, off screen from tern:** switch to the email sink and show exactly three messages. Do not
  rely on tern's own log for this. A viewer trusts the downstream system.
- **Closing line (narration or end card):** `It chased five clients before the coffee was ready, and it didn't email the one you told it to call.`

## 5. If something goes wrong on camera

| Failure | What to do |
|---|---|
| Connector draft fails the test call | Keep it in. Show tern reading the error, adjusting, retrying. If it takes more than 20 seconds, cut the demo and pre-build, but say "built earlier" in the caption |
| Karbon rate limit | Seed fewer clients, or cache one list call. Never fake the response |
| Draft tone is off | Edit it on screen; it doubles as the edit-before-approve moment |
| Approval card delayed on the phone | Re-record. A late card in beat 4b breaks the deferred-approvals point |
| `kill -9` lands after all sends finish | Move it earlier; the run must be mid-send for the replay to show |

## 6. What this demo must not claim

- **No sign-in button for Karbon.** Credentials go through the secret card.
- **No writes to Karbon.** Don't mark work items done, change a status or post a note back, even though the API may
  allow it. Reminders go by email only.
- **No "works with every practice tool".** The claim is: tern wrote a connector for *this* tool from its API docs.
- **Karbon approval is not guaranteed.** If v3 access is delayed, do not substitute a mock and call it Karbon.

## 7. Pass criteria

A person who has not seen tern, shown the cut once, can answer all four:

1. Which clients were contacted, and which one was not and why?
2. Could tern change anything in Karbon? (Correct: no.)
3. What happened when the process was killed?
4. How would you check for duplicates? (Correct: look in the inbox, not in tern.)

If a viewer misses 2, rewrite the permission card. If they miss 3, the resume beat is too fast.

## 8. The live-call version

1. Ask the prospect, in advance, which practice tool they use and whether they can create API keys.
2. If it is Karbon, run beats 1–3 against their real account, read-only. Stop after "Connected: found N clients" and
   ask whether N is right. That moment is the demo.
3. If it is another tool, read its API docs first and decide before the call whether to try. A failure live is a
   result: log the tool and the specific reason.
4. Run beats 4a–5 on the seeded demo firm. Never send mail to a real client from their account on a first call.
