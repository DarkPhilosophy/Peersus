# Peersus launch and marketing plan (master document)

Purpose: one evidence-based plan a single person can execute for the first public release. Researched on 2026-11-12.
Companion files: [launch-day.md](launch-day.md), [posting-calendar.md](posting-calendar.md), [website.md](website.md), [channels.md](channels.md), [assets-checklist.md](assets-checklist.md).

## 1. Where we actually stand

[docs/go-no-go.md](../go-no-go.md) (evaluated 2026-11-11) says **No-Go for a public release on every platform**, and **Go for continued private development builds on Linux and Android**. Nothing in this plan overrides that. Every posting below is conditional on the gate in §6; the calendar is written relative to a launch day D that is only *proposed*, never promised.

What is true today, per the repo docs:

- Linux and Android run in development builds and have device-level evidence ([engineering log](../engineering-log.md)); optimized release artifacts were built as files but **never run as applications** (blocker 1). No Android release package, Flatpak or AppImage exists.
- Windows and macOS are **not supported**: two Windows interface tests fail and no window ever opened; macOS builds pass in a guest but the program aborts without a Metal device ([go-no-go.md](../go-no-go.md) blocker 11, [macos-evaluation.md](../macos-evaluation.md)). Marketing must say "not supported", not "planned".
- Production trust is not configured (blocker 2): the extension store and the application updater work end to end **with development keys only**. A product build reports the store as "not set up" ([extension-store.md](../extension-store.md), [app-update.md](../app-update.md)).
- "Devices nearby" (BLE) is experimental, off by default, and shipping it at all is an owner decision (blocker 14, [nearby-send.md](../nearby-send.md)). Never a headline feature.
- Store submissions are unstarted: Google Play needs disclosures/consent/declarations for the accessibility service plus a published privacy policy; Apple's Mac App Store cannot carry native extensions or a self-updater, and the Ko-fi button risks guideline 3.1.1 ([store-policy-google-play.md](../store-policy-google-play.md), [store-policy-apple.md](../store-policy-apple.md)). Whether Google accepts WASM as an interpreter is unconfirmed.
- Known open measurements that trolls and reviewers will find in the log anyway: Android idle CPU spent on name resolution (E-020/E-021), control-cost overhead (E-019/E-023), extension rate-limiting not built (A-04). Say them before someone else does.

## 2. Positioning

Peersus is **not** "another AirDrop clone". The honest differentiator set:

1. **Send to remembered devices across the room *and* across the internet.** Transfers are direct-first end-to-end encrypted QUIC; when no direct route exists the same encrypted traffic can ride configured relays that see connection metadata, not file contents ([architecture.md](../architecture.md)). Competitor check (verified today): LocalSend is explicitly LAN-only — its own FAQ answers "Does it work over the internet? No." ([localsend.org](https://localsend.org/)); PairDrop *can* send over the internet but via temporary public rooms and browser pairing ([PairDrop README](https://github.com/schlagmichdoch/PairDrop)). The honest gap Peersus fills: **a native app where the same paired, remembered device is reachable whether it sits on your LAN or another network** — persistent pairing, per-file consent, no room codes. This is a design claim, not a benchmark: never say "faster" or "more reliable" without numbers.
2. **Consent before receive.** Nothing is written to disk before the receiving person accepts each file; pairing is a deliberate ceremony; devices that paired stay paired ([architecture.md](../architecture.md), [nearby-send.md](../nearby-send.md)).
3. **No product account, no ads, no tracking, no telemetry.** Apache-2.0 source ([README.md](../../README.md)). This is verifiable in the repo — keep it verifiable; it is the whole pitch to the privacy audience.
4. **Extensions that ask permission, checked by a signature.** The store verifies a maintainer signature and per-plugin signed approvals; every extension carries a licence+notice agreement the application renders itself, and outside-store packages get a scroll-to-end warning before install ([extension-store.md](../extension-store.md)). Honest framing: "an extension system built around grants and agreements" — **not** "a store full of plugins", because today the catalog is effectively empty and the first-party ones are unproven end-to-end.

The full competitor picture lives in the sibling files [competitors.md](competitors.md) and [competitors-matrix.md](competitors-matrix.md). One fact from them must shape every message: **AlterSend shipped Peersus's exact thesis first** (May 2026, all five platforms, browser receive) — so "works over the internet" alone is *not* a differentiator. The defensible wedge is the trust architecture: durable authenticated pairing, per-file consent, honest relay disclosure, and a signed extension model — said without disparaging anyone.

### One-line pitch candidates (owner picks one; all use only verified claims)

- **A:** "Peersus sends files between your devices — across the room or across the internet — with the receiver's yes before anything lands. No account, no ads, no tracking. Apache-2.0."
- **B:** "AirDrop-style sending for Linux and Android that also works over the internet, end-to-end encrypted, and asks the receiver first. Free software."
- **C:** "Your files. Your other device. Pair once, approve what arrives, and let relays carry ciphertext — never the payload — when a direct route fails."
- **D (technical, for HN/Lobsters):** "A consent-first file-transfer workspace in Rust: direct-first QUIC with optional relays, per-extension grants, and a signed catalog."

Avoid in any final copy: "secure", "military-grade", "private by design" as an absolute, "faster than", "all platforms", "the LocalSend killer", any store/installer claim before it exists, "background clipboard sync", "resume support" (reconnecting is not resume, [architecture.md](../architecture.md)), and "self-hosted" — the relay server itself is not part of this workspace, so "run your own relay" is only partially true (a custom relay *URL* can be configured; do not promise a server binary).

## 3. Audiences

| Audience | Where | Lead message | Proof required |
|---|---|---|---|
| Linux users who move files to/from an Android phone | r/linux (rules-permitting), Mastodon/Bluesky, OMG!Ubuntu, 9to5Linux, YouTube | "One tool, both directions, no account; receiver approves every file." | The 60–90 s demo video (assets-checklist) |
| FOSS-on-Android users | r/fossdroid, later F-Droid | "Open source, APK from the release page; store flavours differ — say why." | Signed APK + checksums |
| Privacy-conscious users | r/fossdroid, Mastodon, privacy press; **not** r/privacy (self-promo banned) | "End-to-end encrypted, relays see metadata not payload, no telemetry — and nobody independent has audited it yet." | privacy.html + the honest disclaimer |
| Rust/open-source contributors | Lobsters (later), r/rust (case-by-case), TWiR (walkthrough only) | "Interesting architecture: one owner, typed contracts, WASM+native extension isolation." | architecture.md + a technical blog post |
| Over-internet senders (the differentiator audience) | HN, r/selfhosted megathread | "Send to your own devices on another network without a cloud drive." | Relay honesty paragraph |

## 4. Messaging pillars → the words that may be used

Each pillar maps to a repo doc. If a claim is not in the doc, it does not go in the post.

1. **Consent-first transfers** — receiver accepts every file; pairing is deliberate and persistent. ([architecture.md](../architecture.md))
2. **Encrypted end to end, honest about relays** — direct first; relays carry ciphertext and see connection metadata; relays can be turned off. ([architecture.md](../architecture.md))
3. **No account, ads, trackers or telemetry — Apache-2.0.** ([README.md](../../README.md))
4. **Extensions with real boundaries** — per-extension grants shown in plain words, signed catalog, licence+notice agreement before install, outside-store warning. ([extension-store.md](../extension-store.md))
5. **Early and honest** — tested on Linux and Android; pre-release; Windows/macOS unsupported. This is a *credibility* pillar, not an apology: every post names the limits unprompted.

### Never claim (with the doc that forbids it)

- Working installers/stores on any platform until artifacts exist and run (go-no-go 1, 15).
- "Audited", "secure", "can't lose files" — the site's own disclaimer says the opposite (site/index.html disclaimer; go-no-go).
- Nearby-send as a feature: it is experimental and off by default (go-no-go 14); if asked, say "an opt-in experiment we are still validating".
- Extension store as shipped functionality on launch builds until the production key exists (go-no-go 2).
- Anything about remote device control beyond "experimental, accessibility-permission feature under evaluation" — it is the single biggest Google Play risk (store-policy-google-play.md B1).
- Comparison claims: do not rank against LocalSend/PairDrop/Syncthing on speed, security or reliability. LocalSend is Apache-2.0 and well loved ([github.com/localsend/localsend](https://github.com/localsend/localsend)); disparaging it buys nothing.
- "Self-updating" on Play or Apple builds: the Play flavour defers to Google Play and macOS self-update is not implemented ([app-update.md](../app-update.md), store policies).

## 5. Launch sequencing

Day D is the day a **public, runnable artifact + public repository** exist. Sequence:

- **Gate (go/no-go rerun):** release artifacts built *and run as applications* on Linux and Android; signing keys decided (owner); privacy page published; site download page live; demo video recorded. Until then, **all public posting stays at zero** — Show HN rules explicitly forbid landing pages without a usable thing ([showhn.html](https://news.ycombinator.com/showhn.html)).
- **Phase 1 — quiet launch (D to D+2):** repository public, GitHub Release (Linux archive + community-flavour APK + checksums + release notes), website download page live. Posts: r/fossdroid, Mastodon/Bluesky/X, GitHub release notes. Purpose: shake out install failures before the big threads.
- **Phase 2 — amplification (D+3 to D+7):** regular HN submission (not Show HN unless the artifact is trivially runnable by a stranger — see launch-day.md), r/selfhosted weekly megathread (modmail first), r/androidapps self-promo megathread, press tips (OMG!Ubuntu, 9to5Linux, LWN, It's FOSS), XDA thread, Romanian communities.
- **Phase 3 — distribution & stores (weeks 2–6):** Play closed test (12 testers × 14 days rule for new personal accounts, [Play Help](https://support.google.com/googleplay/android-developer/answer/14151465)), AppImageHub once a real AppImage exists ([submission rules](https://github.com/AppImage/appimage.github.io)), Flathub once an AppImage/Flatpak build exists *and* the manifest is written by a human — Flathub bans AI-authored manifests and requires meaningful development history ([docs.flathub.org requirements](https://docs.flathub.org/docs/for-app-authors/requirements)); F-Droid is an owner **research task** (Rust/Skia/wasmi build reproducibility; whether the store's GitHub-Release downloads and the relay default trip Anti-Feature flags such as NonFreeNet — [inclusion policy](https://f-droid.org/en/docs/Inclusion_Policy/)), Product Hunt once there is polish + a video (PH is a *product* audience; free FOSS tools land mixed).
- **Windows/macOS:** publish "not supported" everywhere a platform list appears. No "coming soon" promises; the only honest sentence is "not a candidate for a first release" until go-no-go 11 changes.

## 6. Community & support plan

- **One community channel only.** Recommendation: **GitHub Discussions** on the public repo (zero new infra, searchable, owned by the project) over Discord/Matrix — Discord adds a moderation surface and Matrix hosting adds an identity problem on day one. Owner decision §13.
- **Feedback loop:** every bug report template asks for build number, platform, and a reproducible step list (mirrors [qa-strategy.md](../qa-strategy.md)); every fix lands in the engineering log with evidence. Weekly "fixed/reproduced/can't reproduce" triage posted in Discussions.
- **Support/tips:** Ko-fi stays as the *site's* voluntary-tip page (0% on one-time tips per [Ko-fi help](https://help.ko-fi.com/hc/en-us/articles/360002506494-Does-Ko-fi-take-a-fee) — note the pricing page lists a 5% service fee for non-Gold on some features; quote only "tips", not "free"), but **inside store builds it is a policy risk** ([store-policy-apple.md](../store-policy-apple.md) 3.1.1; [docs/support.md](../support.md) notes Play compatibility is unverified). Owner decision.
- **Support tone:** follows [docs/support.md](../support.md) — no nag screens, no ranking, acknowledgements opt-in only.

## 7. Metrics to watch (no analytics anywhere — see website.md §7)

The site must stay analytics-free; measurement is opt-in and coarse:

- **GitHub:** release asset download counts (public API), stars/day, issue+Discussion volume, first-time contributors. Realistic day-1–7 band for a niche Linux/Android tool with no audience: tens-to-low-hundreds of stars if the HN post reaches the front page; single digits otherwise. Comparable: the biggest LocalSend HN thread (Apr 2026, third-party post) reached ~923 pts / 275 comments ([HN Algolia record](http://hn.algolia.com/api/v1/search?query=LocalSend&tags=story)); that is the ceiling story, not the median.
- **Qualitative (the real target):** ≥ 20 distinct pieces of actionable feedback in week 1 (install failures, pairing confusion, consent clarity); every one triaged within 48 h.
- **Store/listing:** Play closed-test opt-ins ≥ 12 (the hard floor for a new personal account), Flathub/AppImageHub listing acceptance.
- **Failure metrics:** install-failure rate in feedback, "couldn't pair" reports, APK download → no-issue silence (unmeasurable — say so).
- Do not buy stars/upvotes: PH bans paid upvotes explicitly ([PH launch guide](https://help.producthunt.com/en/articles/17734306-1-how-product-hunt-works)); GitHub trending is star-velocity based ([community discussion](https://github.com/orgs/community/discussions/163970)) and farmed stars get noticed.

## 8. Risks and crisis replies (pre-written, keep in launch-day.md §6)

| Risk | Prepared reply gist |
|---|---|
| "Why not just use LocalSend?" | "Use it — it's good. Peersus exists for the case it doesn't cover: sending to your own devices over the internet, consent-first, without an account." |
| "Unaudited crypto, why should I trust it?" | "You shouldn't, yet. The disclaimer says so, the relay paragraph says what a relay can see, and independent review is on the roadmap — that honesty is the point." |
| "AI slop" | All public text is human-written (HN explicitly bans generated text, [newsguidelines.html](https://news.ycombinator.com/newsguidelines.html)); Flathub even bans AI-authored manifests. Answer once, point at the engineering log, move on. |
| Security researcher posts a real bug | Take it to the SECURITY.md channel, confirm fast, do not litigate in public; fix before marketing resumes. |
| "This is Snapdrop/PairDrop again" | "PairDrop does internet transfers via temporary rooms and browser pairing; Peersus is a native app where a *remembered, paired device* is reachable whether it's on your LAN or another network — no room codes, plus per-file consent and an extension model." |
| "AlterSend already does exactly this" | True — they shipped first and on more platforms ([competitors.md](competitors.md)). The differences that remain: durable authenticated pairing, per-file consent rigor, a signed extension model, and a public engineering log. If their product serves you better, use it. |
| "I can't send to someone who doesn't have the app" | Correct — the receiver needs Peersus (files-code still requires the app). That's a deliberate consent boundary, not an oversight; browser-receive tools trade it away. |
| Shizuku/community APK fear | "Optional community flavour; the Play flavour works without it; it's how the app self-updates on Android per docs/app-update.md." |
| Play/Apple rejection | Expected; say what was rejected, what changes; never rant publicly ([Lobsters brigading rule](https://lobste.rs/about) as a norm). |
| Android battery/CPU complaints (log E-020/E-021 is public) | "Measured and open; the fix is in progress; here's the log entry." Do not deny. |
| Ko-fi "donation scam" accusation | "Voluntary tips unlock nothing; the app is Apache-2.0; link is external and off by default in store builds." |

## 9. Budget

- **$0 baseline (recommended):** GitHub hosting+releases+Pages, Ko-fi free tier, all community channels free, press tips are free, AppImageHub/awesome-selfhosted listings are PRs. Play publishing deferred until the $25 one-time developer fee is approved ([Play Console sign-up](https://support.google.com/googleplay/android-developer/answer/6112435)); Apple $99/yr is **not** spent at all (macOS unsupported, [Apple Developer Program](https://developer.apple.com/programs/)).
- **Small optionals (owner decision):** domain renewal for the site host; a Product Hunt launch costs $0; do **not** buy ads, upvotes, stars, or a "featured" slot.

## 10. Pre-launch checklist (condensed; per-day detail in posting-calendar.md)

1. Gate: release artifacts run as applications on Linux + Android (go-no-go 1).
2. Owner decisions in §11 resolved.
3. Repo public; release notes written; checksums published.
4. Website: download page + OG image + privacy page final (website.md).
5. Assets: demo video + screenshot set + press kit folder (assets-checklist.md).
6. Accounts warmed where required: Lobsters needs 70 days for self-promo ([about](https://lobste.rs/about)); PH personal account; Reddit account with real history.
7. Drafts ready for every channel in posting-calendar.md.

## 11. Decisions for the owner

1. **Launch scope:** confirm Linux + Android community APK as day-1 scope; confirm "not supported" wording for Windows/macOS.
2. **Signing & trust:** produce the maintainer + update production keys (go-no-go 2) or launch without store/self-update enabled.
3. **Devices nearby:** ship behind the off-by-default switch, or exclude from the release build (go-no-go 14).
4. **Remote control / accessibility service:** keep on Play (disclosure+declaration) vs community-only (store-policy-google-play.md B1).
5. **Ko-fi:** keep in app About on non-store builds; decide for Play flavour; keep on website.
6. **Relays default:** project relay + public n0 pool enabled by default is a privacy-relevant choice — confirm it is what the launch copy says.
7. **HN strategy:** regular submission vs Show HN vs waiting for a community member to post (see launch-day.md §3).
8. **Community home:** GitHub Discussions vs Discord vs Matrix.
9. **Store budgets:** approve $25 Play fee; defer Apple; decide domain spend.
10. **Product Hunt:** launch day-one or week 4–6 (recommended: later, when a real demo video and polish exist).
11. **Press embargo:** none vs coordinated same-day tips (recommended: none — too small to embargo).
12. **F-Droid:** pursue (build-from-source audit) or skip this cycle.
13. **Name/legal:** Peersus trademark sanity check before public posts (unconfirmed — no search done).
14. **Romanian communities:** post in Romanian threads personally (authentic) vs English only.

*This document recommends; it does not authorize any posting, account creation, purchase or store submission.*
