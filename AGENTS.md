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
- Skia (Freya's renderer) is **built from source** (`FORCE_SKIA_BUILD=1`), not taken as the prebuilt binary archive the bindings download, because the bindings do not verify what they download. Source is still fetched over the network at a pinned tag (the bindings' `[package.metadata] skia`) from the bindings' own fork (`github.com/marc2332/rust-skia`), so what changes is that we trust reviewable source and not an opaque archive; the fork stays a trust decision. Proven on the development host (Bazzite, immutable): a source build linked and the resulting binary resolves every shared library on the host. It needs `freya-skia-safe` with feature `embed-freetype` and the C and graphics development headers, which the immutable host lacks, so the build runs in the `peersus-ui-build` distrobox (Fedora 44; `dnf` packages: `fontconfig-devel freetype-devel mesa-libEGL-devel mesa-libGL-devel libxkbcommon-devel wayland-devel libX11-devel libxcb-devel clang gcc gcc-c++ python3 ninja-build git cmake pkgconf-pkg-config lld`): `distrobox enter peersus-ui-build -- env FORCE_SKIA_BUILD=1 CARGO_TARGET_DIR=<dir on disk> cargo build`. The first build takes about seven minutes. Do not set `LIBRARY_PATH` or symlink host libraries inside the container. A local mirror of exported binaries, if one is ever used to save time, lives outside the repository with its sha256 recorded and is selected with `SKIA_BINARIES_URL=file://...`.
- Build UI targets on disk, not on a RAM-backed `/tmp`: set `CARGO_TARGET_DIR` to a directory on disk.
- A web, desktop or Android build counts as working only after a person has seen it run. A process that starts is not an acceptance.

## Validation

Before claiming a behaviour, run the real entry point. A passing unit test is not visual or end-to-end acceptance. Do not build distribution packages unless asked.
