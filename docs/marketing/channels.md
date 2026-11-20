# Where to post and the rules of each channel

Purpose: per-channel rules, costs, formats and go/no-go status for the Peersus launch, each norm cited. Researched on 2026-11-12. Reddit pages could not be fetched directly (anti-bot); subreddit rules are cited from their canonical pages and the quoted text comes from public rule listings/search — **re-verify each rule page on the day**, subreddits change rules often.

Legend: ✅ launch-ready · ⚠️ conditional · ⏳ later · ❌ excluded.

## Aggregators & communities

### ✅ Hacker News (news.ycombinator.com)
- Rules: [Show HN rules](https://news.ycombinator.com/showhn.html) — must be something people can try; no landing pages, no "not ready" work; don't ask friends to upvote. [Site guidelines](https://news.ycombinator.com/newsguidelines.html) — no soliciting votes/comments; no deleting-and-reposting; submit original source; no generated text; don't use HN primarily for promotion.
- Norms that matter: the biggest wins in this category were *regular* submissions of the repo posted organically — LocalSend [923 pts, Apr 2026](https://news.ycombinator.com/item?id=47933208); LocalSend [563 pts, Oct 2023](https://news.ycombinator.com/item?id=37938183); Syncthing [623 pts, 2014](https://news.ycombinator.com/item?id=7734114). PairDrop's history also shows a 102-pt "repo vanished" scare thread — keep the repo up ([item 39665668](https://news.ycombinator.com/item?id=39665668)).
- Cost: free. Format: plain title + URL, then a first comment. Timing: weekday, US-awake hours (see launch-day.md §2).

### ⚠️ Lobsters (lobste.rs)
- Rules: [about](https://lobste.rs/about) — self-promo must be **under ~25% of your activity** and **new users (first 70 days) may not submit self-promo at all**; invites come via the invitation tree/chat; marketing-analytics domains banned and `utm_*` parameters stripped.
- Plan: **not a launch channel** unless the owner already holds an account ≥70 days old. Better route: a genuinely technical post (e.g., "designing per-extension grant contracts in Rust") later in the cycle, submitted once the project is public.

### Reddit — general
- Site-wide: the longstanding self-promotion norm is ~10% of your activity may be self-promotional ([Reddit self-promotion wiki](https://www.reddit.com/wiki/selfpromotion); Reddit's content policy on spam applies everywhere — [Reddit Help](https://support.reddithelp.com/hc/en-us/articles/28012014962580-How-do-I-keep-spam-out-of-my-community)). Account must look like a person, not a launch script: comment history, no brand-new account posting its own project.
- Reddit could not be fetched from this environment (403/JS challenge); rule texts below come from the subs' public rule listings surfaced via search — verify at `https://www.reddit.com/r/<sub>/about/rules` on the day.

| Sub | Status | Rule of record | Where to post |
|---|---|---|---|
| r/fossdroid | ✅ | Purpose is "to promote … FOSS alternatives on Android" ([posting guidelines](https://www.reddit.com/r/fossdroid/comments/1jpb78f/rfossdroid_posting_guidelines), [rules.json](https://www.reddit.com/r/fossdroid/about/rules.json)) | Normal post, launch day |
| r/selfhosted | ⚠️ | Self-promo is restricted; weekly AutoMod **"New Project Megathread"** exists ([example](https://www.reddit.com/r/selfhosted/comments/1xcw6eb/new_project_megathread_week_of_22_oct_2026/), [rules.json](https://www.reddit.com/r/selfhosted/about/rules.json)) | The weekly megathread; modmail if unsure. Fit is partial (client app, not a server). |
| r/androidapps | ⚠️ | Self-promotion only via pinned **"Self Promotion Megathread"** ([megathread listing](https://www.reddit.com/r/androidapps?f=flair_name%3A%22MEGATHREAD%22)); only Play/Aurora/Samsung/F-Droid/GitHub links pre-approved — no direct APK links ([rules.json](https://www.reddit.com/r/androidapps/about/rules.json)) | Megathread comment linking the GitHub release |
| r/opensource | ⚠️ | Self-promo "allowed with conditions" — must add value beyond the link ([rules.json](https://www.reddit.com/r/opensource/about/rules.json), [rules page](https://www.reddit.com/r/opensource/about/rules)) | One substantive self-post (what's verified/what isn't), not a link-drop |
| r/linux | ⚠️ | No explicit self-promo rule text found; "no spamblog, original source/original title" applies ([about.json](https://www.reddit.com/r/linux/about.json)). Mods remove promo at discretion — modmail first. | One post after modmail OK |
| r/rust | ⚠️ | Self-promo is restricted/case-by-case ([rules wiki](https://www.reddit.com/r/rust/wiki/rules)); "lesson learned" framing beats announcements | One technical post only, not "release" spam |
| r/privacy | ❌ | "No Self-Promotion Or Other Promotional Content" ([rules](https://www.reddit.com/r/privacy/about/rules)) | Do not post |
| r/android | ❌ | r/Android wiki restricts self-promotion and app promo ([rules](https://www.reddit.com/r/Android/wiki/rulesandregs)) | Do not post |
| r/Romania | ⚠️ | The [regulament](https://www.reddit.com/r/Romania/wiki/regulament) has a defined theme; self-promo norm unconfirmed. Personal Romanian-language post in a free thread is safest. | "Forum Liber" thread or r/programare; owner decision |
| r/UniRO / r/programare / r/Roumanie | ⚠️ | Smaller subs, rules unconfirmed — check `/about/rules` on the day | Optional, personal post only |

### ⏳ Product Hunt
- Rules/mechanics: [How Product Hunt works](https://help.producthunt.com/en/articles/17734306-1-how-product-hunt-works) — launches go live **12:01 AM Pacific**, daily leaderboard, featuring is editorial, personal accounts only (no brand accounts), verified email required, **paid upvotes get you banned**, launch is free.
- Verdict: ⏳ week 4–6, once the demo video and polish exist; PH rewards shiny consumer products, and a pre-release FOSS tool with missing platforms will underperform. Needs: maker comment prepared, thumbnail, gallery images, first-comment FAQ (assets-checklist.md).
- Note: "relaunching" an already-submitted product has its own rules — don't submit early "to reserve" the name.

## Listings & stores

| Channel | Status | Norm/rule (cited) |
|---|---|---|
| GitHub Releases | ✅ | The release *is* the product page; checksums, notes, assets. Free. |
| Google Play | ⏳ | Needs: accessibility-service disclosure+consent+declaration or move to community flavour; published privacy policy URL in Console **and** in-app ([Play Help privacy](https://support.google.com/googleplay/android-developer/answer/10144311)); data-safety form ([Play Help](https://support.google.com/googleplay/android-developer/answer/10787469)); new personal accounts need a **closed test with ≥12 testers opted in for ≥14 days** ([Play Help](https://support.google.com/googleplay/android-developer/answer/14151465)); targetSdk 36 required ([Play Help](https://support.google.com/googleplay/android-developer/answer/11926878)). Whether WASM extensions pass Play's interpreter rule is **unconfirmed** ([store-policy-google-play.md](../store-policy-google-play.md)). Cost: $25 one-time ([sign-up](https://support.google.com/googleplay/android-developer/answer/6112435)). |
| F-Droid | ⏳ (research task) | [Inclusion policy](https://f-droid.org/en/docs/Inclusion_Policy/): 100% FLOSS build; no proprietary tracking/ads libs; **must not download additional executable binaries without opt-in consent that explains bypassing F-Droid's checks** — the extension store's WASM/native downloads need explicit analysis; the default relay may need a NonFreeNet-style Anti-Feature flag; app should be ≥3 months actively developed (satisfied by history, unconfirmed). Owner research item, not a launch channel. |
| Flathub | ⏳ | [Requirements](https://docs.flathub.org/docs/for-app-authors/requirements): meaningful dev history + tagged releases; **AI-generated material must be disclosed, manifests must not contain AI-generated content, and AI agents must not author the submission PR or its text**; sandbox-hostile features may be rejected. Needs a working Flatpak — none exists today (go-no-go 1). |
| appimage.github.io | ⏳ | [Submission rules](https://github.com/AppImage/appimage.github.io): one PR adding `data/Peersus` containing the repo URL; AppImage must be type-2 SquashFS, run on the oldest supported Ubuntu LTS, **start without network**, show a window in 30 s, pass `desktop-file-validate` + appdir lint; X11 required; AppStream metainfo enables own screenshots. No AppImage exists today (go-no-go 1). |
| awesome-selfhosted | ⚠️/❌ | Contributions go via [awesome-selfhosted-data](https://github.com/awesome-selfhosted/awesome-selfhosted-data/blob/master/CONTRIBUTING.md) (one YAML file, kebab-case; security-questionable software removed). **Fit doubtful**: Peersus is a client app, not server software — evaluate honestly; do not force it. |
| Apple stores / Developer ID | ❌ now | macOS aborts without Metal; no `.app`/signing/notarisation; MAS cannot carry native extensions or self-updater; Ko-fi link risks 3.1.1 ([store-policy-apple.md](../store-policy-apple.md), [Apple guidelines](https://developer.apple.com/app-store/review/guidelines/), [$99/yr](https://developer.apple.com/programs/)). Publish "not supported". |

## Press & newsletters (tips are free; personalize, never blast)

| Outlet | How | Source |
|---|---|---|
| LWN.net | tips to `lwn@lwn.net`; say if you don't want credit | [LWN contact](https://lwn.net/lwn/op/Contact.html) |
| OMG! Ubuntu! | tip form | [omgubuntu.co.uk/tip](https://www.omgubuntu.co.uk/tip) |
| 9to5Linux | contact page (Marius Nestor) | [9to5linux.com/contact-us](https://9to5linux.com/contact-us) |
| Android Police | tips/contact page | [androidpolice.com/contact](https://www.androidpolice.com/contact) |
| It's FOSS | contact/about page | [itsfoss.com](https://itsfoss.com/) — verify current tip channel on the day |
| XDA Forums | Android "Apps and Games" thread; include a real description, not just a link; self-promo clause applies | [XDA forum rules](https://xdaforums.com/t/xda-developers-forum-rules.4200559) |
| This Week in Rust | ⚠️ **Project/Tooling Updates no longer accepts PR submissions**; a *written-by-a-human* walkthrough/deep-dive article can still be proposed via PR to the draft | [TWIR README](https://github.com/rust-lang/this-week-in-rust) |
| Linux YouTubers (e.g. channels covering Linux apps: The Linux Experiment, Brodie Robertson, Switched to Linux, TechHut) | ⚠️ cold-pitch norms vary; email a 3-line pitch + demo video link, never a form-letter blast; no pay-for-mention without "sponsored" disclosure | channels' own About/商务 pages — verify on the day |
| FOSDEM / Linux meetups (Brussels; RO: local LUGs, DevTalks) | ⏳ CFP deadlines; a 5-min lightning talk on the extension-permission model is honest material | [fosdem.org](https://fosdem.org/) — verify edition dates |

## Social

| Channel | Status | Norms |
|---|---|---|
| Mastodon (fosstodon.org / mastodon.social) | ✅ | Server rules apply per instance ([mastodon.social standards](https://mastodon.social/about), [community standards](https://help.joinmastodon.org/article/12-community-standards)); fediquette: CW where apt, alt text on images, hashtags for discovery ([fediquette guide](https://help.joinmastodon.org/article/13-fediquette-community-etiquette-guide)). #linux #foss #android |
| Bluesky | ✅ | Standard posts; link cards need OG tags (website.md). |
| X/Twitter | ✅ | Weakest signal for FOSS launches but free; one post + thread reply. |
| LinkedIn | ⚠️ optional | Only if the owner wants the professional-audience angle. |
| Discord/Matrix/Slack communities (self-hosted, linux, rust) | ⚠️ | Server-specific promo channels only; many ban drive-by promos entirely — read pins; never DM members. Owner decision which to join. |

## Excluded channels (with reasons)

- **r/privacy, r/android** — promo banned (above).
- **Paid ads / promoted posts** — budget §9 in plan.md; misaligned with a privacy tool's credibility.
- **IndieHackers/ShowHacker clones, launch-directory spam farms** — low signal, and mass-submitting the same text reads as spam.
- **Email newsletters with paid "placement"** — undisclosed paid placement conflicts with the honesty pillar.
