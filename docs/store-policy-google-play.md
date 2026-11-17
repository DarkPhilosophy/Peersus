# Google Play requirements for Peersus (Play flavour, `ro.peersus`)

Sources read 2026-11-12; Play Help pages are policy and change often. Priority: 1 = rejection/removal, 2 = commonly flagged, 3 = practice. Repo facts come from files I read.

**Play vs community.** Play carries WASM extensions (`wasmi`, isolated `:wasm` service) and Play's update API. Native `.so`/process extensions, Shizuku and shell self-update are community-only. The repo enforces this: `android/app/build.gradle:74-91,139-159`, `scripts/inspect-android.py:35-41`, `plugins.rs:76-80` (`native-extensions` feature). **Gap:** `AndroidManifest.xml` is shared, so everything in it ships on Play.

## A. Code, updates, install
| # | Pri | Requirement, source | Decisive quote | How to check in this app |
|---|---|---|---|---|
| A1 | 1 | No downloaded executable code except VM/interpreter. Device and Network Abuse, https://support.google.com/googleplay/android-developer/answer/16559646 | "an app may not download executable code (such as dex, JAR, .so files) from a source other than Google Play." Exempt: "code that runs in a virtual machine or an interpreter" | Final Play AAB has no `libpeersus_native_android.so`, `NativeSlots`, Shizuku (`inspect-android.py` raises). What Android does with a store package of kind `native_library`/`process`: unknown, read `plugins.rs:76-80`. |
| A2 | 1 | Same page: runtime-loaded interpreted code | "interpreted languages … loaded at run time … must not allow potential violations of Google Play policies." | Audit host services a WASM extension can call: `ExtensionClipboard.java`, `ExtensionFiles.java`, `ExtensionAuthorization.java`, `docs/api/service-messages.md`. |
| A3 | 1 | Deceptive Behavior §5, https://support.google.com/googleplay/android-developer/answer/9888077 (now /17006354) | "Don't dynamically download and execute code from a remote source that introduces features or functionality not present during the app review." | Extensions add features after review. Disclose in listing and reviewer notes; keep confirm-before-install; keep the host API the only capability. [INFERENCE] WASM is not named. |
| A4 | 1 | Self-update, A1 page | "may not modify, replace, or update itself using any method other than Google Play's update mechanism." | **Confirmed disabled on Play:** `update.rs:25-34` returns Err when `update_managed()==Some(true)`; `app_update.rs:488-491,533-539,553-554,623` refuse check/download/install, so no `app-release-*` fetch (`StoreDownload.java:196-200`). Play `AppUpdate.java` only calls `AppUpdateManager`. No `REQUEST_INSTALL_PACKAGES` in the main manifest. |
| A5 | 2 | Install permission, https://support.google.com/googleplay/android-developer/answer/16558241 | "Don't request this permission … P2P must be the primary purpose of the app" (file sharing/transfer is a listed use) | Keep it out of the merged `playRelease` manifest. |
| A6 | 2 | Hostile downloaders, https://support.google.com/googleplay/android-developer/answer/9888380 | File-sharing apps ok if "They don't drive downloads without user interaction" | Extension auto-update switch (`docs/app-update.md:59`): default off, resets permissions. |

## B. Permissions and special access (policy …/answer/16558241)
| # | Pri | Item | Quote | Check |
|---|---|---|---|---|
| B1 | **1** | **AccessibilityService** `RemoteInput` (`AndroidManifest.xml:66-73`; `remote_input.xml`: `canPerformGestures`, `canRetrieveWindowContent`, no `isAccessibilityTool`) lets a paired device tap, swipe, type. | Tool eligibility: "Only services that are designed to help people with disabilities access their device" (https://support.google.com/googleplay/android-developer/answer/10964491). Others need an in-app disclosure, affirmative consent, listing text, Console declaration. | Only the `remote_input_description` string exists; it "cannot be substituted" for the disclosure. If kept on Play: consent screen before settings handoff, say what window content is read, listing text, declaration plus video. Malware "Backdoors" ("remote-controlled operations") may be cited. Owner decision: declare or move to community. Play `InputRecoveryProvider` only opens settings. |
| B2 | 1 | FGS `connectedDevice`, `TransferService` (`AndroidManifest.xml:6-7,53-54`) | "is initiated by the user or is user perceptible"; "can be terminated or stopped by the user" (…/answer/16559646). Video must show "pairing and connecting to an external device directly inside the app" (…/answer/13392821) | Risk: "background mode" keeps it always on (manifest lines 5-6). Check stop control, notification, `BootStart` only after opt-in. Prepare declaration and video. |
| B3 | 2 | `SYSTEM_ALERT_WINDOW` (`OfferPanel.java:158-159`) | "Direct users to the system settings page for approval of special permissions (for example, `SYSTEM_ALERT_WINDOW`)." | `MainActivity.java:335-338` does. `TransferNotifications.java:66-69` opens it from a notification path: check a user action precedes it. Panel must not mimic OS warnings (Deceptive Behavior). |
| B4 | 2 | Camera, notifications, BLE `neverForLocation` (`:15-25`) | "Use a runtime permissions request whenever available" (User Data) | Camera asked on opening Receive. Check the notification prompt. No location permission (grep). |
| B5 | 3 | Keep absent: `MANAGE_EXTERNAL_STORAGE`, `QUERY_ALL_PACKAGES`, battery exemption, background location | "P2P sharing" invalid for QUERY_ALL_PACKAGES (…/answer/10158779) | Grep found none; re-check merged manifest. Local Network permission needs a declaration from 2027-08-25 at targetSdk 37. |

## C. Data, privacy, declarations
| # | Pri | Requirement | Quote | Check |
|---|---|---|---|---|
| C1 | 1 | Privacy policy in Console and in-app, https://support.google.com/googleplay/android-developer/answer/10144311 | "Apps that do not access any personal and sensitive user data must still submit a privacy policy." Public non-geofenced URL, no PDF. | `site/privacy.html` is a draft (`docs/release-readiness.md:75`). Name developer and contact. In-app link: unknown, grep UI. |
| C2 | 1 | Data safety form, also for closed testing, https://support.google.com/googleplay/android-developer/answer/10787469 | End-to-end encrypted data "unreadable by you or anyone other than the sender and recipient … does not need to be disclosed." | Relays forward ciphertext (`docs/architecture.md`). Decide for diagnostics (`crates/peersus-diagnostics`), GitHub store/update fetches, BLE announcement, relay token/IP: unknown, check what leaves the device. Declare SDKs (Play app-update, rustls-platform-verifier). |
| C3 | 3 | Account deletion applies if "users … create an account from within your app" (C1 page) | same | No account (owner statement). Document identity reset/uninstall for the form's deletion question. |
| C4 | 2 | Target audience, ads, app access, https://support.google.com/googleplay/android-developer/answer/9859455 | "You must declare your app's target age group." | Not for children; no ads SDK in `android/app/build.gradle`. App access: give reviewers steps (pairing needs a second device; pre-launch report cannot pair). |

## D. Third-party extensions (UGC https://support.google.com/googleplay/android-developer/answer/9876937; Impersonation …/answer/9888374)
| # | Pri | Point | Quote | Check |
|---|---|---|---|---|
| D1 | 2 | UGC applicability | UGC is "content that users contribute to an app, and which is visible to … users." | [INFERENCE] a signed, PR-reviewed catalog is curated, not in-app UGC, but extension-written notices show in-app. If UGC: terms before upload plus in-app report/block. No in-app report path in docs: unknown, check Extensions UI. |
| D2 | 2 | Takedown and review | n/a | Maintainer plus independent approval (`docs/extension-store.md:113-114`). Add a documented abuse channel and removal timeline. |
| D3 | 3 | Impersonation | "Don't imply that your app is related to or authorized by someone that it isn't." | Overlay must not look like host/OS dialogs (fixed size, host-owned buttons). |

## E. Technical and account
| # | Pri | Requirement | Quote | Check |
|---|---|---|---|---|
| E1 | 1 | Target API, https://support.google.com/googleplay/android-developer/answer/11926878 | "must target Android 16 (API level 36) or higher" from 2026-08-31 | `targetSdk=36` ✓ (`build.gradle:14`). |
| E2 | 1 | 16 KB pages, https://developer.android.com/guide/practices/page-sizes | "Starting February 1, 2027, if your app updates don't support 16 KB memory page sizes, you won't be able to release these updates." | Every `.so` (Skia, wasmi) ELF align ≥ 2**14; `useLegacyPackaging=false` ✓. `release-readiness.md:110` says checked once: re-run `inspect-android.py` on the final bundle. |
| E3 | 1 | AAB and Play App Signing, https://developer.android.com/guide/app-bundle | "From August 2021, new apps are required to publish with the Android App Bundle" | Release is deliberately unsigned (`build.gradle:58`): need `bundlePlayRelease` with an upload key. `debuggable=false` ✓ (`:53`). |
| E4 | 2 | New personal accounts, https://support.google.com/googleplay/android-developer/answer/14151465 | "closed test … minimum of 12 testers … opted in continuously for at least 14 days." | Personal accounts created after 2023-11-13. Organisations need a D-U-N-S number (…/answer/13634885). Review "up to seven days or longer" (…/answer/9859751). |

## F. Outside Play (community)
| # | Pri | Point | Quote | Check |
|---|---|---|---|---|
| F1 | 2 | No Play rule bans other distribution; developer verification is the constraint, https://developer.android.com/developer-verification/guides/faq | "If you distribute your app through other stores, or if users sideload your app directly, these new verification requirements won't apply to your app yet." Global rollout 2027. | Register `ro.peersus` and keys. Both flavours share one `applicationId` (no suffix in `build.gradle`), so a project-signed community APK cannot update a Play-signed install. Decide id/key plan. |
| F2 | 2 | Play build must not point to non-compliant code (A1 page) | "Apps that link to or facilitate the distribution or installation of malicious software." | Grep Play strings for the community download URL and Shizuku steps. |

## G. Licences (no Play-specific screen requirement found)
| # | Pri | Point | Quote | Check |
|---|---|---|---|---|
| G1 | 3 | Duty is the developer's, https://developers.google.com/android/guides/opensource | "you as a developer are responsible for appropriately displaying the notices for the open source libraries that your app uses." | About screen exists (`crates/peersus-ui/src/about.rs`, `NOTICE`). |
| G2 | 2 | Apache-2.0 §4, https://www.apache.org/licenses/LICENSE-2.0.txt | "You must give any other recipients … a copy of this License"; NOTICE "within a display generated by the Derivative Works" | `NOTICE` shown ✓ (`about.rs:37`). Check licence texts, not names, for ring, wasm-tools, IconPark. |
| G3 | 2 | BSD-3/MIT, https://opensource.org/license/bsd-3-clause | "Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer" | `about.rs:309`: Skia's full texts "are being added". Open. |
| G4 | 2 | MPL-2.0 `attohttpc`, https://www.mozilla.org/en-US/MPL/2.0/ | "You must inform recipients of the Executable Form how they can obtain a copy of such Source Code Form" | `about.rs:307` says unchanged, links repo. Add exact version. |
| G5 | 2 | LGPL-3.0 §4(d), https://www.gnu.org/licenses/lgpl-3.0.txt | "recombine or relink the Application with a modified version" | No LGPL/GPL in `about.rs`; run cargo-deny on the Android graph. Static link in the Rust `.so` means relink material is owed. FreeType used under FTL ✓. Shizuku (MIT, community only) is not credited. |

## Open first
`android/app/src/main/AndroidManifest.xml`; `res/xml/remote_input.xml`; `android/app/build.gradle`; `src/play/java/dev/peersus/app/AppUpdate.java`; `crates/peersus-ui-android/src/{update.rs,plugins.rs}`; `crates/peersus-app/src/app_update.rs:488-554`; `scripts/inspect-android.py`; `crates/peersus-ui/src/about.rs`; `NOTICE`; `site/privacy.html`; `docs/release-readiness.md:74-79`.

## Unconfirmed
- Whether Google accepts WASM as an "interpreter" (policy names JavaScript, Python, Lua).
- Whether `SYSTEM_ALERT_WINDOW`/`RECEIVE_BOOT_COMPLETED` need a Console declaration.
- Play 64-bit rule (not read; ABIs are arm64-v8a, x86_64); pre-launch report details; Play EULA/ToS practice (no Play field found).
- Exact data leaving the device; any in-app privacy link or extension report path.
