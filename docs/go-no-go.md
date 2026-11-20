# Go/No-Go evaluation

Evaluated on 2026-11-15 against the working tree of that day; rows 6, 7, 8, 11 and 19 brought up to 2026-11-17, when that tree, with the work since, became the one commit of `main` (`6d5f04c`, E-138). Every statement below points at
the entry of the [archived engineering log](engineering-log-through-E150.md) that holds its evidence; where the log says a thing was not
looked at, this page does not claim it. This is a local evaluation, not an independent audit. The evaluation of
2026-11-11 is replaced by this one.

## Verdict

**No-Go for a public release, on every platform.**

The [current development milestone](engineering-log.md) tracks the local candidate separately. A green submission CI, a higher static-analysis score or a desktop build does not by itself establish readiness for a public release.

**Go for continued private development builds on Linux and Android**, which is the only thing the evidence covers:
the strict gate is green on Linux, an optimized Linux build is installed and used on two desktops, and development
packages were installed and observed on the phones named in the log.

The verdict does not rest on one defect. No data loss or security defect is open in the log. It rests on what was
never built, never run on a device or never decided, listed below. What changed since 2026-11-11: the audit findings
that were open are closed in code and were observed on devices (3, 4, 5, 9, 18), an optimized desktop build of the
tree the gate ran on is installed and has run as an application (1), and a class of faults that left a desktop
without its pointer was found and fixed (17).

## What blocks a release

| # | Blocker | State | Evidence |
|---|---|---|---|
| 1 | Optimized builds. The Linux release programs of the tree the last gate ran on, plus E-078 and without the keeper (E-080), are installed for one user on both desktops, byte-verified, with the three extensions of the same day; the owner controlled one desktop from the other with an earlier build of the day. Android has only the debuggable package of the tree before E-080 (its libraries are optimized; E-080 changes nothing a phone runs), on both phones; no Android release package, Flatpak or AppImage was built. | Partly done | E-058, E-060, E-061, E-074, E-076, E-080 |
| 2 | Production trust is not configured: the store's maintainer key and the application-update key are development keys with local seeds; no product build trusts either. Signing identities and store listings are the owner's. | Owner | E-029, [extension store](extension-store.md), [application updates](app-update.md) |
| 3 | Rate limit and temporary suspension of an extension that floods: built, with tests that failed first, and seen on both phones and one desktop with the installed builds: a flood of events is paused and the person told, the application and the other extensions go on. A flood that contains calls ends as a restart of that extension instead of a pause, and a paused extension that keeps writing is started again after about ten seconds on two devices of three: both bounded, neither traced to its cause. | Closed; observed on three devices | E-069, E-074 |
| 4 | The agnostic rule in the HTTPS and input services: the application names no provider's header (the extension names its own, the person is shown them) and the input arguments have one definition. The Android half is built and installed; the prompt with the header's line was seen on a phone. A file was not actually sent from a phone: that publishes at a third party under the owner's key and waits for his word. | Closed; prompt seen; a real upload from a phone not made | E-072, E-074 |
| 5 | The other audit findings (A-01 … A-13): each closed by an entry or decided, with its test. Left: a control-frame write to an extension that never reads has no time limit (bounded; nobody else waits on it), the duplicates of A-14, and the size of three files (A-16). | Closed in code; two small rows open | E-009 … E-012, E-055, E-071, E-072, audit table |
| 6 | The published gate finished complete runs on the host, green, on the one commit of `main` (`Quality gates`: compilation and automated tests on Linux, the pinned Nix shell); the artifact runs on `548b912` produced Linux, AppImage and Flatpak; Android ran out of Gradle's default 512 MiB heap packaging the APKs, and with 3 GiB produced its unsigned APKs and bundles on the next commit (E-147); five automated dependency branches are unevaluated. | Gate and every artifact green on the host; dependency branches open | E-025, E-138, E-146, E-147 |
| 7 | Android at rest. Every repeated call into Java runs on one permanently attached thread (E-128). NX, build 19 (E-135): 3.9% of a core and 72 wake-ups a second with the Send page open, 1.4% and 34 with the owner started by its service alone; name lookups 5 samples in 15 s, the permission rules none. Build 18, after hours of running, had reached 15.4% and about 500 a second (name lookups and permission rules); whether the lookups come back after hours is not yet measured. In deep sleep the application still answers nothing. | Measured; the long run open | E-020, E-021, E-027, E-128, E-135 |
| 8 | Controlling a device: measured again with release builds on both sides (E-146): about 59 batches a second reach the controlled computer, gaps of 17.2 ms at the median and none over 50 ms (E-019 had gaps of 80–130 ms); the phone's application uses about 35 % of a core while a finger drags the pad, Control's worker about 7 %, the controlled computer's owner about 3 %. The time from the touch to the pointer is not measured (no way to stamp the touch on the phone). No single cause of the phone's cost; each remedy changes something visible. | Measured, owner's decision | E-019, E-023, E-146 |
| 9 | The stress run of a misbehaving extension on devices, with the rate limit: made on NX, ROG and one desktop (seven ways of misbehaving each); the application's process, its control channel, the other extensions and the paired devices were unaffected throughout. Not made on HP; no transfer was in progress meanwhile. | Done on three of four devices | E-028, E-074 |
| 10 | ROG carries the application and the three extensions of the gate's tree; the stress run and the presence of the other devices were read there through the application's control channel. Nobody who built it looked at its screen. Its owner's own trials are recorded where they were said. | Installed; driven, its screen not looked at | E-065, E-066, E-038, E-074 |
| 11 | Windows: builds in a guest; the library tests of the core and the window pass (543 and 667, 2026-11-17); two transfer tests of 32 and more files run out of their ten seconds on the guest, whose disk takes 61 ms to flush a small file (measured); every program carries its version resource; the extension host and the interpreter run real extensions there (their tests and the real-connection tests pass on `548b912`, E-147); the window was seen on the guest's console (software renderer), with the Test extension's page answering a press, its switch and typed text; the application cannot update itself there; no installer. macOS: builds in a guest (14.8.9, x86-64), `cargo xtask build` included; the native helper builds there now, and the host, interpreter, plugin, store and real-connection tests pass (E-147); each helper carries its own identity (embedded `Info.plist`); the window was seen there only with the software renderer, and as a staged `Peersus.app` (ad hoc seal, not notarized); replaying a controlling device's input is written and compiled, not run on a Mac; pairing with the guest was not achieved; nothing ran on a real Mac or on Apple Silicon. Neither is a supported platform today. | Open / unsupported | E-024, E-044, E-046, E-047, E-067, E-136, E-147, [macOS evaluation](macos-evaluation.md) |
| 12 | Application updates: the Play path cannot be observed without a listing; refusals on Android without a provider, and a rollback against a real release, were not observed. | Not observable / not done | E-029 |
| 13 | Screenshot environment hooks are read by product builds; the submission validator has a key that cannot be satisfied. | Open | E-006, E-004 |
| 14 | Devices nearby (experimental, a switch that is off by default): sending between two phones was not seen by who built it; what makes files go rests on two announcements nobody authenticates (a stated risk); no battery cost was measured; a device that is not paired was seen only before the last change of the rule. It cannot be on by default, and whether it is shipped at all behind its switch is the owner's. | Experimental, owner's decision | E-032, E-035, E-038, E-039, [devices nearby](nearby-send.md) |
| 15 | Store submission (read from primary sources on 2026-11-12, none of it acted on): Google Play wants a disclosure, consent and declaration for the accessibility service in the shared manifest (or it moves to the community flavour), an in-app link to a published privacy policy (`site/privacy.html` is a draft; Apple 5.1.1(i) wants the same), the full texts of the BSD/MIT licences of what is bundled and Shizuku credited; Apple's Mac App Store cannot carry native extensions or a self-updater, a catalog of third-party extensions risks guideline 3.2.2(i), the Ko-fi button is an outside payment link (3.1.1), the program aborts without Metal (2.1), encryption export documents are probably due, and no signing or notarisation exists. Whether Google accepts WASM as an interpreter and whether Apple reaches Developer ID builds with its code rule are unconfirmed. | Open, owner's decisions | E-043, [Google Play](store-policy-google-play.md), [Apple](store-policy-apple.md) |
| 16 | Licence and notice of extensions: built, tested and seen on a desktop window; not seen on a phone, on a light theme, or with a very long agreement; extensions installed before it keep running and are never "accepted". | Built, partly seen | E-043 |
| 17 | A desktop left without its pointer or unreachable: a cleaned temporary folder, a late failure of forwarded input, a controller gone silent, a pointer held on an edge. Each fixed with its test and in the installed desktop build; two keys always bring the pointer back; a window brings back an owner that ended. An owner that dies while no window is open is started again by nothing until a window is opened: a process that kept it was built and taken out the same day, by the owner's word (E-080). A press held for more than thirty seconds no longer ends the visit (E-078): in the installed build, not tried. Not confirmed by the owner's hand for every case. | Fixed; observed except where said | E-049, E-050, E-054, E-056, E-057, E-058, E-078, E-080 |
| 18 | A device that comes back (a phone that slept, an application started again) was shown offline for a time by devices that had its old session. The rule is changed, tested in the transport, installed on all four devices and seen: a phone whose application was stopped and started at once showed the three others online after 4.4 s (about 100 s before), and none of them showed it offline. A desktop that sleeps and wakes was not tried. | Closed; observed with a phone | E-070, E-074 |
| 19 | The website: the project's own (not modelled on any other product's site), served by the project relay under its own name from the staged folder, on the server; pages, the store from the signed approvals and the relay's endpoints checked over HTTPS, plain HTTP sent to HTTPS. The store lists only Test until the other extensions are accepted; the privacy page is a draft the owner must approve (it names the host and a contact). | Deployed; the privacy text and the other extensions open | E-133, E-137 |

## What is in place

- The strict gate (formatting, metadata and script tests, `check`, `clippy -D warnings`, tests, the example guest,
  the dependency audit, no module cycle) is green on Linux; see "Gate status" in the log for the last run, its
  numbers and what was changed after it.
- An optimized Linux build of the tree the gate ran on is installed on two desktops (E-074), and earlier builds of
  the day were used between them (E-058, E-060, E-061); development packages of the same tree run on both phones
  (E-074), and later work was observed on NX (E-019 … E-038, E-066).
- What the Send page does when files are handed over is driven by a test on a real owner with no display, and that test
  fails on the defect it was written for (E-039).
- The store and automatic updates of extensions and of the application work end to end with development keys, on
  Linux and on one phone (E-029).
- One extension cannot cost another its events, hold every consent prompt, make devices without end, or flood: each
  has a bound and a test (E-009, E-069, E-071). A misbehaving extension did not stop, slow or disconnect the
  application on the two phones and the desktop it was tried on, with the installed builds (E-074), and one that
  keeps failing is turned off with a fixed message (E-028).
- The application names no extension, provider or service of an extension in common code; the two exceptions the
  audit found are gone (E-011, E-072).
- Two documents are generated from the source and checked by the gate (E-068).

## Not part of this evaluation

Key bindings, asked for on 2026-11-15 as an extra, are a design only (E-073). Nothing of them is built, and no
release waits for them.

## Before the verdict can change

1. Decide the owner's items in "Decisions for the owner" of the log, and blockers 2, 8, 14 and 15 above.
2. Build today's tree as release artifacts of every platform to be shipped and run each as an application, seen by
   a person (1); make the stress run on HP and with a transfer in progress (9); look at ROG's screen (10).
3. Let the hosted gate finish green (6).
4. State Windows and macOS as unsupported in what is published, or do the work in 11.
