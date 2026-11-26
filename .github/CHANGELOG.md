# Changelog

Generated sections come from `docs/changes.json`. Edit that source and run `python3 scripts/gen-docs.py`; text outside the markers remains manual. Development entries do not imply a published release, passing CI, deployment or platform runtime verification.

<!-- CHANGES-START -->
## Unreleased

### Added

- Capture crashes on Linux, off by default (Settings → Diagnostics, `--crash-capture`): a separate `peersus-diag` program watches the owner and each window from outside and writes a minidump to the local diagnostics folder when one crashes; nothing is sent, and the page lists, opens and deletes the reports.
- Let an extension, with the person's permission, read the date and time on the device to the minute (`peersus.clock` `wall`, `clock.wall`) and record from the microphone into its own storage as a WAV (`peersus.audio` `record`/`finish`/`cancel`, `audio.record`; on Android the system's microphone question is asked from the open window).
- Publish signed development releases from CI on request (`.github/workflows/release.yml`): desktop programs, the Android package signed with the fixed development key, and the signed list of files.
- Let extension pages nest four levels deeper (`MAX_DEPTH` 16) and send only the style properties that differ from the default, so a conversation with threads and voice messages fits on a desktop page.
- Show Restore on the window's enlarge button while it is maximised, with marks drawn like Heresia's and names for screen readers.
- Route Back by scope: what the application opened closes first, then an extension page that takes Back (`back` page feature, `Event::Back`), then the first screen; on Android leaving takes a second Back with a short notice (Settings → Device, on by default). Escape is Back on a desktop.
- Fold each extension on the Permissions tab to its name and how many of its permissions are allowed; a press opens its switches.
- Let an extension page show this device's own picture (`Node::Avatar` with `this-device`).
- Add a searchable Architecture page to the site with five views and guided flows. CI refreshes the public data independently of site deployments; the page keeps a fallback snapshot.
- Move the desktop window bar between the top and bottom in Settings → Appearance. The choice is saved and menus open toward the page.
- Choose the renderer in Settings → Appearance and see which backend the window actually uses. Vulkan is tried first where supported, including on Android. On desktop, compare local frame times, main-thread CPU and resident memory against the current renderer using the same short synthetic scene.
- Download the person's language for this release from the repository's languages branch (the device's language by default), keep it cached, show English for whatever it does not translate, and add a Languages page to Settings that shows how far each language is translated, imports a language file, saves the template and opens the page to share a translation; extensions can ask for the person's language (`peersus.language`).
- Keep an extension's unchanged permissions across a Store update when its sound predecessor also came from the Store; a manual replacement starts with nothing allowed.
- Let an extension page follow a two-finger twist (`rotate` page feature, `Event::Rotate`), so a map can turn on a phone.
- Allow extensions to render accessible canvases in their own process: WASM uses Skia CPU in its helper, while native libraries and standalone executables supply their own finished frames. The owner relays only bounded rasters and the window composites them.
- Attach a paired desktop proportionally to a local edge while keeping its real display sizes. The pointer enters and returns at the corresponding fraction of the edge; free placements still use their original coordinates.

### Fixed

- Let desktop pages paint and receive pointer input in the gaps of the window bar, without taking away the window controls or resize grips.
- Keep Android Vulkan rendering at the current window size after rotation or resize instead of stretching pixels from a buffer with the previous dimensions.
- Release held input when either device withdraws access or the paired visit disconnects, even between data streams. A remote refusal reveals no reason and is not reported as a transfer error.
- Keep only current grants whose declaration is unchanged in a Store update or reviewed development replacement. An untrusted predecessor or forgotten pairing carries no grants forward.
- Open the desktop window from bare peersus and use peersus owner for headless or login-service starts.
- Keep catalog extensions in one stable list across installation and removal, with a green Store badge, inline enable/disable and update actions, always-visible installed-plugin configuration and actions, expandable package details and installation from file beside shared search.
- Keep direct connections first with independently enabled, reorderable relay fallbacks, retained addresses, advanced timing and credential controls, and a compatible migration to the new project relay default.
- Native helper compilation and Linux staging honor CARGO_TARGET_DIR so desktop programs and their helpers use the same build output.
- Keep one bounded previous generation of the desktop owner log without breaking inherited log handles or the window's startup error reader.
- Install Linux user executables together under lib/peersus behind one bin/peersus symlink, preserving it during in-app updates and reporting legacy files without removing them.
- Linux staging refuses to overwrite an existing distribution; regression scenarios cover default, relative and absolute target paths.
- Use the shared atomic writer for the catalog rollback floor instead of a predictable temporary file.
- Allow full-width extension action labels to wrap rather than forcing them onto one line.
- Keep all desktop header controls visible in narrow windows with flexible navigation and reduced decorative spacing.
- Let the desktop device-name field shrink so its Save button stays readable in narrow windows.
- Keep the language list inside safe window bounds, flip it above the selector when needed, and contain wheel and touch scrolling within the list.
- Include generated AppImage files, checksums and runtime notes in the release-preparation artifact.
- Draw a translucent shape of an extension's drawing in a layer the size of that shape instead of the whole drawing, cutting the window's CPU per map zoom step by about 35-40%.

### Development

- Require every extension manifest to name its execution kind. Discovery, installation and store checks refuse an omitted declaration instead of treating it as a standalone executable.
- Keep pan and zoom previews on finished canvas bitmaps only. Remove declarative subtree following and the unused scale and overscan options; keep small drawings and bounded retained strokes.
- Keep reviewed trace events in optimized desktop deploy builds and add bounded, private development-only panic and hang reports. Desktop release preparation rejects development diagnostics at compile time.
- Backport maintained netlink and title-font dependencies without disabling network monitoring or native window decorations; retain upstream licenses and patch provenance.
- Replace unmaintained token-pasting dependencies through minimal Freya and rav1e compatibility manifests while retaining image-format support.
- Migrate Linux sleep, theme, portal preferences and accessibility wire consumers to zbus 6 while retaining protocol/signature checks and original licenses; full platform runtime verification remains pending.
- Remove the unused asynchronous keyring backend's legacy wire dependencies without replacing the application's native credential stores or lookup behavior; the regenerated root lockfile passes the strict local audit without suppressions, while hosted CI and platform runtime verification remain pending.
- Keep application About, prototype security notes and dependency documentation synchronized with local security patches and their actual verification limits.
- Implement the Android upload WASM guest and explicit caller-scoped file, consent and HTTPS host services, with opaque credentials, cancellation and a manual development package; device and live-upload acceptance remain pending.
- Gate the upload guest's formatting, strict lint, protocol tests, WASM compilation and real isolated-worker exchange with synthetic host replies; retain platform-verifier licenses and pinned Android artifact provenance.
- Include the licensed upload WASM package in Android builds and initialize it through the normal removable-package installer, preserving existing packages, disabled state and later removal without automatically granting file or network access.
- Add explicit development desktop and Android builds plus signature-checked, data-preserving deployment to selected devices without implicit installation or uninstall.
- Add root build, strict-check, isolated store-demo and loopback-only website-preview shell helpers using the existing build tasks.
- Stage clearly labelled public store fixtures under store-test/ without populating or changing production trust.
- Provide a disposable desktop store owner backed by the real verifier, installer and module hosts.
- Consolidate build caches under target/ and place Android native staging, Gradle intermediates and packages there instead of source directories.
- Pin the Rust build and formatting toolchain and use its declaration in CI and release-artifact workflows.
- Include the toolchain declaration in local Flatpak source snapshots and add staging regression checks to the metadata gate.
- Generate marked README and changelog sections from workspace metadata, plugin manifests, workflow definitions and curated change entries; check reproducibility in CI.
- Wait for sender-side profile exchange completion before checking picture-transfer counters in integration tests.
- Keep translation catalog coverage across multiline Rust calls and include the remaining settings controls in that check.
- Update the withdrawn async-executor 1.14.0 dependency to patch release 1.14.1.
- Describe each application release only by its signed SHA256SUMS: the tag v&lt;version&gt;-&lt;channel&gt;.&lt;build&gt; locates it, the artifact names carry the version, channel and build, and archives list each program's digest. Development builds follow the dev channel; the first installation script and the download page read the same signed list. An installation from a release keeps that list beside its programs; the owner checks them once per start and puts a missing or damaged program back from the running build's release.

### Documentation

- Add architecture, API, development, website, contribution and security documentation.
- Add issue, pull-request and discussion templates with guidance for non-sensitive diagnostics.
- Align website build instructions with the desktop helper and Android distribution-flavor build tasks.
<!-- CHANGES-END -->
