# Changelog

Generated sections come from `docs/changes.json`. Edit that source and run `python3 scripts/gen-docs.py`; text outside the markers remains manual. Development entries do not imply a published release, passing CI, deployment or platform runtime verification.

<!-- CHANGES-START -->
## Unreleased

### Fixed

- Keep catalog extensions in one stable list across installation and removal, with a green Store badge, inline enable/disable and update actions, always-visible installed-plugin configuration and actions, expandable package details and installation from file beside shared search.
- Keep direct connections first with independently enabled, reorderable relay fallbacks, retained addresses, advanced timing and credential controls, and a compatible migration to the new project relay default.
- Native helper compilation and Linux staging honor CARGO_TARGET_DIR so desktop programs and their helpers use the same build output.
- Linux staging refuses to overwrite an existing distribution; regression scenarios cover default, relative and absolute target paths.
- Use the shared atomic writer for the catalog rollback floor instead of a predictable temporary file.
- Allow full-width extension action labels to wrap rather than forcing them onto one line.
- Keep all desktop header controls visible in narrow windows with flexible navigation and reduced decorative spacing.
- Let the desktop device-name field shrink so its Save button stays readable in narrow windows.
- Keep the language list inside safe window bounds, flip it above the selector when needed, and contain wheel and touch scrolling within the list.
- Include generated AppImage files, checksums and runtime notes in the release-preparation artifact.

### Development

- Share reviewed first-party tracing configuration across the owner and UI, using bounded off-thread desktop output and native Android Logcat, safe invalid-filter fallback and structured gesture events without synchronous diagnostic files.
- Backport maintained netlink and title-font dependencies without disabling network monitoring or native window decorations; retain upstream licenses and patch provenance.
- Replace unmaintained token-pasting dependencies through minimal Freya, pulp and rav1e compatibility manifests while retaining image-format support.
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

### Documentation

- Add architecture, API, development, website, contribution and security documentation.
- Add issue, pull-request and discussion templates with guidance for non-sensitive diagnostics.
- Align website build instructions with the desktop helper and Android distribution-flavor build tasks.
<!-- CHANGES-END -->
