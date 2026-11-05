# Draft: note to the Maricopa County Assessor

Status: draft, 2026-11-04. Not sent. Send through the Assessor's contact form with the **"API Question/Token"** option.
Fill the bracketed parts. Keep your employer out of it unless the side-project check in
[launch-plan.md](launch-plan.md) §3 says otherwise.

---

**Subject:** API token request, and a question about permitted use

Hello,

I'm requesting an API token for the Assessor's API, and I'd like to check what use the token permits before I start.

**Who I am.** I'm an independent developer building tern, an open-source personal software agent. [Name, city, contact
details.] I'd use the token to build and test an integration for the Assessor's property lookup endpoints.

**What the integration would do.** A user of the software (for example, a small home-service business or a home
inspector) gives it a list of addresses for upcoming jobs. The software looks up each address and shows the user the
basic property details for that job, such as year built, living area, lot size and whether the record shows a pool.
It makes one lookup per address, for a job the user already has. It would not collect owner names or mailing addresses,
would not store parcel data beyond the user's own device, and would not build lists or run bulk queries by property
characteristic.

**My questions.**

1. Does this use count as a "commercial purpose" under Arizona law? If so, what do you need from me or from the
   end user (for example, the Commercial Purpose Public Record Request form), and is there a fee?
2. Is the token for one person or one organisation? Because the software is open source, each user would request
   their own token. I would not share mine or bundle one with the software. Is that the arrangement you'd expect?
3. Are there limits on request rate or volume I should design around?
4. Which endpoints are currently supported? The documentation I found is dated 2021, and I'd like to build only
   against fields that are live.
5. May I show results from the API in screen recordings and public demonstrations of the software, using public
   or consenting addresses? Is any attribution or disclaimer required?
6. Is there anything in the terms of use I should read beyond the API documentation PDF?

I'm glad to adjust the design if any of this isn't appropriate. Thank you for your time.

[Name]
[Email · phone]

---

## Notes for you, not for the Assessor

- **Don't send before you've read the API documentation PDF yourself.** Question 6 assumes you have; edit it if the
  PDF answers 1 to 5.
- **Keep the description exactly as true as the product.** If you later add owner fields or bulk queries, the answer you
  got no longer covers it.
- **Keep their reply.** Whatever they say about commercial purpose goes in the repo, next to the demo script, so a
  later launch claim can point at it.
- **Don't wait on it for the recording.** If the token arrives before the reply, you can test, but hold public
  filming until question 5 is answered.
