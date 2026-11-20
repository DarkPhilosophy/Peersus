# Six-week posting calendar with ready-to-paste drafts

Purpose: a dated plan relative to launch day D, per channel, with draft texts the owner can paste after inserting real URLs. Researched on 2026-11-12.
Replace every `URL` with the real link and check it resolves before posting. Every draft below only claims what the repo docs verify; limits are stated on purpose — they are the brand. Where a channel's rule requires something (megathread-only, no APK links), the draft already respects it; see [channels.md](channels.md).

## Week 0 — launch week (D–D+6)

| Day | Channel | Action |
|---|---|---|
| D-1 | (internal) | Rehearse install on a clean machine; final link check. |
| D 08:30 | Mastodon/Bluesky/X | Post §1a. |
| D 09:00 | r/fossdroid | Post §1b. |
| D 11:00 | r/selfhosted megathread | Comment §1c (only in the weekly New Project Megathread). |
| D 11:15 | r/androidapps megathread | Comment §1d (link GitHub, not the APK). |
| D 11:30 | Press tips | Send §1e variants to LWN, OMG!Ubuntu, 9to5Linux, It's FOSS, Android Police. |
| D 14:00–15:00 | Hacker News | Submit §1f + first comment §1g. |
| D 18:00 | XDA Forums | Post §1h. |
| D 20:00 | r/Romania Forum Liber / r/programare | Post §1i (Romanian, personal voice) — owner decision. |
| D+1 | GitHub Discussions | Post §1j day-one note. |
| D+2–6 | All | Replies only; no new posts except fixing real questions. |

### 1a. Mastodon/Bluesky/X post (also fits LinkedIn)

```
Peersus is out (pre-release): consent-first file transfer between your own
devices — Linux and Android today. Direct end-to-end encrypted connection
first; when there's no direct route, configured relays carry ciphertext
(they can see connection metadata, not the files).

No account, no ads, no tracking, no telemetry. Apache-2.0.
Receiver approves every file before anything is written.

Honest limits: pre-release, Linux + Android tested, Windows/macOS not
supported, extension store still being set up.

URL
```

### 1b. r/fossdroid post

**Title:** `[DEV] Peersus — consent-first file transfer between your devices (Linux + Android, works over the internet too, Apache-2.0, pre-release)`

```
Hi r/fossdroid — I built Peersus, a file-sending app for your own devices.
What it does:

- Pair two devices deliberately (a shared confirmation), then send files;
  the receiver approves each file before it is written.
- Connections are end-to-end encrypted and direct-first. When a direct
  route doesn't exist, configured relays carry the same ciphertext —
  they see connection metadata, not file contents. Relays can be turned
  off or re-ordered in settings.
- Works between networks, not only on the same LAN. That's the gap I
  built it for.
- No account, no ads, no tracking, no telemetry. Apache-2.0.
- Extension system: every extension asks for named grants, carries its
  licence+notice up front, and the app verifies a signed catalog before
  installing anything. (Catalog is still being populated — this is a
  foundation, not a selling point yet.)

Honest limits: it's a pre-release. Linux and Android are the tested
platforms; Windows and macOS are not supported. The community-flavour APK
is on the release page; Google Play submission needs disclosures I'm
still preparing (the app has an accessibility-service-based remote-input
feature, which Play treats seriously — details in the repo's store-policy
doc). An experimental, off-by-default BLE "devices nearby" mode exists and
is labelled exactly that.

Repo + releases: URL
I'd value precise feedback: install failures, pairing confusion, or
consent wording that wasn't clear.
```

### 1c. r/selfhosted "New Project Megathread" comment

```
**Peersus** — consent-first file transfer between your own devices
(Linux + Android, Apache-2.0, pre-release). Pair devices once, receiver
approves every file, end-to-end encrypted direct-first connections with
optional relays carrying ciphertext when there's no direct route — so it
works between networks, not just the LAN. No account, no tracking.
Not a server app — but it fits the "my own infrastructure, no cloud
middleman" ethos. Relay can be disabled; a custom relay URL can be
configured.
Limits: pre-release; Windows/macOS unsupported; Google Play pending
disclosures.
URL (GitHub repo + release APK)
```

### 1d. r/androidapps "Self Promotion Megathread" comment

```
Peersus — consent-first file transfer between your own devices. Linux +
Android today (Windows/macOS unsupported). End-to-end encrypted,
direct-first, works over the internet via optional relays that only see
metadata. No account/ads/trackers; receiver approves every file.
Apache-2.0, pre-release. Source + APK on GitHub: URL
```

### 1e. Press tip email (per outlet, personalize the first line)

```
Subject: New open-source file-transfer app — Linux + Android, works over
the internet, pre-release

Hi <name>,

I think this fits <outlet>'s coverage of Linux/Android FOSS. Peersus is a
new Apache-2.0 file-transfer app I've built: deliberate pairing between
your own devices, the receiver approves every file, and transfers are
end-to-end encrypted — direct connection first, with optional relays that
carry ciphertext (they see connection metadata, not file contents) when no
direct route exists. So a paired, remembered device is reachable whether
it's on your LAN or on another network — without an account or cloud drive.

It's a pre-release: Linux and Android are tested; Windows and macOS are
explicitly not supported, and that's stated plainly on the site rather
than oversold. Happy to answer anything or point you at the engineering
log (which keeps a dated record of what's verified and what isn't).

Site: URL · Repo/release: URL · Short demo video: URL (if ready)

Thanks for your time — <name>
```

### 1f. HN submission

- **Title:** `Peersus: consent-first file transfer for Linux and Android, works over the internet`
- **URL:** the repo URL (per HN evidence, the repo itself is the strongest link for this category) — or the site download page if the owner prefers. No `utm_` parameters (they get stripped on Lobsters and look spammy everywhere).

### 1g. HN first comment (post immediately after submitting)

```
I built Peersus. What it is: consent-first file transfer between your own
devices — deliberate pairing, receiver approves every file, end-to-end
encrypted QUIC, direct path first and configured relays carrying
ciphertext when there isn't one, so it works across networks rather than
only LAN. No account/ads/trackers; Apache-2.0; Rust.

What's verified: real-device use on Linux + Android (the repo has a dated
engineering log with measurements and honest failures).
What's not: Windows/macOS are not supported; the extension-store signing
setup and most extensions are unfinished; a BLE "devices nearby" mode is
experimental and off by default; nobody has audited it.

Happy to answer anything — I'd most like feedback on pairing/consent
clarity and install friction.
```

### 1h. XDA thread (Android Apps & Games)

```
[APP][FREE][FOSS] Peersus — consent-first file transfer between your own
devices (pre-release)

Peersus pairs your Android phone with your Linux machine (or another
phone) deliberately — no accounts, no ads, no tracking, Apache-2.0.
The receiver approves every file before anything is written.
Transfers are end-to-end encrypted: direct connection first, optional
relays (ciphertext only — they see metadata, not contents) when there's
no direct route, so it also works between networks, not just the same
Wi-Fi.

The honest part: this is a pre-release. Tested on real devices, but
Windows/macOS aren't supported, the extension store is still being set
up, and there's an experimental off-by-default BLE "devices nearby" mode.
There's an accessibility-service-based remote-input feature — it's what
keeps this on a community APK for now while Play disclosures are prepared.
Community APK + source: URL (GitHub)
Feedback wanted: install issues, pairing confusion, consent wording.
```

### 1i. Romanian personal post (owner's voice — adjust freely)

```
Salut — am construit Peersus, o aplicație open-source (Apache-2.0) pentru
transfer de fișiere între dispozitivele tale: Linux și Android, pre-release.
Fără cont, fără reclame, fără tracking. Destinatarul aprobă fiecare fișier,
conexiunile sunt criptate cap-coadă și funcționează și între rețele
diferite (relee opționale care văd doar metadate, nu conținutul).
Limitări spuse direct: Windows/macOS nu sunt suportate, e pre-release.
Cod + APK: URL
Orice feedback — mai ales instalare și pairing — e binevenit.
```

### 1j. GitHub Discussions day-one note

```
Title: Day one — what people asked and what's next

Body: <the 3 most-asked questions, each with a straight answer and a link
to the doc/issue that covers it>. Repro template reminder. "What would
make you trust this enough to use weekly?" as the one open question.
```

## Week 2 — consolidation (D+7–D+13)

| Day | Channel | Action |
|---|---|---|
| D+7 | Repo | Publish "week one" changelog/discussion post listing fixed issues w/ evidence links. |
| D+8 | Blog/site | Technical post draft begins (see below); not promotional. |
| D+9 | r/selfhosted megathread | Only if last week's comment got questions; otherwise skip — no double-dipping. |
| D+10 | Press | Second tip wave ONLY to outlets that didn't get one; never re-tip. |
| D+11 | Mastodon/Bluesky | One screenshot post: "what week-1 feedback changed" — proof over hype. |
| D+12 | GitHub | Ensure issues have repro fields; add `good first issue` where real. |

**Week-2 technical post idea (for site/blog → then submit to TWiR-adjacent places):** "Designing per-extension grants: how Peersus renders an extension's licence+notice contract before install." TWiR no longer takes bare project PRs but takes human-written walkthroughs ([README](https://github.com/rust-lang/this-week-in-rust)); r/rust and Lobsters tolerate exactly this kind of post.

## Week 3 — distribution groundwork (D+14–D+20)

- Start Google Play closed test if $25 fee + disclosures approved: recruit ≥12 testers from early adopters; 14-day clock starts ([Play rule](https://support.google.com/googleplay/android-developer/answer/14151465)).
- Build a real AppImage → submit `data/Peersus` PR to [appimage.github.io](https://github.com/AppImage/appimage.github.io) (their test runs it screenshot-less on oldest Ubuntu LTS, offline).
- Draft Flatpak manifest **by hand** (Flathub bans AI-authored manifests — [requirements](https://docs.flathub.org/docs/for-app-authors/requirements)); open a pre-submission question in their Matrix room.
- F-Droid feasibility notes (build-from-source with Skia/wasmi; extension-download consent wording vs [inclusion policy](https://f-droid.org/en/docs/Inclusion_Policy/) item 5; relay Anti-Feature). Record outcome in the repo.
- No new promo posts except substantive replies and the technical article.

## Week 4 — second wave (D+21–D+27)

| Channel | Action |
|---|---|
| r/linux | One post **after modmail OK**: release + honest limits + link (original source, original title norm). |
| r/opensource | One substantive post (what's verified/what isn't) if week-0 traffic justified it. |
| Lobsters | Only if account ≥70 days and <25% self-promo ([rules](https://lobste.rs/about)) — submit the technical article, tags `rust` `show`. |
| This Week in Rust | PR the technical article link into the draft (walkthrough category). |
| Mastodon/Bluesky | Milestone post only if real (N testers, first outside contributor, first store extension). |

## Week 5 — Product Hunt (if owner approved; otherwise week 6)

- Schedule launch 12:01 AM PT on a **Tuesday–Thursday** (norm; unconfirmed preference, verify). Prepare: thumbnail 240×240, gallery images (1270×760 norm — verify current spec on the [launch guide](https://www.producthunt.com/launch)), demo video, maker comment, tagline 60 chars, first-comment FAQ. All-day replies (PH runs on the maker's presence).
- Same day: no other big pushes — PH is a full-day job.

## Week 6 — durability (D+35–D+42)

- Evaluate F-Droid decision outcome; submit or close.
- If a Flathub-ready Flatpak exists and manifest is human-written: submit.
- First "state of the project" post: numbers that are true (download counts, issues fixed, contributors), roadmap honesty.
- Set the ongoing cadence: one substantive post per month max per channel; every release note on the repo; quarterly technical article.

## Standing rules every week

- Never cross-post identical text the same day to two subreddits; rewrite per community.
- Never post where self-promo is banned (channels.md ❌ list).
- Every claim in every draft traces to a repo doc — if a feature shipped differently since this was written, fix the draft before pasting.
- Log every post in a scratch file: URL, date, response — week-6 review needs it.
