# Architecture and interface contract

## Build and source of truth

Rust workspace (edition 2024, MSRV 1.96). Dependencies are declared once in the root `Cargo.toml` and referenced with `workspace = true`. `unsafe_code` is forbidden except in the two Android platform crates below, and there only for narrow, audited FFI boundaries.

| Owner | Responsibility |
|---|---|
| `crates/peersus-plugin-api` | Headless extension contract: manifest, services, contributions, settings, broker. No toolkit, OS, network or product concept. |
| `crates/peersus-core` | Product model, commands, pairing, settings. Knows nothing about any concrete plugin. |
| `crates/peersus-net` | Identity, encrypted transport, transfer, QR. |
| `crates/peersus-app` | Runtime owner, CLI, local IPC, instance lock, plugin host. |
| `crates/peersus-ui` | Freya frontend (library). Renders `Snapshot` and plugin `Contributions`; owns no business logic. |
| `crates/peersus-ui-android` | Android application entry and JNI platform adapters. Narrow audited FFI only. |
| `crates/peersus-plugin-android` | Isolated WASM worker JNI descriptor adapter. Links the bounded interpreter only; no UI, application identity, network stack or external-native loader. Narrow audited FFI only. |
| `plugins/` | Bundled plugin adapters (clipboard first). Depend on `peersus-plugin-api` only. |

## Development builds and artifact layout

- Project stage: **private, unpublished development prerelease**. Cargo `debug`/`release` profiles are compilation choices, not permission to publish, tag or change the project version.
- Preferred entry point: `./dev.sh`. It delegates to `cargo xtask`; do not maintain a second set of ad-hoc build commands.
- `./dev.sh desktop` builds the desktop programs, bundled extensions and native helper in the Cargo **dev** profile (`debug/`).
- `./dev.sh android` builds community native libraries for both declared Android ABIs, then packages a **debuggable community APK** with Gradle's existing local debug signer. The Rust Android libraries remain optimized (`release`); a debug APK does not imply unoptimized Rust.
- `./dev.sh desktop --container NAME` or `./dev.sh android --container NAME` uses an existing Distrobox for compilation. Android APK packaging/debug signing stays on the host, avoiding an unrelated container debug certificate.
- `./dev.sh deploy nx rog` explicitly updates only those selected physical phones. It builds nothing, deduplicates multiple ADB transports, checks every installed signer before any installation, uses replace-install and verifies the installed APK byte-for-byte. It never uninstalls or clears application data; a signer mismatch is a refusal, not a reason to erase data.
- `./build.sh desktop` selects optimized desktop release artifacts; `./build.sh android` prepares both unsigned Android release flavors and bundles. Build commands never deploy.
- `./check.sh` runs the existing strict verification gate. Individual existing gates remain available as arguments.

Canonical helper layout is owned by `scripts/build_layout.py`, not hand-written OS aliases. Desktop helpers set the native `CARGO_BUILD_TARGET`; Cargo itself creates the standard target-triple directory:

```text
target/ui/<rust-target-triple>/debug/                  desktop development programs and helpers
target/ui/<rust-target-triple>/release/                optimized desktop programs and helpers
target/android/<android-rust-target-triple>/release/  optimized Android Rust artifacts
target/android/jniLibs/<flavor>/<android-abi>/         Gradle native-library staging inputs
target/android/gradle/                               Gradle caches, intermediates and raw outputs
target/distribution/android/community/debug/          installable development APK
```

The development APK is `target/distribution/android/community/debug/app-community-debug.apk`. `CARGO_TARGET_DIR` and an explicit `CARGO_BUILD_TARGET` remain overrides; consumers must use the same environment and resolver. Bare Cargo retains its normal native `target/<profile>/` convention unless those variables are set. Never silently select an older flat-directory artifact when a triple-specific artifact is missing. Changing the default layout does not migrate, delete or overwrite old caches. Windows/macOS triples describe layout; their appearance here does not claim new cross-compilation or packaging support.

See `docs/development.md` for prerequisites, detailed commands and the distinction between artifacts, staging and published releases.

## Plugin agnosticism rules

- The core and the UI never name a plugin. A plugin is identified by an id assigned by the host, never by a manifest field.
- Anything a plugin adds to the product (pages, actions, settings, per-device grants) is declared as data in its manifest and rendered generically.
- Cross-plugin communication goes through the broker: exact versioned imports/exports, provider allowlists and capability grants.
- Network streams are routed by `(service, version)` to the provider that exports them; no host code branches on a service name.
- Per-device grants and plugin settings are stored as generic maps keyed by plugin id.

## Pairing contract

Both users independently choose the same one of three number circles; a different choice refuses. File transfer never pairs a device implicitly.

## User interface (decided; migration from the Blitz prototype is not finished)

- Toolkit: **Freya** (`freya`, MIT), on the 0.5 release candidate line, pinned exactly in the root `Cargo.toml`. Chosen after a like-for-like trial against Xilem; the evidence and the accepted weaknesses are in the project notes, not repeated here. The UI is a single library crate plus thin entries (a desktop `main`, an Android `android_main` in the platform crate), so one set of screens serves desktop and mobile.
- The look is the application's own: no operating-system title bar; the header, its navigation and the window buttons are drawn by the application and themed through Freya's global `Theme`. All screen rules (what to show, what a person is told, which device a send goes to) live in `peersus-ui/src/view.rs`, which knows no toolkit, so that replacing the drawing layer never means rewriting a rule.
- Mobile has its own layout (safe-area insets, navigation placement); the desktop layout is not shrunk onto a phone.
- `unsafe` is allowed only in the two Android platform crates above, for their entry symbols and descriptor/JNI ownership transfers. Every `unsafe` block carries a `// SAFETY:` comment naming the invariant. Every other Rust crate keeps `unsafe_code = "forbid"`.
- A web view is allowed only for content that really is web content (published API and wiki pages), never as the base of the interface.
- Skia (Freya's renderer) is **built from source** (`FORCE_SKIA_BUILD=1`), not taken from the unverified prebuilt archive. The bindings fetch their pinned source revision from their fork; this remains a source trust decision. Linux rendering requires C/C++ and graphics development headers; on an immutable host, use the existing build container through `./dev.sh ... --container NAME`. Keep `embed-freetype` enabled. Do not substitute host libraries into the container or bypass the source-build setting to make a build pass.
- Build UI targets on disk, not on a RAM-backed `/tmp`: set `CARGO_TARGET_DIR` to a directory on disk.
- A web, desktop or Android build counts as working only after a person has seen it run. A process that starts is not an acceptance.

## Validation

Before claiming a behaviour, run the real entry point. A passing unit test is not visual or end-to-end acceptance. Do not build distribution packages unless asked.
