# Apple requirements for Peersus (App Store, Mac App Store, Developer ID)

Read 2026-11-12, primary sources only; policy pages change often (Guidelines: "Last Updated: October 16, 2026"). **C** confirmed in quoted source, **I** inference, **U** not confirmed. P1 rejection or removal, P2 commonly flagged, P3 good practice.

## Sources (all read 2026-11-12)
S1 https://developer.apple.com/app-store/review/guidelines/ · S2 https://developer.apple.com/support/terms/apple-developer-program-license-agreement/ (DPLA) · S3 https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution · S4 https://developer.apple.com/documentation/security/hardened-runtime · S5 https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/EntitlementKeyReference/Chapters/EnablingAppSandbox.html (2017 archive) · S6 https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app · S7 https://developer.apple.com/documentation/bundleresources/describing-use-of-required-reason-api and .../privacy-manifest-files · S8 https://developer.apple.com/support/third-party-SDK-requirements/ · S9 https://developer.apple.com/app-store/app-privacy-details/ · S10 https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy · S11 https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption and https://developer.apple.com/documentation/security/complying-with-encryption-export-regulations · S12 https://developer.apple.com/help/app-store-connect/manage-app-information/provide-a-custom-license-agreement · S13 https://developer.apple.com/news/upcoming-requirements/ and https://developer.apple.com/app-store/submitting/ · S14 https://developer.apple.com/distribute/app-review/ · S15 https://developer.apple.com/programs/enroll/ · S16 https://sparkle-project.org/documentation/sandboxing/ (third party)

## Channel verdicts
- **Mac App Store:** downloaded native extensions and a self-updater cannot ship; WASM-only extensions are a risk.
- **iOS (not planned):** WASM only, no updater.
- **Developer ID:** no App Review; needs signing, hardened runtime, notarisation (S3: "Notarization of macOS software is not App Review.").
- DPLA 3.3 scopes its rules to apps "submitted to the App Store, Custom App Distribution, or TestFlight, or … Ad Hoc"; DPLA 3.2 item 7 permits macOS apps "distributed outside of the App Store using Apple Certificates and/or tickets". **I:** the executable-code rule does not textually reach Developer ID. **U:** no Apple sentence confirms it.

## 1. Downloaded and interpreted code
| Requirement, source, quote | How to check in this app | Pri |
|---|---|---|
| S1 2.4.5(iv), Mac App Store: may not "download or install standalone apps, kexts, additional code, or resources to add functionality" (C). S1 2.5.2, all stores: may not "download, install, or execute code which introduces or changes features" (C). | Desktop `process` and `native_library` artifacts: `docs/extension-store.md` (`execution` row); unpack path `crates/peersus-app/src/store_service.rs`. Ship neither in a store build. | 1 |
| S2 DPLA 3.3.1(B): interpreted code may be downloaded if it keeps the "primary purpose", does not bypass "signing, sandbox", and "does not create a store or storefront for other Applications" (C). Is an extension an "Application"? **U**. | `wasmi` interpreter, no JIT (`crates/peersus-plugin-runtime/Cargo.toml`). Keep extensions inside file transfer. | 1 |
| S1 3.2.2(i): no "interface for displaying third-party apps, extensions, or plug-ins similar to the App Store" (C). 3.2.1(ii) allows a collection "designed for a specific approved need" with "robust editorial content" (C, about apps; extensions **I**). | Store pages in `peersus-ui`: short curated list with editorial text. | 1 |
| S1 4.7: "HTML5 and JavaScript mini apps … chatbots, plug-ins" may be offered (C); WASM not named (**U**). 4.7.1: privacy, filter/report/block, 3.1. 4.7.2: no exposing "native platform APIs … without prior permission from Apple". 4.7.3: no data or permissions to software "without explicit user consent in each instance". 4.7.4: index with universal links. 4.7.5: age gating. | Host services (clipboard, files, HTTP) exposed to extensions; are grants per call or per extension? Where can users report or block an extension? | 1 |
| No Apple text says user confirmation legitimises downloads (**U**). | Describe the flow in review notes (S1 2.3.1(a)). | 2 |
| S6: a sandboxed child "must use exactly two App Sandbox entitlement keys: `com.apple.security.app-sandbox` and `com.apple.security.inherit`" (C, S5); "Apple silicon requires that all code be signed" (C). **I:** a downloaded program cannot get the inherit entitlement from you; **U:** Apple does not say so. | Helper beside the executable (`docs/architecture.md` line 50, `crates/peersus-plugin-runtime/native/host.c`): sign with those two keys. | 1 (MAS) |
| S4: Hardened Runtime "disallow[s] … just-in-time (JIT) compilation" (C). S3: plug-ins "inherit the entitlements of the host process" (C). | Entitlements: no `cs.allow-jit`; `cs.disable-library-validation` only for downloaded dylibs, so skip `native_library` on macOS. | 2 |
| S1 2.4.5(iii): may not "spawn processes that continue to run without consent after a user has quit" (C). | Extension helpers and control socket must end at quit. | 2 |

## 2. Self-update
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S1 2.4.5(vii), Mac App Store: "must use the Mac App Store to distribute updates; other update mechanisms are not allowed" (C). Notify-only checker: **U**. iOS: S1 2.5.2 (C). | Compile out the updater as the Play flavour does: `crates/peersus-app/src/update_install.rs`, `docs/app-update.md` (macOS "cannot replace itself yet"). | 1 |
| S3, Developer ID: software "distributed with Developer ID must be notarized" (C). **I:** each update archive needs Developer ID signing, hardened runtime, timestamp, notarisation, stapling. S16: "Sparkle 1 does not support sandboxed applications" (C). | The signed release catalog is separate from Apple signing. The repo has no `.app`, signing or notarisation (`docs/macos-evaluation.md`). | 2 |

## 3. Privacy
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S1 5.1.1(i): "a link to their privacy policy in the App Store Connect metadata field and within the app in an easily accessible manner" (C). | `site/privacy.html` exists. A grep of `crates/peersus-ui/src`, `peersus-app/src`, `peersus-ui-android/src` for "privacy policy" and "privacy.html" found no in-app link. | 1 |
| S9: privacy details "required to submit new apps and app updates", including "third-party partners"; "collect" means off-device and kept longer than the request needs (C). | Relay logs, catalog host, update check, IP addresses (S9 lists IP). | 1 |
| S7: manifest data types apply "on all platforms"; required-reason APIs apply to "iOS, iPadOS, tvOS, visionOS, or watchOS" (C). S8: SDKs "that repackage" listed ones, including BoringSSL, need a manifest (C). | iOS needs `PrivacyInfo.xcprivacy`. `ring`, `aws-lc-rs` are in `Cargo.lock`; whether crates count as SDKs and which Rust std calls need reasons: **U** (use Xcode's privacy report). | 2 iOS / 3 |
| S1 5.1.2(i): "explicit permission … via the App Tracking Transparency APIs to track" (C). | `peersus-diagnostics` is local; confirm no analytics SDK (**U**). | 3 |
| S10: "Any app that uses the local network, directly or indirectly, should include this description"; outgoing TCP to local addresses needs access; macOS 15+ prompts; a helper's access is attributed to the app (C). | Direct peer addresses: `crates/peersus-net/src/peers.rs`. Set `NSLocalNetworkUsageDescription`; `NSBonjourServices` only for Bonjour (no mDNS crate among those I searched in `Cargo.lock`). Nearby is Bluetooth LE and absent on macOS (`docs/nearby-send.md`); Bluetooth purpose strings **U**. | 2 |
| S1 5.1.1(iii): "use the out-of-process picker or a share sheet rather than requesting full access" (C). | S5 keys: `files.user-selected.read-write`, `files.downloads.read-write`, `bookmarks.app-scope`. Data is in `~/.local/share/peersus`; a sandbox forces the container. Clipboard and keychain entitlements: **U**. | 2 |

## 4. Legal and licences
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S2: "If Your Application … includes any FOSS, You agree to comply with all applicable FOSS licensing terms"; licence terms must not require disclosing Apple signing or DRM keys (C). | `NOTICE`, `crates/peersus-ui/src/about.rs` (notices; test crediting each direct dependency), `crates/peersus-ui/assets/IconPark-LICENSE`. Audit the whole tree (Skia, vendored crates) for GPL, LGPL, MPL; no cargo-deny or about config found (**U**). | 1 |
| No Apple rule requires an in-app notices screen (**U**). The duty comes from each licence (Apache-2.0 §4, MIT, BSD). | Keep notices in About. Shizuku (MIT) is Android only. | 2 |
| S11: encryption "limited to … the Apple operating system" needs no document; an "industry standard algorithm, not provided within the Apple operating system" needs a "French encryption declaration" (only for France) (C). A year-end US self-classification report may apply (C). | `rustls`, `ring`, `aws-lc-rs` in `Cargo.lock` are not OS crypto. **I:** `ITSAppUsesNonExemptEncryption` likely `YES`. ECCN/ERN values: **U**. | 1 |
| S12: "If you don't provide a custom EULA, the standard EULA is applied" (C). S1 2.4.5(vi): may not "present a license screen at launch, require license keys" (C). | Keep the standard EULA (Apache-2.0 in `Cargo.toml` is the source licence). The planned extension licence overlay is at install, not launch; confirm no first-run acceptance. | 2 |
| S1 1.2: needs "filtering objectionable material", "a mechanism to report offensive content", "the ability to block abusive users", "published contact information" (C). Extensions are developer code, so **I** 1.2 does not apply; 4.7.1 asks the same of plug-ins. | Report/block path for extensions: unknown, check `peersus-ui`. Contact info (1.5). | 2 |
| S1 2.3.10: no "names, icons, or imagery of other mobile platforms" in app or metadata (C). 5.2.1: submit as the IP rights holder (C). | Store listing, screenshots (`shots/` shows Android), site copy. | 2 |

## 5. Design and functionality
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S1 2.1(a): "We will reject … binaries that crash" (C). | `docs/macos-evaluation.md`: aborts at `freya-winit … metal.rs:66` with no Metal device and no fallback; a VM reviewer may hit it. | 1 |
| S1 2.1(a): demo account or "fully-featured demo mode"; "other hardware or resources that might be needed" (C). | Pairing needs a second device: prepare a demo video, keep relays live, document the extension flow. | 1 |
| S1 3.1.1(a): outside the US storefront, apps "may not include buttons, external links, or other calls to action that direct customers to purchasing mechanisms other than in-app purchase". 3.1.1 allows IAP currencies to "tip the developer". 3.2.1(vii): a gift needs "100% of the funds go to the receiver" (C). | `crates/peersus-ui/src/support.rs` (`KO_FI_URL`, About button), `site/index.html`. Fees make "100%" doubtful (**I**). Store builds: remove it, replace with an IAP tip, or keep only the website link. Developer ID unaffected. | 1 |
| S1 2.4.5(i)(ii): "appropriately sandboxed"; "packaged and submitted using technologies provided in Xcode"; no code "in shared locations" (C). S13: remove `com.apple.quarantine` before upload (C). | No `.app` or Info.plist exists yet. | 1 |
| S1 4.2: "elevate it beyond a repackaged website" (C). S14 lists "Substandard user interface" (C). | Not a web wrapper; risk is the custom renderer and frameless window with drawn controls (`docs/macos-evaluation.md`). | 2 |
| S1 2.5.1 and DPLA 3.3.1(A): "only use public APIs" (C). | Grep of `vendor/freya-skia-bindings/src` and `vendor/freya-core` for common private symbols found none (not proof); `vendor/winit` uses objc2 AppKit. | 2 |
| S5: `network.client`, `network.server` (QUIC dial and listen) (C). S3: `get-task-allow` must not be true when notarising (C). | `cs.*` entitlements only if proven needed; temporary exceptions need justification (**U**). | 2 |
| S1 2.3 and S13: accurate metadata; answer the updated age-rating questions (C). | Do not claim nearby or Android features on macOS. | 2 |

## 6. Technical
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S13: "built with Xcode 26 or later using an SDK for iOS 26 …" since 2026-04-28; Submitting page: Xcode 27, SDK 27 from April 2027. macOS not named (**U**). iOS targets iOS 13 or 15. | Toolchain and CI: all workflows run on Ubuntu. | 1 |
| S3, Developer ID: Developer ID certificate, Hardened Runtime, secure timestamp, no `get-task-allow`, then staple; use `notarytool` (C). "Your Account Holder must sign the app" (C). | `host.c` step fails on Apple flags (`docs/macos-evaluation.md`). Sign every executable, helpers included. | 1 |
| S6 shows a universal `x86_64 arm64` build; no page I read requires one (**U**). | Guest was x86-64 only; nothing ran on Apple Silicon. | 3 |
| S14: "at least 50% of submissions in less than 24 hours and 90% in less than 48 hours"; S3: notarisation "usually takes less than an hour"; export documents about two business days (S11). | Plan for a first rejection. | 3 |

## 7. Developer programme
| Requirement, source, quote | How to check | Pri |
|---|---|---|
| S15: "99 USD per membership year"; fee waiver for nonprofits. Individual: legal name "will be displayed as the seller name". S13: apps without EU trader status "will be removed" in the EU (C). | A personal name and contact details become public. | 2 |
| S15 organisation: legal entity, no DBAs, "must have a D-U-N-S Number", public website, work-domain email, signer with legal authority (C). | A project without a legal entity cannot enrol as an organisation. | 2 |

## Look at these first
`docs/extension-store.md`, `docs/architecture.md` (line 50), `crates/peersus-plugin-runtime/`, `docs/app-update.md`, `crates/peersus-app/src/update_install.rs`, `crates/peersus-ui/src/about.rs`, `support.rs`, `NOTICE`, `site/privacy.html`, `docs/nearby-send.md`, `crates/peersus-net/src/peers.rs`, `Cargo.lock`, `docs/macos-evaluation.md`.

## Unconfirmed
1. Whether a user-confirmed WASM catalog passes review; whether an extension is an "Application" in DPLA 3.3.1(B)(c).
2. Whether DPLA 3.3.1(B) reaches Developer ID builds.
3. Apple wording on a notify-only update checker in a store build.
4. Sandbox behaviour for a downloaded, unsigned child process.
5. macOS minimum Xcode/SDK for uploads; universal binary requirement.
6. Required-reason categories against Rust std; whether crates count as SDKs.
7. Clipboard, keychain, Bluetooth entitlements and purpose strings; temporary-exception review rules.
8. ECCN/ERN values; the exact `ITSAppUsesNonExemptEncryption` answer.
9. Whether Ko-fi tipping outside the US counts as a purchasing mechanism.
10. Any Apple rule on third-party notice screens.
