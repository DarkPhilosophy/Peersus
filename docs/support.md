# Voluntary support and public gratitude

## Implemented boundary

The website and native About component link to https://ko-fi.com/DarkPhilosophy, matching the existing `.github/FUNDING.yml` handle. The owner reports Stripe is connected to Ko-fi. No payment account was inspected, altered, or tested. No Stripe SDK, secret, Checkout Session, webhook, embedded widget, analytics, cookie or payment endpoint belongs in this integration. Card and other payment methods depend on the provider and region. The link opens only after deliberate activation; merely opening About or the website does not contact Ko-fi or Stripe.

All app functions remain free. Support buys no product, service, access, support priority, in-app badge, sticker, emoji, entitlement, royalties, equity or revenue share. “Encouragement”, “A little momentum”, and “Long-term goodwill” are non-priced gratitude language, not commercial tiers. One-off support is sufficient; do not silently enroll anyone in recurring payments. Fees and tax treatment are not promised. Do not call the tip a tax-deductible charitable donation.

## Native integration

The shared About page renders `support::Support` after its security disclaimer and separately from dependency credits. The component consumes the generic `link::UrlOpener` context and calls it only on deliberate button activation. Android supplies its native ACTION_VIEW adapter through the platform crate; a failed dispatch displays a manual-link error. Desktop uses the existing toolkit URL opener when no platform callback is supplied; that API does not report browser launch success, so the interface does not claim success. No payment or account state is introduced into the core or plugin API. New strings fall back to English where a translation is unavailable.


## Public thanks operations

`site/supporters.json` is intentionally `[]`. Only after verifying a real contribution and obtaining separate, specific, revocable permission for the exact public display name may the maintainer add `{ "displayName": "approved name", "consent": true }`. This is schema documentation, not a supporter entry to publish. Keep consent evidence privately in an appropriate owner-controlled system; never add that evidence, an email, legal name by default, amount, payment ID, receipt, avatar URL, or webhook payload to the repository. Neither an ordinary tip nor a public Ko-fi comment alone implies consent to republication.

Review names for impersonation, personal-data leakage and abuse before publication. The browser creates text nodes with `textContent`; it never injects user HTML or URLs. Both staging and runtime bound the list to 200 names, 80 characters each, exclude controls/directional overrides, and require explicit consent. Static validation cannot establish real consent: that is a maintainer responsibility. Empty, unavailable, and JavaScript-disabled states are honest. Requests for acknowledgement/correction/removal go via the creator contact on Ko-fi until an owner-approved dedicated channel exists. Honor withdrawal by removing the relevant entry; explain that third-party caches and archives cannot be recalled.

## Static preview and delivery

From the project root, stage into a new, nonexistent directory:

```sh
python3 site/build.py --output /tmp/peersus-site-preview
python3 -m http.server 8080 --bind 127.0.0.1 --directory /tmp/peersus-site-preview
```

Use a different nonexistent output path on repeat runs. The script performs only local validation and copying; it does not delete/overwrite an old output, fetch dependencies, publish, or invoke a provider. Serve over HTTP rather than opening index.html as a file, because acknowledgements load via same-origin fetch. The staging artifact excludes build.py and all private project files.

`.github/workflows/pages.yml` is manual `workflow_dispatch`, contents-read only, validates/stages and uploads a reviewable artifact with 14-day retention. It has no `deploy-pages`, `configure-pages`, Pages enablement, deployment environment, Pages-write or identity-token permission. It cannot publish a site. The site is hosted by the project relay under its own name (`https://peersus.axxa.dev`, [website.md](website.md#where-the-site-is-served)); copying a staged folder there is a separate, owner-authorized step.

Before launch: owner review of privacy draft; actual retention/contact details; real distribution validation/signatures; Android store policy review; actual link/checkout availability inspection by the owner without making a payment. Do not add a made-up privacy address, screenshots, download URL or store badge to “finish” these decisions.

## Policy evidence (read 2026-11-01)

- Google Play, [Understanding Google Play’s Payments policy](https://support.google.com/googleplay/android-developer/answer/10281818?hl=en), “Do direct tips or contributions…”: the exception requires 100% of the tip/contribution to go to the creator and no digital content/services, including badges, stickers or emojis. This implementation removes digital benefits; it does **not** prove the 100% condition or guarantee Play approval. Provider fees, routing and any Ko-fi memberships/shop on the destination need owner review before Play distribution. Do not conflate an external link with blanket store-policy compliance.
- Ko-fi, [What is Ko-fi?](https://help.ko-fi.com/hc/en-us/articles/115004000994-What-is-Ko-fi): connects the creator’s PayPal/Stripe account, payments go to that account; provider fees can apply. No live account or fee plan verified here.
- GitHub, [Displaying a sponsor button](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/displaying-a-sponsor-button-in-your-repository): FUNDING configuration is discovery, not an entitlement/payment backend. Existing GitHub Sponsors handle is not proof the account accepts sponsorship; no unverified Sponsors CTA is exposed on the site.

## Parent validation checklist

Render at desktop and narrow mobile widths, in light and dark mode, with reduced motion and keyboard-only navigation. Check no horizontal overflow, readable focus and contrast, all fragment links, native details/summary, theme state and labels, install/privacy return links. On Support, inspect requests: only same-origin site assets plus supporters.json before explicit provider navigation. The separate GitHub map check runs only on Architecture. Exercise empty list, unavailable/invalid JSON, and hostile display-name text; verify no HTML interpretation or external request. Disable JavaScript: core navigation/install/support must still work. Open native About and ensure no provider request/browser until button press; activate with keyboard and parent-provided Android intent. Exercise false-return and absent-Android-opener paths and confirm the visible fallback address. Build/compile and browser checks are intentionally left to the parent assignment, not claimed here.
