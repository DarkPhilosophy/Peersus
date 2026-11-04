# Peersus: positioning and launch plan

## Positioning

**Your files. Your other device.** Native, consent-first file transfers without a product account or another cloud folder. “Move it. Don’t store it.” is a tagline about the core handoff, not a claim that receiving devices, history, relay metadata or optional cloud upload store nothing.

Lead with the Linux + Android everyday handoff: choose a file, pair the intended devices, explicitly approve receipt. Explain encrypted direct/relayed transport plainly. The site uses the real application mark and warm-paper/deep-green palette, orange outgoing and blue incoming cues. Its connection diagram is explicitly an illustration, not an invented application screenshot or measured transfer result.

Do not market “military-grade”, “100% secure”, independently audited, maximum Wi-Fi speed, guaranteed offline/local-only, universal platform availability, working installers or invisible Android background clipboard sync. Do not advertise unreleased plugins as shipping functionality. No fabricated testimonials, supporters, counters, ratings or press logos. No claims of being faster or safer than competitors without comparable measurements.

## Evidence, not market-size fiction

Primary sources inspected 2026-11-01:

1. [LocalSend’s own website](https://localsend.org/) leads with free, cross-platform, local-network sharing, no account and no tracking. These are established category expectations, not distinctive proof for Peersus. LocalSend’s public site is a competitor’s self-description, not an independent benchmark; its counters are not used as market statistics here.
2. [LocalSend issue #1191](https://github.com/localsend/localsend/issues/1191) requests interrupted-transfer resume, with individual comments describing large-transfer pain. This is anecdotal evidence of a task to investigate, not prevalence, a measured failure rate, or proof Peersus already solves it. Test interruption, reconnect and OS lifecycle before considering resume a differentiator.
3. [LocalSend issue #187](https://github.com/localsend/localsend/issues/187) requests automatic clipboard sharing and was closed as a duplicate of #163. The historical maintainer comment notes Android background limitations. This is a user request, not proof of current competitor capability or a current Android API guarantee. Prefer explicit clipboard consent and truthful focus restrictions over an automatic-everywhere promise.

Recommendations inferred from those sources: show a real end-to-end Linux/Android handoff, document failure recovery, make consent visible, and publish a platform-specific clipboard table only after validating it. No inferred demand numbers, TAM or unsupported superiority claims.

## Audience and messages

- Linux users moving photos/documents to an Android phone: demonstrate one clean handoff and say exactly where it saves.
- Privacy-conscious users: describe endpoint encryption, relay metadata and plugin boundaries; acknowledge no independent audit.
- Rust/open-source contributors: publish architecture/build prerequisites and scoped reproducible issues after the repository is public; do not confuse implementation language with a performance result.

### Ready-to-edit launch copy

**Short description:** “Peersus is a native, consent-first file-transfer project for your own devices. No product account, ads or tracking. Linux and Android have been exercised in development; this is pre-release software.”

**Longer announcement:** “We’re building Peersus for the small handoffs between your computer and phone. Pair the devices, choose a file, and approve the incoming offer. Core transfers use end-to-end encrypted connections, with optional relays when a direct route is unavailable. Relays can see connection metadata, not the file payload. Linux and Android are our tested development targets so far; Windows/macOS and optional clipboard/cloud plugins need further validation. It’s free software, with voluntary tips and no paid features. We’d value precise feedback on setup, interruption handling and consent clarity.”

This is copy, not a posted announcement. Add real public installation/repository destinations only when approved and working. Do not post it before a usable public artifact or source path exists.

## Launch sequence and channels

1. **Readiness first:** parent validates desktop/mobile site, app, transfers, permissions and distribution. Owner approves privacy/contact/host and real public source/release URLs. Never fill missing release information with invented downloads. Add only actual, redacted and authorized screenshots with a useful caption.
2. **Small opt-in preview:** invite people who ask to try it. Give exact OS/build versions, known limitations and a reproducible feedback template. Ask about successful first transfer and unclear decisions, not donations. Do not harvest emails, scrape users, bulk-message or import third-party community identities.
3. **Public developer release:** once the owner separately authorizes publishing, use the actual repository release notes and a focused Rust/Linux community post where project promotion is allowed. Disclose authorship and pre-release status. Respect community rules; never promote in competitor bug reports or send unsolicited DMs. A Show HN-style technical demonstration can follow a real usable release, not a conceptual landing page.
4. **Practical walkthrough:** a short owner-approved Linux → Android recording showing pairing, consent and saved-file verification. Include captions and a non-video text alternative. No simulated progress or fictional transfer speed.
5. **Feedback loop:** prioritize setup failures, lost progress, wrong-device risk, permission ambiguity and actual repeat-use stories. Publish changes with concrete before/after evidence. Request independent security review when feasible; never pre-label the project audited.

These are recommendations, not publication authorization or executed outreach.

## Useful measurements without surveillance

No analytics/event pixels or tracking identifiers are implemented. Use opt-in reports and reproducible lab sessions: time to first accepted transfer, setup errors, interrupted-transfer outcome, file-integrity results, permission comprehension and accessibility failures. Compare exact builds, devices, file sizes and network conditions; report sample size and unsuccessful runs. Public release/download counts, if later available, are coarse distribution signals, not users or retention. Optional support is not an engagement metric and must not optimize into coercion.

## Support tone

One quiet support section and one About block, no startup prompt, no transfer-success nag, no attention-grabbing popup. Ko-fi is external and loaded only by choice. Gratitude choices have no fixed prices, access or benefits. Public acknowledgement is separate consent, initially empty; see [support operations](support.md). Payment funding is not a substitute for a credible release, privacy information, or reliable software.
