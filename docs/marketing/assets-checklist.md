# Launch assets: what exists and what is missing

Purpose: a complete inventory of visual/press assets, what already exists in the repo, and exactly what must still be produced for launch day. Researched on 2026-11-12.
Everything here must show the real application — the site's own rule applies: illustrations are labelled illustrations, and "no fabricated testimonials, supporters, counters, ratings or press logos" ([marketing.md](../marketing.md)).

## 1. What exists

| Asset | Where | State |
|---|---|---|
| Wordmark + mark (SVG) | `site/brand/peersus.svg`, `site/brand/peersus-mark.svg` | ✅ production brand, used on the site |
| Hero composite | `docs/assets/readme-hero.png` (1672×941) + `readme-hero-bg.png` | ✅ real Android app frame in a branded layout — but the app UI shown is **Romanian**; fine for README, needs an English variant for international press/OG |
| Screenshot corpus | `shots/` (~120 PNGs: `nx-*` Android, `desk*`, `xvfb-*` desktop, `qr*`, `land*`, `pt*`, `pn*`, `v_*`, `final_*`) | ⚠️ raw development captures, some contain test-device names/states; usable as source material only after review & crop — never publish unreviewed (may show device names, pairing codes, IPs in UI chrome) |
| Extension demo fixture | `store_demo` / signed Echo fixture | ⚠️ not a real extension; never screenshot it as "the store" |
| Website diagrams | `site/index.html` inline illustration | ✅ labelled illustration |

## 2. Missing — required for launch (blockers)

| # | Asset | Spec / notes | Needed by |
|---|---|---|---|
| A1 | **OG/link-preview image** `site/og.png` | **1200×630 px** PNG (de-facto OG standard — [Meta docs](https://developers.facebook.com/docs/sharing/webmasters/images)); reuse `readme-hero.png` cropped/re-rendered at exactly 1200×630 with an **English** UI frame or the brand side only; add `og:*`/`twitter:card` meta (website.md #3) and `og.png` to `FILES` in `site/build.py` | Day D — every posted link renders it |
| A2 | **Screenshot set (5–8)** | Real app, real devices, English UI: (1) Send/pick-files, (2) Peers list + pairing code, (3) the pairing circle on both devices, (4) Receive + per-file accept, (5) Transfers/history, (6) Settings → relay policy, (7) extension agreement overlay. Same set powers the download page, press kit, Play listing later. Crop status bars; check nothing private shows. | Day D — download page + press tips |
| A3 | **Demo video, 60–90 s** | One continuous take on real devices: open app on Linux + Android → pair → send a file → receiver approves → file lands in Downloads. Screen-record each device (Android: `adb shell screenrecord` or built-in recorder; desktop: OBS/PipeWire capture), join the takes, add captions. **No simulated progress, no fake speed** ([marketing.md](../marketing.md)). Captions + a non-video text version required (accessibility). | Day D — hero/press/PH later |
| A4 | **English UI hero frame** | Re-render `readme-hero.png` with English app UI (the app supports multiple languages per engineering-log i18n guards — unconfirmed which; use the shipping default). Needed for OG (A1) and press kit. | Day D |
| A5 | **Press kit folder** `/press/` or `press.html` + zip | Contents: brand SVGs (light+dark), mark PNG exports (512, 1024), screenshot set (A2), demo video link, 1-paragraph brief (the A-pitch), 3 facts ("Apache-2.0 · Linux+Android · no account"), honest limitations paragraph, contact line, "not affiliated with" note if mentioning AirDrop. | Week 0 — press tips |

## 3. Missing — channel-specific (schedule order)

| # | Asset | For | Spec / cite | When |
|---|---|---|---|---|
| B1 | GitHub **social preview** image | repo Settings → Social preview | 1280×640 recommended ([GitHub community](https://github.com/orgs/community/discussions/)) — *unconfirmed official doc; verify in repo settings UI* | Week 0 |
| B2 | **Feature graphic** | Google Play listing later | 1024×500 (Play listing norm — verify current spec when the Console account exists) | Week 3+ |
| B3 | Play **phone + tablet screenshots** | Play listing | from A2 set + tablet-UI captures; Play needs at least phone screenshots | Week 3+ |
| B4 | **Product Hunt gallery** | PH launch (week 4–6) | 240×240 thumbnail + gallery images (1270×760 common norm — verify current spec on [PH launch guide](https://www.producthunt.com/launch)) | Week 5 |
| B5 | **AppStream metainfo** screenshot | appimage.github.io + Linux stores | referenced via metainfo file so the catalog shows *your* screenshot, not an auto-capture on a bare X session ([AppImageHub rules](https://github.com/AppImage/appimage.github.io)) | When AppImage exists |
| B6 | Flathub screenshots | Flathub listing | per [Flathub requirements](https://docs.flathub.org/docs/for-app-authors/requirements) appdata | When Flatpak exists |
| B7 | Short GIF (≤8 s, ≤5 MB) | Reddit/social posts | the "send → accept → lands" moment only; GIFs outperform walls of text on r/fossdroid | Week 0–1 |
| B8 | Mastodon/social card variants | social posts | reuse A1; add `alt` text everywhere (fediquette — [Mastodon guide](https://help.joinmastodon.org/article/13-fediquette-community-etiquette-guide)) | Week 0 |

## 4. The demo-video plan (A3) in detail

- **Script (75 s):** 0–8 s both apps open (Linux window + phone side by side); 8–20 s pairing: code shown on one, entered on the other, same-circle confirmation; 20–35 s pick a file on the desktop (a real photo/PDF you own); 35–50 s phone shows the incoming offer, person reads the file name/size and presses accept; 50–60 s progress, file opens from Downloads; 60–75 s Settings → relay section, one line of text: "encrypted end to end; relays carry ciphertext".
- **Rules:** real devices only; no speedrun edits during the transfer; record the network conditions honestly in the caption ("same Wi-Fi" / "different networks via relay"); captions on, no voiceover needed; a text version goes under the video on the site.
- **Failure rule:** if pairing takes >30 s in the take, keep it or re-record — never cut the wait; the wait is the product.

## 5. Screenshot hygiene (applies to A2 and any post)

- Fresh capture on the **release build**, not a dev build — the build number may show.
- Scrub: device names that identify the owner, pairing codes/invitations, relay tokens, IPs/hostnames, notification contents from other apps, personal files. The engineering-log rule applies: never publish invitations/identities/tokens ([docs/README.md](../README.md)).
- Consistent theme (pick light or dark and stay) and window size; PNG for UI (no JPEG smear on text).
- `alt` text describes what the screenshot proves ("Receive screen asking to accept report.pdf, 2.4 MB").

## 6. Press kit page copy (A5)

```
Peersus — consent-first file transfer between your own devices.
Pre-release: Linux and Android tested; Windows/macOS not supported.
Pair devices deliberately, receiver approves every file, end-to-end
encrypted direct-first with optional relays that see metadata not
contents. No account, no ads, no tracking. Apache-2.0.
Contact: <owner email>. Assets: logos (SVG/PNG), screenshots, demo video.
```

## 7. Owner decisions for assets

1. OG image content: cropped hero (fast) vs dedicated card render (better).
2. English vs bilingual demo video captions.
3. Whether `shots/` corpus gets a curated public album or stays private (recommend private).
4. Domain for the press-kit/OG absolute URLs (needed before `og:image` can point anywhere real).
