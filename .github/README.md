# Peersus

Private connections. Your devices, together.

Peersus is a development-stage device connection and file-transfer application with a Rust application owner, desktop and Android frontends, and manifest-driven extensions. Pair devices deliberately, review incoming transfers, and grant extensions only the access they need.

<!-- STATUS-START -->
Workspace version: **0.1.0** (development; not a release claim). Rust toolchain: **1.96.1**; declared MSRV: **1.96**. License: **Apache-2.0**.
<!-- STATUS-END -->

**Development status:** a successful build is not a supported public release. The extension store requires a maintainer trust key and published signed catalog; it is not automatically available just because its UI and installer exist. See [release readiness](docs/release-readiness.md) for historical evidence and limitations.

## Components

- A single application owner handles validated commands, snapshots, pairing, transport and transfer state.
- The CLI and graphical frontends use shared application contracts, rather than independent implementations of transfer policy.
- Extensions declare capabilities, settings, actions and services. The host assigns their identity and checks grants; presentation code does not identify extensions by display name.
- Desktop helper executables must ship beside the owner. Android uses its own platform integration and build flavors; desktop packaging is not an Android installation method.

Read the [architecture](docs/architecture.md), [API reference](docs/api/README.md) and [extension-store contract](docs/extension-store.md) before changing those boundaries.

### Bundled extensions

<!-- PLUGINS-START -->
| Bundled extension | Declared platforms | Exported services |
| --- | --- | --- |
| [peersus-clipboard](crates/peersus-clipboard/plugin.json) | android, linux, macos | `clipboard.text` v1 |
| [peersus-upload](crates/peersus-upload/plugin.json) | linux, macos | `upload.file` v1 |
<!-- PLUGINS-END -->

## Build desktop

Use the toolchain declared in `rust-toolchain.toml`, plus the Linux prerequisites described by `scripts/install-linux-deps.sh`. Review system package commands before running them. Skia is built from source, which needs its compiler dependencies and network access on a cold build.

```sh
FORCE_SKIA_BUILD=1 cargo xtask build
./target/release/peersus-ui
```

`cargo xtask build` prepares the desktop owner, UI, extension hosts, store fetcher and bundled extension programs. If `CARGO_TARGET_DIR` is set, executables are under that directory's `release/` instead. Keep the owner and its helpers together; building only the UI is not a complete distribution.

To stage a fresh Linux distribution after the build:

```sh
python3 scripts/stage-linux.py
```

The output is `<target-directory>/distribution/linux/`. Staging deliberately refuses to overwrite an existing distribution. This is not an installer, a signature or a publication step.

Root shortcuts are available as `./dev.sh`, `./build.sh`, `./check.sh`, `./store-demo.sh` and `./website.sh`, each with `--help`. Development build and explicit phone deployment are separate commands. Desktop helpers use Cargo's `target/ui/<target-triple>/<profile>/` layout; APKs ready for development deployment are under `target/distribution/android/community/debug/`. Explicit Cargo output/target overrides are preserved. See [the helper reference](docs/development.md#root-shell-helpers).


## Use the CLI

```sh
./target/release/peersus --help
./target/release/peersus peers
./target/release/peersus plugins
```

The CLI exposes pairing, transfers, settings and generic plugin commands. Consult `--help` on the corresponding subcommand for the current syntax. Never place relay access tokens or other credentials in command arguments; the relay token is entered through Settings and kept separately from ordinary preferences.

## Android

Android requires SDK 36, a compatible NDK, Java 17, `cargo-ndk`, and Rust targets for each packaged ABI. The minimum platform is API 31. The manual release-artifact workflow documents the pinned NDK. `cargo xtask android` builds both Play and community flavors, including native-library inspection, and produces unsigned release artifacts. It does not sign, publish or install them.

The community flavor enables native extensions; do not assume a Play build has the same executable-extension policy. See [development and verification](docs/development.md) for build entry points and validation boundaries.

## Verification

```sh
cargo xtask gate
```

The gate checks formatting, metadata and Python tooling, desktop-target compilation, Clippy, tests and the dependency advisory audit. It excludes the Android entry from desktop Cargo checks; it does not replace a device run or desktop rendering inspection. Install the metadata validators and `cargo-audit` first. Do not bypass a failed gate by weakening assertions or suppressing advisory failures.

<!-- CI-START -->
These are workflow definitions, **not live passing-status claims**.

- [Local release preparation](.github/workflows/artifacts.yml)
- [Quality gates](.github/workflows/ci.yml)
- [Generated documentation](.github/workflows/docs.yml)
- [Approve extension and publish catalog](.github/workflows/extension-approve.yml)
- [Extension submission check](.github/workflows/extension-validate.yml)
- [Website preview artifact](.github/workflows/pages.yml)
<!-- CI-END -->

## Development changes

<!-- LATEST-START -->
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
<!-- LATEST-END -->

## Website and extension store

The website is a dependency-free static site. `site/build.py` validates local resources and stages a new output directory. A signed extension catalog can be included using `--catalog`; the application, not the web page, verifies its signature. See [website preparation](docs/website.md). No local build command implicitly deploys, signs or publishes content.

## Contributing and security

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [Documentation index](docs/README.md)

Licensed under [Apache-2.0](LICENSE). Third-party notices are in [NOTICE](NOTICE); vendored dependencies retain their own licenses.
