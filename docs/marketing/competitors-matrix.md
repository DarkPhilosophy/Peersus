# Competitor matrix — one-page comparison

Purpose: quick-scan matrix of file-transfer tools vs Peersus; details and per-claim sources in `competitors.md`.
Researched on 2026-11-12.

Legend: ✅ yes/shipped · ⚠️ partial/limited/unconfirmed · ❌ no · — n/a · ★ = GitHub stars (api.github.com, 2026-11-12)

| Tool | Platforms (L/W/M/A/iOS/Web) | Internet send (beyond LAN) | E2E encrypted | Account-free | Remembers paired devices | Open source | Price / cap | Resume | No-install receiver | Extensions | Adoption |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Peersus** | ✅/❌/❌/✅/❌/❌ | ✅ QUIC+relay (optional) | ✅ | ✅ | ✅ durable auth pairing | ✅ Apache-2.0 | free, no cap | ❌ | ❌ | ✅ signed store (dev keys) | unpublished, v0.1.0, No-Go today |
| LocalSend | ✅✅✅✅✅/❌ | ❌ LAN only | ⚠️ self-sign TLS, can disable | ✅ | ⚠️ favorites only | ✅ Apache-2.0 | free, no cap | ❌ (#1191) | ❌ | ❌ | 98.5k★ · 10M+ Play · 4.6★ |
| AlterSend | ✅✅✅✅✅/⚠️ recv | ✅ DHT+relay | ✅ Noise | ✅ | ✅ pair once | ✅ Apache-2.0 | free, no cap | ❌ unconf. | ✅ browser recv | ❌ | 2.2k★ · new (May 2026) |
| WeTransfer | web/all | ✅ cloud store+forward | ❌ operator holds files | ⚠️ optional | — | ❌ | free 3GB or 10 transfers/30d; ~$7–25/mo | ⚠️ retry | ✅ link | ❌ | brand leader · layoffs 75% · AI ToS row |
| PairDrop | PWA all | ✅ rooms/pairing (TURN) | ✅ WebRTC | ✅ | ✅ secrets | ✅ GPL-3.0 | free | ⚠️ | ✅ it *is* web | ❌ | 11.8k★ |
| Snapdrop | PWA all | ❌ LAN | ✅ WebRTC | ✅ | ❌ | ✅ GPL-3.0 | free | ⚠️ | ✅ | ❌ | 19.7k★ · unmaintained → LimeWire |
| Send Anywhere | ✅✅✅✅✅/✅ | ✅ P2P key or link | ⚠️ link files server-held | ✅ key flow | ⚠️ recent devices | ❌ | free+ads; links ≤10GB free / 30GB paid; key ≤50GB | ⚠️ | ✅ web receive | ❌ | ~39M installs · 4.5★ |
| AirDrop | ❌/❌/✅/❌/✅ | ⚠️ since iOS17.1 via internet | ✅ | Apple ID implicit | ✅ contacts | ❌ | free | ⚠️ | ❌ | ❌ | OS built-in |
| Quick Share | ❌/✅app/❌/✅built-in/❌ | ❌ proximity only | ✅ | Google acct for some flows | ✅ | ❌ | free | ⚠️ | ❌ | ❌ | OS built-in |
| KDE Connect | ✅✅⚠️/✅/⚠️ | ❌ LAN | ✅ TLS pairing | ✅ | ✅ pairs | ✅ GPL | free | ⚠️ | ❌ | ⚠️ plugins (own system) | 4.2k★ · 1M+ Play |
| Syncthing | ✅✅✅/⚠️fork/⚠️3p | ✅ sync | ✅ | ✅ | ✅ | ✅ MPL-2.0 | free | ✅ sync model | ❌ | ⚠️ API | 90.4k★ |
| croc | ✅✅✅/⚠️/⚠️ CLI | ✅ via relay | ✅ PAKE | ✅ | ❌ | ✅ MIT | free | ⚠️ | ❌ | ❌ | 40.9k★ |
| Magic Wormhole | ✅✅✅/⚠️/⚠️ CLI | ✅ mailbox+transit | ✅ PAKE | ✅ | ❌ | ✅ MIT | free | ⚠️ | ⚠️ libs only | ❌ | 23.1k★ |
| Taildrop | ✅CLI✅✅✅✅ | ✅ tailnet | ✅ WireGuard | ❌ Tailscale acct | ✅ own devices | ⚠️ clients open | free tier incl. | ✅ ≤1h window | ❌ | ❌ | alpha feature |
| Blip | ⚠️/✅✅✅/✅ | ✅ P2P+fast relay | ✅ mTLS claim | ✅ | ✅ | ❌ | free non-commercial; $25/mo business | ✅ flagship | ❌ | ❌ | ~4.7★ · ~7k reviews |
| Dropbox Transfer | web/all | ✅ cloud | ❌ | ❌ acct | — | ❌ | free 2GB; paid ≤250GB | ⚠️ | ✅ link | ❌ | brand |
| Smash | web/all | ✅ cloud | ⚠️ e2e optional | ✅ | — | ❌ | free unlimited size, >2GB queued; ~$6–10/mo | ⚠️ | ✅ link | ❌ | mid |
| Filemail | web/all | ✅ cloud | ❌ | ✅ basic | — | ❌ | free 5GB/7d; Personal 250GB | ✅ paid | ✅ link | ❌ | mid |
| Resilio Sync | ✅✅✅✅✅ | ✅ P2P sync | ✅ | ⚠️ licence email | ✅ | ❌ | free (all ex-Pro); business paid | ✅ | ⚠️ link send | ⚠️ API | mature · Nasuni |
| LANDrop | ✅✅✅✅✅ | ❌ LAN | ⚠️ TLS | ✅ | ❌ | ✅ BSD-3 | free | ⚠️ | ❌ | ❌ | 6.0k★ · dormant 2024-08 |
| Feem | ✅✅✅✅✅/⚠️ | ❌ offline LAN/hotspot | ⚠️ unconf. | ✅ | ⚠️ | ❌ | free; Pro ~$5/dev/yr | ⚠️ | ❌ | ❌ | thin, unconfirmed |
| Flying Carpet | ✅✅✅✅✅ | ❌ ad-hoc Wi-Fi (no net at all) | ✅ | ✅ | ❌ | ✅ GPL-3.0 | free | ⚠️ | ❌ | ❌ | 5.4k★ |
| Warpinator | ✅⚠️❌/⚠️port/❌ | ❌ LAN | ⚠️ group code | ✅ | ❌ | ✅ GPL-3.0 | free | ⚠️ | ❌ | ❌ | 1.6k★ · Mint bundled |
| ShareDrop | PWA all | ⚠️ room URLs | ✅ WebRTC | ✅ | ❌ | ✅ MIT | free | ⚠️ | ✅ | ❌ | 10.8k★ · unmaintained |

**Peersus bottom line:** unique on durable authenticated pairing + signed extension store + signed self-updater; behind on platforms (Linux+Android only today), resume, browser-receive, and every adoption metric.
