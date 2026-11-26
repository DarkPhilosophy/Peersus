# Launch day D — hour-by-hour runbook

Purpose: the exact schedule and checklists for the first public day, assuming the §0 gate passed. Researched on 2026-11-12.
Times are owner-local (Europe/Bucharest, EET = UTC+2; marked PT where a platform's clock matters). If you launch in summer, PT offset shifts by one hour — recheck.

## 0. Gate — must all be true before anything in this file runs

- Release artifacts exist **and were run as applications**: Linux desktop program folder + Android community APK installed and exercised by a person ([go-no-go.md](../go-no-go.md) blocker 1).
- Public repository live; GitHub Release published with checksums; website download page live (see [website.md](website.md)).
- Demo video + screenshots ready ([assets-checklist.md](assets-checklist.md)).
- Owner decisions in [plan.md §11](plan.md) are resolved. If any is "no", the affected channel below is skipped, not improvised.

**Hard rule:** if the artifact cannot be installed and complete one real transfer by a stranger following the install page, do not launch. HN rules: "If your work isn't ready for users to try out, please don't do a Show HN." ([showhn.html](https://news.ycombinator.com/showhn.html)) The same standard applies to a regular submission.

## 1. The night before (D-1, ~21:00–22:00 local)

- [ ] Re-download the exact release artifacts to a clean machine/VM; run the install page instructions verbatim; do one real transfer.
- [ ] Verify all links in every post draft (posting-calendar.md) resolve: release page, download page, privacy page, repo.
- [ ] Prepare the reply doc: paste the crisis replies from [plan.md §8](plan.md) plus three short paragraphs: "what it is", "what's verified", "what's not done".
- [ ] Charge the Android test phone; keep it beside you for comment screenshots/questions.
- [ ] Sleep. Launches lost to a 03:00 fumble are not heroic.

## 2. Morning, launch window (all times local EET)

Product Hunt days run on Pacific Time from 12:01 AM PT ([PH launch guide](https://help.producthunt.com/en/articles/17734306-1-how-product-hunt-works)) — that is **10:01 EET in winter**. If PH is deferred per the recommendation, the PH line items below move to the later PH week and everything else keeps its times.

| Time | Action |
|---|---|
| 07:00 | Coffee. Open the release page in a private window; click every download link. Check the site on a phone. |
| 07:30 | Flip the repo to public (if it isn't already). Confirm README renders, hero image loads, LICENSE/NOTICE present. |
| 08:00 | **Release live:** publish the GitHub Release (Linux archive + Android community APK + SHA-256 sums + notes). Download page now links real files — verify in a private window. |
| 08:30 | **Mastodon + Bluesky + X post** (drafts in posting-calendar.md §W0). These are low-risk warm-ups that generate the first outside eyes and catch broken links. |
| 09:00 | **r/fossdroid post** (FOSS-friendly; draft in calendar). Answer early comments within minutes — first-hour thread velocity matters everywhere. |
| 10:00 | (If PH this day) PH launch goes live at 12:01 AM PT = 10:01 EET; post maker comment immediately; reply to every comment. |
| 10:30 | **Hacker News submission** — see §3 for which kind. Post at a weekday US-morning-adjacent hour; 10:30 EET is ~03:30 ET; better is **14:00–15:00 EET** (07:00–08:00 ET, ~lunch PT) so the US audience is awake when it needs its first organic votes. No friends upvoting — banned ([showhn.html](https://news.ycombinator.com/showhn.html), newsguidelines: "Don't solicit upvotes"). |
| 11:00 | Post in **r/selfhosted weekly "New Project Megathread"** ([example thread](https://www.reddit.com/r/selfhosted/comments/1xcw6eb/new_project_megathread_week_of_22_oct_2026/); modmail first per sub rules — see channels.md). Only if the megathread is open this week; otherwise move to next open one. |
| 11:15 | Comment in **r/androidapps "Self Promotion Megathread"** ([megathread](https://www.reddit.com/r/androidapps?f=flair_name%3A%22MEGATHREAD%22)); only GitHub/store links are pre-approved there (see channels.md). |
| 11:30 | **Press tips out** (OMG!Ubuntu tip form, 9to5Linux contact, LWN lwn@lwn.net, It's FOSS, Android Police contact — drafts in posting-calendar.md §W0-press; addresses in channels.md). One personalised line each, not a blast. |
| 12:00 | Lunch. Then nothing new posts — the rest of the day is **replies**. |

## 3. Which HN submission (owner decision — pick one)

- **Option A — regular submission (recommended):** title "Peersus: consent-first file transfer for Linux and Android (end-to-end encrypted, works over the internet)", URL = the repo or the site download page. Rationale: the biggest comparable launches were organic *regular* posts of the repo itself — LocalSend 923 pts ([HN item 47933208](https://news.ycombinator.com/item?id=47933208)), LocalSend 563 pts ([item 37938183](https://news.ycombinator.com/item?id=37938183)), Syncthing 623 pts ([item 7734114](https://news.ycombinator.com/item?id=7734114)). A personal, honest first comment ("I built this; here's what's verified and what isn't") performs better than the title doing selling.
- **Option B — Show HN:** only if a stranger can download, install and do a transfer in <5 minutes including pairing a second device they own. Otherwise it violates the spirit of the rules.
- **Option C — don't self-post:** let it be discovered. Safest reputationally; slowest. Do not ask anyone to post it for you — that is the same ring the rules ban.

**In the thread:** first comment = the honest paragraph (what it is, what's verified: Linux+Android real-device use, what's not: Windows/macOS, store, extensions catalog, nearby experiment). Answer every question with specifics from the engineering log. Never argue; HN guidance: respond to the strongest interpretation ([newsguidelines.html](https://news.ycombinator.com/newsguidelines.html)).

## 4. Afternoon and evening

| Time | Action |
|---|---|
| 13:00–18:00 | Reply rotation every 30–60 min: HN thread → Reddit → Mastodon/Bluesky → repo issues/Discussions. Correct false claims once, politely, with a link. Screenshot nothing private. |
| 15:00 | If HN is dead (normal — most submissions die in /newest): do not repost, do not delete-and-repost (banned, [newsguidelines](https://news.ycombinator.com/newsguidelines.html)). Put energy into the Reddit threads and Discord/Discussions replies. |
| 18:00 | **XDA thread** in Android Apps and Games (rules: include description, don't paste only a link, self-promotion clause — [XDA forum rules](https://xdaforums.com/t/xda-developers-forum-rules.4200559)). Evening CET catches the Android crowd. |
| 20:00 | Romanian/EU evening: r/Romania "Forum Liber" thread or r/programare if the owner is comfortable posting personally in Romanian (rules of the regulament apply; self-promo norm unconfirmed — verify on the day, see channels.md). |
| 22:00 | Close-out note to self: tally reply themes, file every bug report as an issue with repro fields, note the one question asked most — it becomes tomorrow's FAQ addition and a docs fix. |

## 5. What NOT to do on day D

- No Product Hunt + HN + 3 subreddits + press in the same 4 hours if you can't reply to all — stagger instead (PH recommended to week 4–6 anyway).
- No "please upvote" anywhere, to anyone, on any platform (banned on HN, PH, Reddit).
- No claiming the Play Store / F-Droid / Flatpak listing "is coming this week". The honest line: "Google Play submission requires disclosures we're preparing; community APK is available now."
- No editing the release assets after links are out — if a critical bug forces a re-spin, bump the version, publish new checksums, and post the correction visibly.
- No DM-ing journalists or influencers who didn't ask (unsolicited DMs are the fastest credibility loss).
- No posting in r/privacy (self-promotion banned — see channels.md) or r/androidapps main feed.

## 6. Crisis replies (copy-adapt; full table in plan.md §8)

- **"Why not LocalSend?"** → "LocalSend is good software and stays the right answer for same-network sending. Peersus exists for the other case: your own devices on different networks — direct encrypted path first, relays carrying ciphertext when there isn't one."
- **"Unaudited / why trust the crypto?"** → "Correct, nobody independent has audited it — the site says so plainly. Pairing is explicit, the receiver approves every file, relays see metadata not payloads. Use it for non-secret files until it earns more trust."
- **"Android battery drain?"** → "Measured and open: idle name-resolution costs are documented in the engineering log (E-020/E-021) and being fixed. If you see worse, I want the report."
- **"Is this AI-written?"** → "No — and the repo's engineering log is the receipt. Judge the code." (HN bans generated text; keeping everything human-written is both required and an advantage.)
- **"Shizuku = spyware?"** → "Optional, community flavour only, and off unless you install a provider. The Play flavour never uses it." ([store-policy doc](../store-policy-google-play.md))

## 7. Day D+1

- [ ] Publish a short "day one" note in the repo Discussions: what people asked, what's fixed already, what was learned. This converts launch traffic into a community habit.
- [ ] Triage every issue; reply to every unanswered thread comment (they stay discoverable for years).
- [ ] Update the site FAQ with the top 3 questions.
- [ ] If anything was wrong in a public post (a claim, a version number), correct it visibly where it was said — quiet edits read as cover-ups.
- [ ] Then stop refreshing stats. Week 2 plan is in posting-calendar.md.
