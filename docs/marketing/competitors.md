# Competitor research for Peersus — the file-transfer market as it stands

Purpose: ground Peersus positioning in what competitors actually are, cost, and get complained about.
Researched on 2026-11-12. Every external fact below carries the URL it was fetched from on that date; where a number could not be confirmed it is marked **unconfirmed**. Peersus claims are referenced to the repository document they come from. GitHub stars/forks/issues were read from `api.github.com` on 2026-11-12 (URLs given per entry).

---

## 1. The three that matter most

### 1.1 LocalSend — the incumbent in Peersus's lane

- **What it is:** free, open-source app to share files and text with nearby devices over the local network, no internet connection or third-party server required. Written in Flutter/Dart. ([README](https://raw.githubusercontent.com/localsend/localsend/main/README.md), fetched 2026-11-12)
- **Platforms:** Windows 10+, macOS 12+, Linux, Android 7+, iOS 15+, Fire OS. Distributed through Winget/Scoop/Chocolatey/EXE, App Store/Homebrew/DMG, Flathub/Snap/AUR/Nix/DEB/TAR/AppImage, Play/F-Droid/APK, Amazon. ([README download table](https://raw.githubusercontent.com/localsend/localsend/main/README.md), 2026-11-12)
- **How a transfer works:** each device runs an HTTPS REST API on TCP/UDP port 53317; discovery is multicast on the LAN. The TLS certificate is generated on the fly per device. No account, no relay, no internet. ([README "How It Works"](https://raw.githubusercontent.com/localsend/localsend/main/README.md) and [protocol repo](https://github.com/localsend/protocol), 2026-11-12)
- **Encryption / what the operator can see:** self-signed per-device TLS on the LAN; there is no operator. Note the README itself suggests "Disable encryption on both devices" as a speed remedy — encryption is optional in practice. ([README troubleshooting](https://raw.githubusercontent.com/localsend/localsend/main/README.md), 2026-11-12)
- **Licence:** Apache-2.0. ([GitHub API](https://api.github.com/repos/localsend/localsend), 2026-11-12)
- **Price / limits:** free, in-app purchases listed on Play (donation model); no size limits. ([Play listing](https://play.google.com/store/apps/details?id=org.localsend.localsend_app), 2026-11-12)
- **Size limits:** none stated; per-file performance complaints exist (below).
- **Extensions/plugins:** none.
- **Pairing/remember:** no cryptographic pairing. "Favorites" pin a discovered device by name/IP so it is probed directly; identity is a random fingerprint per install, not a durable mutually-authenticated pairing. ([README troubleshooting](https://raw.githubusercontent.com/localsend/localsend/main/README.md), 2026-11-12)
- **Update model:** explicitly none — README: "the app does not have an auto-update"; users rely on stores/package managers. ([README](https://raw.githubusercontent.com/localsend/localsend/main/README.md), 2026-11-12)
- **Adoption:** 98,494 ★, 5,624 forks, 1,032 open issues; latest release v1.18.2 published 2026-08-21, arm64 APK alone downloaded ~234k times from GitHub. Google Play shows **10M+ downloads** and ~4.6★ (14–16k reviews; rating varies 4.2–4.7 by locale). ([GitHub API](https://api.github.com/repos/localsend/localsend), [release API](https://api.github.com/repos/localsend/localsend/releases/latest), [Play](https://play.google.com/store/apps/details?id=org.localsend.localsend_app); all 2026-11-12)
- **Strengths:** ubiquitous platform coverage incl. iOS; zero setup on a friendly LAN; huge mindshare — it *is* the category default for "AirDrop for everything".
- **Weaknesses / what users complain about (all fetched 2026-11-12):**
  - Transfer speed degrading or being abysmal on Wi-Fi: [issue #2547](https://github.com/localsend/localsend/issues/2547) (3–4 MB/s degrading over GBs), [#2382](https://github.com/localsend/localsend/issues/2382) (5 Mb/s phone-to-phone), [#1429](https://github.com/localsend/localsend/issues/1429) (600 KB/s over HTTPS, closed as duplicate), [#979](https://github.com/localsend/localsend/issues/979).
  - Discovery failures: [#1598](https://github.com/localsend/localsend/issues/1598) (no discovery on VPN), [#1406](https://github.com/localsend/localsend/issues/1406) (appears to need internet to begin), README's own troubleshooting table (AP isolation, public-vs-private Windows profile, iOS Local Network permission).
  - No interrupted-transfer resume — long-requested: [issue #1191](https://github.com/localsend/localsend/issues/1191) (already cited in docs/marketing.md).
  - No automatic clipboard sharing — [issue #187](https://github.com/localsend/localsend/issues/187) closed as duplicate of #163; Android background restrictions noted.
  - No internet/WAN send at all — same-LAN only by design; no relay fallback. No pairing, so every device appears under an auto-name and the receiver must approve against an unverified alias.
- **Recent news:** v1.18.x line active through 2026-08; releases continue at a steady cadence.

### 1.2 AlterSend — the closest product to Peersus's concept

- **What it is:** free, open-source app to send files directly between devices "over the internet — no cloud storage, no size limits". Positioned explicitly as "LocalSend but over the internet". Electron desktop + React Native/Expo mobile, Hyperswarm (Holepunch) underneath. ([README](https://raw.githubusercontent.com/denislupookov/altersend/main/README.md), [site](https://altersend.com/compare/altersend-vs-localsend); both 2026-11-12)
- **Platforms:** Windows (MS Store, EXE), macOS (DMG arm/intel, Homebrew), Linux (AppImage, Flatpak manifest, AUR), Android (Play, APK), iOS (App Store). ([README download table](https://raw.githubusercontent.com/denislupookov/altersend/main/README.md), 2026-11-12)
- **How a transfer works:** a 32-byte random key is the join code/QR; both sides join the Hyperswarm Kademlia DHT on a BLAKE2b hash of that code; transfer is a Noise-encrypted P2P socket; when direct connection fails (e.g. both behind symmetric NAT) an operator-run **relay** forwards the already-encrypted stream — on by default, can be disabled in Settings → Relay. Browser receive via app.altersend.com without installing. ([README "Under the hood"](https://raw.githubusercontent.com/denislupookov/altersend/main/README.md), 2026-11-12)
- **Encryption / operator visibility:** Noise E2E; DHT bootstrap nodes and the relay see connection metadata only, per the project. Opt-in Sentry crash reporting exists (off by default). ([README](https://raw.githubusercontent.com/denislupookov/altersend/main/README.md), 2026-11-12)
- **Licence:** Apache-2.0 (LICENSE file is verbatim Apache-2.0; GitHub API shows "NOASSERTION" because the file lacks the usual header, but the text is Apache-2.0). ([LICENSE](https://raw.githubusercontent.com/denislupookov/altersend/main/LICENSE), [API](https://api.github.com/repos/denislupookov/altersend); 2026-11-12)
- **Price / limits:** free; no size limit claimed; donation/sponsor model.
- **Pairing/remember:** yes — pair once, devices remembered; pairing stores a public device key, secret in OS keychain. This is a headline differentiator it shares with Peersus. ([README](https://raw.githubusercontent.com/denislupookov/altersend/main/README.md), 2026-11-12)
- **Extensions/plugins:** none.
- **Update model:** store + CI-built installers; no documented self-updater.
- **Adoption:** repo created 2026-05-15; 2,221 ★, 144 forks, 24 open issues, pushed 2026-10-30. Product Hunt page exists (~96 followers). Show HN posted. Store ratings too new/small to quote — **unconfirmed**. ([GitHub API](https://api.github.com/repos/denislupookov/altersend), [Product Hunt](https://www.producthunt.com/products/altersend/reviews), [Show HN](https://news.ycombinator.com/item?id=48408647); 2026-11-12)
- **Strengths:** literally Peersus's thesis (pair once, E2E, LAN + internet, no account, no size limit) shipped and on all five platforms incl. iOS — ~6 months of public head start; browser receive removes install friction for one-off receivers; 13 languages.
- **Weaknesses / complaints:** young codebase and one maintainer; Electron+Expo stack is heavy vs native; Sentry dependency (though opt-in); no extension model; relay/DHT trust rests on the operator's own infrastructure; store presence and review counts still thin. Complaints: too early for a body of public criticism — **unconfirmed**.
- **Recent news:** active releases; separate "AlterVault" photo-vault product announced on the site ([altersend.com](https://altersend.com/), 2026-11-12).

### 1.3 WeTransfer — the incumbent in "send to a stranger" territory

- **What it is:** the original big-file link-transfer service (founded 2009, Amsterdam). Upload → email/link → recipient downloads from WeTransfer's servers. Acquired by Bending Spoons July 2024. ([Consumer Rights Wiki](https://consumerrights.wiki/w/WeTransfer), 2026-11-12)
- **Platforms:** web; desktop/mobile apps exist but the link flow needs none.
- **How a transfer works:** files are uploaded to and stored on WeTransfer infrastructure; recipient pulls. Not P2P, not E2E.
- **Encryption / operator visibility:** transport encryption in transit/at rest on their servers — the operator can technically access stored content; transfers are not end-to-end encrypted. (Basis: service architecture; per WeTransfer help center the files are hosted on their cloud. [Plan limits page](https://site.wetransfer.com/help-center/subscriptions/plan-limits), 2026-11-12)
- **Licence:** proprietary SaaS.
- **Price / limits (fetched from own help-center page, updated 2026-10-23, read 2026-11-12):** Free — **10 transfers or 3 GB total per rolling 30 days** (this is far below the famous old 2 GB-per-transfer model). Starter — 10 transfers or 300 GB / 30 days. Ultimate — unlimited transfers, custom expiry, password, branding. Storage 2 TB default; Teams 5–30 TB. Third-party price watchers put Starter from ~$7/mo billed annually and Ultimate ~$19–25/mo after an October 2026 price cut — **unconfirmed** against the live pricing page, which renders client-side. ([Plan limits](https://site.wetransfer.com/help-center/subscriptions/plan-limits), [PricingSaaS](https://pricingsaas.com/companies/wetransfer), [PulseSignal](https://getpulsesignal.com/pricing/wetransfer); 2026-11-12)
- **Extensions/plugins:** none relevant (API exists historically).
- **Pairing/remember:** n/a — recipient needs only the link.
- **Update model:** SaaS.
- **Adoption:** one of the most recognized brands in the space; no reliable public user count — **unconfirmed**.
- **Strengths:** zero install, zero pairing, recipient needs nothing; brand trust for "send to a client".
- **Weaknesses / complaints:**
  - July 2025 ToS backlash: updated terms suggested uploaded files could be used to "improve machine learning models"; after press coverage (BBC, Guardian, PCMag, CNET) WeTransfer clarified and revised the wording — but the episode burned trust. ([BBC](https://www.bbc.com/news/articles/cp8mp79gyz1o), [Guardian](https://www.theguardian.com/technology/2025/jul/16/wetransfer-user-content-ai-artificial-intelligence); fetched 2026-11-12)
  - Post-acquisition: Bending Spoons laid off ~75% of staff (Sep 2024), plan structure rebuilt Dec 2024 into the restrictive rolling-window free tier. ([Reuters](https://www.reuters.com/technology/bending-spoons-lay-off-75-wetransfer-staff-post-deal-ceo-says-2024-09-08), [WeTransfer help](https://help.wetransfer.com/hc/en-us/articles/23797009758098); 2026-11-12)
  - Structural: files sit on third-party servers; links can leak; expiry; the free tier is now genuinely stingy.

---

## 2. The rest of the field (shorter entries)

**Snapdrop / PairDrop.** Snapdrop pioneered browser-only AirDrop-for-everything (PWA, WebRTC, same network). The SnapDrop/snapdrop repo (19,711 ★, GPL-3.0) has not been pushed since 2025-02-10; snapdrop.net was replaced by PairDrop, and open issues document an opaque handover to LimeWire — the pairdrop-android maintainer removed snapdrop.net support "for security reasons". ([GitHub API](https://api.github.com/repos/SnapDrop/snapdrop), [issue #663](https://github.com/SnapDrop/snapdrop/issues/663), [#651](https://github.com/SnapDrop/snapdrop/issues/651), [fm-sys/pairdrop-android#406](https://github.com/fm-sys/pairdrop-android/issues/406); 2026-11-12). **PairDrop** is the maintained community successor: same WebRTC P2P model plus persistent device pairing via 6-digit/QR shared secrets, temporary public rooms for cross-internet transfer, and a TURN fallback; still a browser PWA — no native app, no extension model. 11,792 ★, GPL-3.0; last push 2026-04-22 (activity slowing). ([README](https://raw.githubusercontent.com/schlagmichdoch/PairDrop/master/README.md), [API](https://api.github.com/repos/schlagmichdoch/PairDrop); 2026-11-12)

**Send Anywhere (Rakuten).** Freemium: 6-digit-key real-time P2P transfer up to **50 GB**; share-link (server-held) up to 10 GB free / 20 GB Lite / 30 GB Standard, ≤1,000 files per link, 48 h link life; "Directly To Device" up to 10 GB. Account-free for key transfers; closed source; contains ads + IAP. Play: 10M+ downloads, ~4.5★, ~347k reviews; ~39M installs per AppBrain. ([limits](https://support.send-anywhere.com/hc/en-us/articles/360005223534), [Play](https://play.google.com/store/apps/details?id=com.estmob.android.sendanywhere), [AppBrain](https://www.appbrain.com/app/com.estmob.android.sendanywhere); 2026-11-12)

**Warpinator (Linux Mint).** LAN-only file sharing, GPL-3.0, 1,596 ★, actively pushed 2026-11-10; ships with Mint, packaged on Arch/Alpine/Flatpak; unofficial Android/Windows ports (slowscript/warp). No pairing, no internet send, group-code trust model; README warns about fake warpinator websites distributing malware. ([API](https://api.github.com/repos/linuxmint/warpinator), [README](https://github.com/linuxmint/warpinator/blob/master/README.md); 2026-11-12)

**KDE Connect.** Device-bridge suite (clipboard, notifications, remote input, file/link share) not a dedicated transfer tool. LAN, TLS-encrypted pairing model — devices pair and are remembered, the model Peersus most resembles. Linux/Plasma first; Android on Play+F-Droid (1M+ downloads, ~28k reviews); iOS/macOS/Windows exist but feature-limited. GPL; mirror 4,151 ★. ([kdeconnect.kde.org](https://kdeconnect.kde.org/), [Play](https://play.google.com/store/apps/details?id=org.kde.kdeconnect_tp), [GitHub mirror API](https://api.github.com/repos/KDE/kdeconnect-kde), [privacy policy](https://community.kde.org/KDEConnect/PrivacyPolicy); 2026-11-12)

**AirDrop (Apple).** BLE discovery + peer-to-peer Wi-Fi, TLS-encrypted, recipient accepts/declines; since iOS 17.1 a transfer continues over the internet if devices leave range. Apple-only, no Windows/Android/Linux, "Contacts only" friction, and a June 2026 research disclosure showed nearby-attacker crash/bypass flaws in AirDrop (and Quick Share). ([Apple support](https://support.apple.com/guide/mac-help/mh35868/26/mac/26), [Apple Platform Security](https://support.apple.com/en-au/guide/security/sec2261183f4/web), [The Hacker News](https://thehackernews.com/2026/06/airdrop-and-quick-share-flaws-let.html); 2026-11-12)

**Quick Share (Google+Samsung).** Nearby Share + Samsung Quick Share merged Jan–Feb 2024 into one cross-Android stack; Android + ChromeOS built-in, Windows app (x64 and ARM since Nov 2024); Wi-Fi Direct — same-network requirement relaxed 2024; still nothing for macOS/iOS/Linux; closed source, Google account for some flows; same 2026 security disclosure applies. ([TechCrunch](https://techcrunch.com/2024/01/09/google-and-samsung-are-merging-nearby-share-and-quick-share-into-a-single-sharing-solution/), [9to5Google ARM](https://9to5google.com/2024/11/11/google-quick-share-windows-arm-release-coming/), [Android Authority](https://www.androidauthority.com/quick-share-for-windows-wifi-direct-3494451); 2026-11-12)

**Syncthing.** Continuous P2P folder *sync* (not send-once), TLS + device IDs, relays + discovery servers with E2E data, MPL-2.0, 90,378 ★. Official Android wrapper **discontinued** Dec 2024 (Play policy burden cited); community fork lives on F-Droid. No iOS officially (Möbius Sync third-party). Paired-and-remembered by design but aimed at standing folders. ([API](https://api.github.com/repos/syncthing/syncthing), [forum announcement](https://forum.syncthing.net/t/discontinuing-syncthing-android/23002); 2026-11-12)

**croc.** CLI file send: code-phrase → PAKE → E2E encrypted transfer, public relay by default (self-hostable), MIT, 40,914 ★, actively maintained. Terminal-only audience; no pairing/memory, no GUI; partial transfers not resumed across sessions. ([API](https://api.github.com/repos/schollz/croc); 2026-11-12)

**Magic Wormhole.** The reference PAKE design (Python CLI + libraries), MIT, 23,097 ★. Mailbox server + transit relay see metadata/ciphertext only. One-shot transfers, no persistence, no GUI. ([API](https://api.github.com/repos/magic-wormhole/magic-wormhole); 2026-11-12)

**Dropbox Transfer / Google Drive / Smash / Filemail (link senders).** Dropbox Transfer: free 2 GB, paid up to 100–250 GB. ([dropbox.com/features/share/file-transfer](https://www.dropbox.com/features/share/file-transfer), 2026-11-12) Filemail: free 5 GB, files held 7 days; Personal plan ~250 GB/unlimited transfers. ([filemail.com/large-file-transfer](https://www.filemail.com/large-file-transfer), [plans](https://www.filemail.com/plans/personal); 2026-11-12) Smash: free "no size limit" but >2 GB transfers join a slower queue; Pro ~$6–10/mo removes queue; 7-day availability. ([fromsmash.com](https://fromsmash.com/comparison/filemail), [brault.app review](https://brault.app/blog/file-transfer-and-sharing/smash-review); 2026-11-12) Google Drive: share links, 15 GB free storage tier. All are store-and-forward: operator holds plaintext (or holds the file at all).

**Tailscale Taildrop.** Send files between your *own* devices over WireGuard tailnet — E2E P2P, fastest available path; alpha; all plans incl. free; Windows/macOS/Linux(CLI)/Android/iOS; resumes interrupted transfers ≤1 h (not when macOS/iOS receives); cannot send to other users even on the same tailnet; requires Tailscale install + account on every device. ([Taildrop docs](https://tailscale.com/docs/features/taildrop), validated Jan 2026, fetched 2026-11-12)

**ShareDrop.** WebRTC PWA in the Snapdrop lineage, MIT, 10,769 ★, last push 2025-02-10 — effectively unmaintained. ([API](https://api.github.com/repositories/17940105); 2026-11-12)

**Blip.** Proprietary native apps (macOS/Windows/iOS/Android, Linux — **unconfirmed**), P2P-first with a "fast relay" when direct fails, mTLS/TLS 1.3 E2E, no size limits, **auto-resume on interruption** (its flagship claim), LAN direct-connect. Free for non-commercial use (standard speed, donation-supported); Business $25/user/mo ($300/yr) or $30 monthly for commercial use. Play ~4.7★ (~7k reviews). No open source, no extensions. ([blip.net](https://blip.net/), [pricing](https://blip.net/pricing), [Play](https://play.google.com/store/apps/details?id=net.blip.android); 2026-11-12)

**Resilio Sync.** BitTorrent-derived folder sync, now owned by Nasuni; personal Sync is free and as of 2026 includes all former Pro features; adds a one-off "send big files to friends and family" flow. Proprietary; business plans paid. ([resilio.com/sync](https://www.resilio.com/sync/); 2026-11-12)

**LANDrop.** LAN drop-any-file utility, BSD-3, 6,001 ★, but last push 2024-08-14 — dormant; no pairing, no encryption claims beyond TLS. ([API](https://api.github.com/repos/LANDrop/LANDrop); 2026-11-12)

**Feem.** "Share files offline" proprietary freemium (FeePerfect AG); Windows/macOS/Linux/iOS/Android/Web; Feem Pro ~$4.99+/yr per device for unlimited features; v5 released Jan 2024; chat + auto-delete messages. Play listing exists; ratings thin — **unconfirmed**. ([feem.io blog](https://feem.io/blog/2024-01-28), [old.feem.io/pricing](https://old.feem.io/pricing), [Play](https://play.google.com/store/apps/details?id=com.feeperfect.airsend.android); 2026-11-12)

**Flying Carpet.** Open-source ad-hoc-Wi-Fi transfer (no network needed at all — device hosts hotspot), Rust+Tauri, GPL-3.0, 5,377 ★; Windows/macOS/Linux/Android/iOS; niche but the only "no infrastructure whatsoever" option besides AirDrop/Quick Share. ([API](https://api.github.com/repos/spieglt/FlyingCarpet); 2026-11-12)

---

## 3. What Peersus is — per repository evidence (not marketing)

Peersus facts below cite repo docs; where the docs say a thing is unverified, that label is kept.

- Deliberate pairing that never expires + per-file receive consent; authenticated QUIC transport, E2E; direct-first with configurable relays that see metadata not content, and can be turned off. (docs/architecture.md "Transfers and relay configuration")
- LAN **and** over-internet transfer in one code path: files-code/QR invitation for unpaired sends, remembered devices thereafter. (docs/architecture.md; README "Pair deliberately. Transfer directly when possible.")
- Experimental BLE "devices nearby" discovery/offers, off by default, Linux+Android only. (docs/nearby-send.md)
- A signed extension store + WASM/native extension hosts with per-capability grants and mandatory licence/notice flow — no competitor in this list has a real plugin platform. (docs/extension-store.md; README Extensions)
- A signed self-updater (Ed25519, channel + rollback floor), observed working on Linux and Android community builds with dev keys; Play path uses Play's update mechanism. (docs/app-update.md)
- Full-fidelity CLI (`peersus call/state/page/status/answer`) — richer than LocalSend's young CLI. (README CLI section)
- No telemetry, no account, Apache-2.0. (README; site/index.html)
- **Honest status:** workspace version 0.1.0, private/unpublished; go-no-go verdict is **No-Go for public release on every platform**; Linux+Android are the only runtime-verified targets; Windows/macOS unsupported today (no window ever opened on either; no updater); production store/update keys don't exist; transfer-resume does not exist ("Reconnecting is not file-transfer resume"). (docs/go-no-go.md; docs/architecture.md; docs/app-update.md "Not done")

## 4. Peersus vs each — adds and lacks

| Vs | Peersus adds | Peersus lacks |
|---|---|---|
| LocalSend | Persistent authenticated pairing; internet reach via QUIC+relay; BLE nearby offers; plugin platform; self-updater; Rust native stack | Everything is unproven outside Linux+Android; iOS absent entirely; LocalSend has 10M+ installs, iOS, store presence, and just works on LAN today; Peersus currently requires deliberate pairing ceremony where LocalSend needs none |
| AlterSend | Real extension platform with signed catalog + permission grants; Rust native (vs Electron/RN); no Sentry-type dependency; more honest consent model per design; app-level self-updater | AlterSend already ships all 5 platforms incl. iOS, browser-receive links, 13 languages, public releases — Peersus has zero public artifacts; Peersus relay is project-operated (similar trust posture), and its DHT-free model needs the project relay or n0 pool reachable |
| WeTransfer | No upload-store-download: payload never touches an operator; no per-transfer quota, expiry, or account; consent gate on receive | WeTransfer's recipient needs nothing — Peersus has no browser-receive path at all; link-senders serve strangers at any file size with zero install; WeTransfer is a known brand, Peersus is unpublished |
| PairDrop/Snapdrop | Native app, authenticated pairing, internet reach without manual TURN dependence, extension model, no browser tab required during transfer | PairDrop needs literally zero install and does pairing + internet rooms in a browser; Peersus cannot serve a no-install receiver today |
| Send Anywhere | Open source; no ads; no 50 GB cap; persistent pairing; no server-held files | Send Anywhere's 6-digit P2P reaches 50 GB with no account and is on every store today with ~39M installs |
| KDE Connect | Focused transfer UX; internet reach; signed plugin model | KDE Connect is a whole device bridge (clipboard/notifications/input) with remembered TLS pairing — Peersus's clipboard story is an unbuilt extension |
| Quick Share / AirDrop | Cross-platform reach incl. internet, consent per file, open code | Both are OS-integrated, zero-install, free; Quick Share covers Android+Windows out of the box — the exact Peersus wedge is narrower than the built-in |
| Taildrop | No Tailscale account/VPN dependency; send to *other people*, not just own devices; files-code for unpaired sends | Taildrop already resumes interrupted transfers (1 h window) and rides a mature mesh; Peersus has no resume |
| croc / Magic Wormhole | Persistent pairing, GUI, relay policy controls, plugins | Those are 40k/23k-star mature CLIs with PAKE simplicity; Peersus's ceremony is heavier for a one-off send |
| Blip | Open source, no $25/mo commercial gate, no account | Blip ships resumable transfers, LAN direct-connect, 4 platforms, polished speed — Peersus's strongest gap: **no resume** |
| Syncthing/Resilio | One-shot consent-bound sends rather than standing folder replication; lighter footprint | They are decade-mature sync engines with remembered devices; Peersus doesn't try to be sync — different job |
| Feem / LANDrop / ShareDrop / Warpinator | Remembered authenticated pairing, internet reach, plugins, active development | All are dormant or narrow; risk is low but they cover the "zero-ceremony LAN drop" niche Peersus deliberately doesn't |

## 5. Tables

### 5.1 Platform coverage (verified state, 2026-11-12)

| Tool | Linux | Windows | macOS | Android | iOS | Web/no-install |
|---|---|---|---|---|---|---|
| LocalSend | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| AlterSend | ✅ | ✅ | ✅ | ✅ | ✅ | receive-only (app.altersend.com) |
| WeTransfer | via browser | ✅ | ✅ | ✅ | ✅ | ✅ full |
| PairDrop | ✅ PWA | ✅ PWA | ✅ PWA | ✅ PWA | ✅ PWA | ✅ native model |
| Send Anywhere | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ receive/link |
| AirDrop | — | — | ✅ | — | ✅ | — |
| Quick Share | — | ✅ app | — | ✅ built-in | — | — |
| KDE Connect | ✅ | ✅ | ⚠️ partial | ✅ | ⚠️ partial | — |
| Syncthing | ✅ | ✅ | ✅ | ⚠️ fork only | ⚠️ 3rd-party | — |
| Taildrop | ✅ CLI | ✅ | ✅ | ✅ | ✅ | — |
| Blip | ⚠️ unconfirmed | ✅ | ✅ | ✅ | ✅ | — |
| croc / wormhole | ✅ | ✅ | ✅ | ⚠️ ports | — | — |
| Resilio/Feem/LANDrop/FlyingCarpet | ✅ | ✅ | ✅ | ✅ | ✅ (Feem/FC/Resilio) | — |
| **Peersus** | ✅ verified | ❌ unsupported | ❌ unsupported | ✅ verified (API 31+) | ❌ | ❌ |

### 5.2 Privacy / trust model

| Tool | Where the bytes go | Who can see content | Account | Metadata exposure |
|---|---|---|---|---|
| LocalSend | LAN direct | nobody (self-signed TLS; can be disabled) | none | LAN peers see device names |
| AlterSend | P2P DHT + relay fallback | endpoints only (Noise) | none | DHT bootstrap + relay operator see connections |
| Peersus | P2P QUIC direct-first + optional relay | endpoints only | none | relay sees connection metadata (stated plainly in docs) |
| PairDrop | WebRTC P2P (+TURN) | endpoints | none | signalling server + TURN see connections |
| Send Anywhere | P2P key transfer or server-held link | operator can hold link transfers | none for keys | ad-tech + server logs (ads, IAP) |
| WeTransfer/Dropbox/Smash/Filemail | operator cloud | operator | optional | full content access in principle |
| AirDrop/Quick Share | direct Wi-Fi | endpoints | Apple/Google identity layer | vendor sees nothing of payload; account linkage exists |
| KDE Connect/Syncthing/Taildrop/Resilio | direct/mesh | endpoints | none (KDE/Syncthing) or vendor account (Tailscale/Resilio) | discovery/relay/coordination metadata |
| croc/wormhole | P2P or public relay | endpoints (PAKE E2E) | none | relay/mailbox sees metadata |
| Blip | P2P + operator fast-relay | endpoints (mTLS claim) | none for community | relay operator sees connections |

## 6. Market gaps and positioning

**Observed gaps:**

1. **"LocalSend that works over the internet" is the open lane — and AlterSend found it first (May 2026).** PairDrop's rooms and Blip's relay cover the same ground partially. Peersus must differentiate on *trust architecture and extensibility*, not on the bare feature, which now has a 2.2k-star incumbent.
2. **Transfer resume is a named, evidenced pain** (LocalSend #1191; Blip built its brand on it; Taildrop has a 1 h window). Peersus explicitly lacks resume — the single most defensible feature investment if/when added; until then it is a hole, not a differentiator.
3. **No competitor has a signed, permission-granted extension platform.** This is Peersus's only structurally unique asset — but the production store key doesn't exist and zero extensions ship.
4. **Consent+identity is sold nowhere else at this rigor:** per-file accept, durable authenticated pairing, honest relay-metadata disclosure. KDE Connect pairs-but-is-a-suite; Blip is closed.
5. **Linux-first credibility:** Warpinator, LANDrop dormant; no serious cross-platform E2E sender targets Linux as a first-class platform. Peersus's verified platform is Linux — honest scarcity.
6. **Browser-receive gap:** every successful "send to stranger" tool (WeTransfer, PairDrop, AlterSend, Send Anywhere links) has a no-install receiver path. Peersus has none — the files-code requires the receiver to have the app. Real product gap.

**Positioning candidates:**

- **A. "The open, honest AlterSend — native Rust, no Electron, extensions."** Risk: reads as a fast-follow with fewer platforms and no downloads yet; invites direct feature checklist comparison where Peersus loses today (iOS, browser-receive, language count).
- **B. "LocalSend for devices that trust each other" — lean into deliberate pairing + consent + remembered devices, LAN-first, internet as the bonus.** Risk: pairing ceremony is friction; LocalSend's zero-ceremony flow is beloved; must prove pairing beats auto-discovery in everyday use, not just in security posture.
- **C. "The extension platform for device-to-device workflows" — Peersus as the host, transfers as the first built-in use.** Risk: most differentiated but least legible to consumers; the store has no production key and no extensions — the platform story is currently the least verifiable claim in the repo.

**Target audiences (by evidence, not guesswork):**

1. Linux+Android owners who already hit LocalSend's walls — VPN/multi-network users (issue #1598 population), same-account multi-device senders wanting remembered pairing, and privacy readers who want relay-metadata honesty instead of "just trust the LAN".
2. Rust/open-source technical users who value a CLI-parity app and auditable stack (croc/wormhole audience wanting a GUI with durable pairing).
3. Long-tail: privacy-sensitive senders fleeing store-and-forward link services post-WeTransfer-ToS-episode — but only once a no-install receive path exists; today Peersus cannot serve this audience.

## 7. SWOT

**Strengths (verified):** durable authenticated pairing; E2E QUIC with honest relay semantics and relay off-switch; per-file consent model; signed self-updater already demonstrated (dev keys); signed extension-store architecture with capability grants; full CLI parity; zero telemetry; native Rust footprint vs Electron.

**Weaknesses (verified):** unpublished, v0.1.0, no public artifacts; only Linux+Android runtime-verified; no iOS path; no transfer resume; no browser-receive; production store/update keys absent; BLE nearby experimental and limited to two platforms; several open audit findings in go-no-go.

**Opportunities:** LocalSend's evidenced resume/discovery pain; WeTransfer's trust erosion; Snapdrop's abandonment vacuum; Linux-first sender niche; extension platform as durable differentiator; AlterSend validates the category but is a one-maintainer Electron product.

**Threats:** AlterSend's 6-month lead on the exact thesis with all-5-platform releases and browser receive; OS-level Quick Share/AirDrop capture casual demand for free; LocalSend could add WAN/relay (protocol v2 discussions exist in its ecosystem); Peersus's own release blockers (go-no-go) keep it invisible while the window narrows.

## 8. Honest minuses

- Peersus today **loses the feature checklist to AlterSend on nearly every axis users can see**: platforms, browser receive, languages, public releases — AlterSend shipped what Peersus promises.
- **No resume** — the most-requested capability in the category — and the docs say reconnecting is not resume.
- **No zero-install receiver path**: sending to a non-user requires them to install and pair; WeTransfer/PairDrop/AlterSend/Send Anywhere all have a lighter path for strangers.
- **Two platforms only, and a No-Go verdict**: no Windows, no macOS window ever opened, no iOS story; the marketing site already says this, and any competitive claim must carry it.
- **Pairing friction is real**: the deliberate ceremony that is Peersus's security story is also more steps than LocalSend's tap-tap-accept on a friendly LAN.
- **The extension platform — Peersus's only unique claim — is currently unusable** (no production key, no shipped extensions, no rate-limit suspension per go-no-go blocker 3).
- **Relay trust is not zero**: Peersus relays see connection metadata; AlterSend's DHT+relay has the same posture, but "no server of ours" marketing must never overclaim this.
- **Android background/lifecycle gaps are documented open issues** (deep-sleep unresponsiveness, CPU burn on name resolution) — competitors' Android apps are boring and stable by comparison.

## 9. Where we win

- **Trust architecture nobody else ships**: durable authenticated pairing + per-file consent + honest relay disclosure + relays can be disabled entirely. LocalSend has no identity at all; KDE Connect's pairing is a suite feature, not the product.
- **The only extension platform in the category**: signed catalog, per-capability grants, WASM isolation, mandatory licence flow. If the store ships, this is a moat no listed competitor has even attempted.
- **Self-updating done right**: signed releases, channel pinning, rollback floor — observed working end-to-end (dev keys). LocalSend deliberately ships no updater; most tools punt to stores.
- **Linux-first, native, auditable**: a real Rust app with CLI parity on the platform Warpinator abandoned and LocalSend treats as one-of-six.
- **LAN+internet in one consent model**: same authenticated path near or far — only AlterSend matches this today, and Peersus adds pairing-graded trust and plugin extensibility on top.
- **No ads, no account, no telemetry, no store-and-forward**: the privacy posture WeTransfer's 2025 episode made salient — stated plainly rather than implied.
