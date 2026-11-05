# Development and verification

## Reproducible entry points

The toolchain is pinned in `rust-toolchain.toml`; `Cargo.toml` declares the minimum supported Rust version separately. CI reads the toolchain file rather than choosing a moving stable channel. Keep the lockfile when reproducing a failure.

| Command | What it checks or creates |
| --- | --- |
| `cargo xtask fmt` | Workspace rustfmt check |
| `cargo xtask metadata` | AppStream/desktop metadata and Python tooling tests |
| `cargo xtask check` | Desktop workspace and all-target compilation |
| `cargo xtask clippy` | Desktop workspace/all-target linting, warnings denied |
| `cargo xtask test` | Desktop workspace unit/documentation tests, followed by application integration tests with `test-plugin,test-identity` enabled |
| `cargo xtask audit` | Dependency advisory audit, warnings denied |
| `cargo xtask gate` | The above gates in that order, stopping on failure |
| `cargo xtask build` | Release desktop programs, bundled extension programs and native host |
| `cargo xtask native-host` | Native extension helper only |
| `cargo xtask dev` | Development-profile desktop programs, bundled extensions and native host |
| `cargo xtask android-dev` | Community native libraries and host-signed debuggable APK; no deployment |
| `cargo xtask android` | Android release libraries, both flavors, unsigned packages and inspection |
| `cargo xtask upload-wasm` | Format, lint and test the upload guest, build WASM, exercise the real isolated worker with synthetic host replies, and create a manual development archive; no upload or installation |
| `cargo xtask flatpak` | Local unsigned Flatpak bundle/repository from source |


### Root shell helpers

The root helpers reuse these tasks rather than maintaining another build implementation. `scripts/build_layout.py` supplies the shared directory and target selection; explicit `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET` overrides are preserved. Helpers work from another current directory:

| Helper | Purpose |
| --- | --- |
| `./dev.sh desktop` | Desktop development build, including bundled extensions and sibling helpers |
| `./dev.sh android` | Optimized Rust Android libraries for both ABIs, packaged as a debuggable community APK |
| `./dev.sh desktop --container NAME` / `android --container NAME` | Compile in an existing Distrobox; Android debug signing remains on the host |
| `./dev.sh deploy nx rog` | Explicitly update the selected phones with the development APK; preflight all signers, deduplicate transports and verify installed bytes |
| `./build.sh` / `./build.sh desktop` | Release desktop build; defaults Cargo output to `target/ui/` |
| `./build.sh android` | Existing unsigned Android release-preparation task; defaults Cargo output to `target/android/` |
| `./check.sh` | Full strict gate, including the advisory audit; defaults Cargo output to `target/ui/` |
| `./check.sh fmt` / `metadata` / `check` / `clippy` / `test` / `audit` | Run just that existing gate |
| `./store-demo.sh` | Build and open the real desktop UI with a fresh, isolated owner and the public signed test catalog |
| `./website.sh` / `./website.sh --empty-store` | Offline, loopback-only website preview with labelled store fixtures or the normal empty store |

All helpers support `--help` and install no prerequisites. Only the explicit `dev.sh deploy` command installs APKs; it never uninstalls, clears data or repairs a signer mismatch by replacing the application identity. Development packaging uses Gradle's existing local debug signer; release packaging remains unsigned. None of these commands commits, pushes, publishes a release or deploys the website. The store launcher stops only its own processes when its window closes or it receives an interrupt; it leaves its disposable data under `target/store-demo/`. See [the demo scope and extension submission flow](extension-store.md#a-local-store-ux-demonstration).

### Explicit repository publication

`python3 scripts/deploy.py dev` and `python3 scripts/deploy.py release --version v0.1.0` **only print a plan**. They do not stage files, create commits or tags, or publish anything unless `--publish` is explicitly supplied. Use a clean committed source checkout and run the quality gates before publication; this helper is not a build, CI approval, binary release upload or website deployment.

Before the first versioned release, development publication defaults to a fresh, signed, **single-commit `main` snapshot**. Its previous tip is preserved in a backup branch in the same atomic push, using an exact destination lease. The first release starts the release history; later releases append signed commits and signed version tags. A release tag must match the committed workspace version and cannot replace an existing tag.

After a versioned release exists, development publication defaults to `dev` and refuses to overwrite `main`; `--target BRANCH` chooses another development branch. The source may be any local branch, but only its committed `HEAD` tree is published. Existing signing configuration is reused with explicit signature verification; signing failure never falls back to unsigned commits. Global configuration, the caller's branch, index, working tree and untracked files are left untouched.

Review the plan before adding `--publish`. A rejected lease or atomic push is an error, not permission to force an update. If a network failure leaves the outcome uncertain, inspect the actual remote refs before retrying.


The metadata gate needs `appstreamcli` and `desktop-file-validate`. The audit gate needs `cargo-audit` (the CI workflow pins its version). Linux rendering builds need the compiler and graphics prerequisites listed by `scripts/install-linux-deps.sh`. Never assume a missing validator means the metadata passed.

The test gate enables application integration features only in its second Cargo invocation.
This exercises plugin processes, Store fixtures and an owner with an in-memory identity;
it does not enable test identity in ordinary development or release builds. A bare
`cargo test --workspace` omits some of this coverage and is not equivalent to the test gate.

## Output directories

Desktop helpers use `CARGO_TARGET_DIR=target/ui` and select the native Rust target explicitly, so Cargo supplies `target/ui/<rust-target-triple>/debug/` or `release/`. This uses Cargo's own grouping, not hand-written names such as `linux-x64`. Development and release programs each keep their matching helpers beside them. The same resolver is used by the store launcher; Linux staging honors the configured target and never silently falls back to stale flat-directory binaries.

Android compilation already uses Cargo's target grouping: `target/android/{aarch64-linux-android,x86_64-linux-android}/release/`. Gradle inputs remain `target/android/jniLibs/{play,community}/<abi>/`; Gradle cache/intermediates/raw packages stay under `target/android/gradle/`. The ready-to-install community development APK is copied to `target/distribution/android/community/debug/app-community-debug.apk` only after packaging and the JVM transport checks pass. `dev.sh deploy --apk PATH nx rog` explicitly selects another debuggable APK.

Bare Cargo retains its normal `target/<profile>/` behavior when no build environment is selected. For a helper-compatible staging shell, select the same environment first. No command migrates or removes older caches. Target directory names do not imply that Windows/macOS cross-compilation or packaging has been implemented.

Staging is deliberately fresh-output-only:

```sh
build_environment="$(python3 scripts/build_layout.py desktop)"
eval "$build_environment"
cargo xtask build
python3 scripts/stage-linux.py
```

Do not delete an existing distribution blindly to make a command pass. Choose a fresh build location or inspect the existing output and remove only artifacts whose ownership and replacement are established.

## Extension execution by distribution

This is the required distribution contract. Do not describe all Android builds as WASM-only, and do not treat the Google Play restriction as a desktop restriction.

| Application distribution | External extension formats |
| --- | --- |
| Android from Google Play | **WASM only**, through the interpreter; no downloaded native `.so`, external process, DEX or JAR execution |
| Android APK from any other store or direct distribution | **Native `.so` and WASM**; use the external/community build with native-extension support enabled |
| Desktop | **Native process binaries, native libraries (`.so` / `.dll`) and WASM** |

The source of an Android download is not detected at runtime to grant extra privileges: the Play and external/community build flavors enforce their own execution policy. Every non-Play distribution must use the external/community flavor when providing the native-extension contract; changing a store name must not silently remove `.so` support or enable native code in a Play build.

The matrix describes what the host distribution must support, not what every individual extension must ship. Package manifests still declare their execution kind, ABI and supported operating systems. Native binaries and libraries must match the host OS and architecture: a Windows `.dll` is not a Linux `.so`, and neither is a portable WASM guest. A desktop WASM package is not Android-only merely because Google Play requires WASM.

Keep format support separate from verified platform readiness. The outstanding OS-specific implementation and acceptance checks are recorded in [release readiness](release-readiness.md); this contract is not evidence that every desktop OS has passed end-to-end testing. Native process/library separation does not by itself provide an adversarial OS sandbox; WASM resource access remains mediated by explicit host services and permissions.

## Android

Install Java 17, SDK 36, build-tools 36.0.0, NDK 27.2.12479018, `cargo-ndk` 4.1.2 and the `aarch64-linux-android`/`x86_64-linux-android` Rust targets as described by `.github/workflows/artifacts.yml`. Set `ANDROID_HOME`, `ANDROID_NDK_HOME` and `ANDROID_NDK` for the actual installation. A desktop Skia build cannot be reused as an Android native library.

`cargo xtask android` builds the Play and community libraries into `target/android/jniLibs/{play,community}/`, enables `native-extensions` only for community, builds community native hosts, invokes Gradle for both unsigned release APKs/AABs, and inspects native artifacts. It does not retrieve signing keys. Never publish unsigned development packages as installable releases.

`android-dev-native` enables the `development-diagnostics` Cargo feature explicitly,
so optimized development libraries emit reviewed owner/platform debug events and
gesture traces to native Logcat. `android` release preparation does not enable it.
This distinction follows the build command, not a runtime permission or the APK's
debuggable flag. Neither mode writes gesture diagnostic files or opens a listener.

### Sandboxed upload extension

The standalone guest in `crates/peersus-upload/wasm` uses the same WASM runtime on Linux and Android. Both adapters use the application's shared file/HTTP authority: explicit file selection, caller-scoped immutable file snapshots, origin-bound upload consent and opaque credential/session handles. Neither credential values nor native file paths are returned to the guest. HTTP redirects and automatic retries are disabled. Cancellation and permission withdrawal release outstanding host work. Platform UI and encrypted credential storage remain platform-specific; the native upload extension's existing credential scope is not borrowed or migrated.

`cargo xtask gate` also runs `upload-wasm`: formatting, strict lint, protocol tests, both dependency audits and the real isolated worker exchange for the upload guest; the declared toolchain includes `wasm32-unknown-unknown`. Build the guest with `cargo build --manifest-path crates/peersus-upload/wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown --target-dir target/upload-wasm`, then package it with `python3 scripts/package-native-extension.py --source crates/peersus-upload/wasm --sdk . --target-dir target/upload-wasm --out target/extensions/upload-wasm --platform linux --id upload`. Use `--platform android` and a fresh output directory for Android. Packaging does not run the quality gate, install the extension or approve a new artifact hash. Linux API-key entry requires `zenity`; file selection and destination/result dialogs use the native desktop dialog adapter.

For a manual CLI installation, unpack the validated archive into a fresh directory and pass its `upload/` directory to `peersus plugin-install PATH_TO_UNPACKED/upload`; this command accepts a package directory, not a `.tar.gz` file. It does not approve permissions or read credentials. Restart the application with the updated host before testing, then approve the extension and configure its API key through its explicit action. A native and a WASM build use the same extension ID: they are alternative implementations, not two extensions to install side by side.

The manual installer refuses a different package already installed under the same ID; it does not update or overwrite that package in place. Use a disposable profile for an isolated trial, or explicitly review and approve removal of the old implementation before switching. Do not rename the ID to install duplicate providers for the same service.

The application carries no extension. No package is built into the APK or installed at first start: every extension, upload and clipboard included, is installed by the person, from the store or by hand, through the ordinary package manager and its approval screen, with all host permissions initially denied. The archives a recipe produces are for that: they are also available for manual development installation, and they are not signed store submissions. Automated worker tests use synthetic file, consent and provider responses, not real credentials or a live upload. Device picker/consent behavior and live-upload verification remain separate acceptance checks.

The interpreter's tests run the real shipped guests through it, and a test that is skipped passes, so `cargo xtask test` builds the 32-bit guests first and demands them (`PEERSUS_REQUIRE_GUESTS=1`): a guest nobody built is a guest nobody checked. The 64-bit guests need a nightly toolchain that builds the standard library, which the gate does not install, so they are checked wherever they exist and are not demanded.

Android HTTPS uses the unmodified upstream `rustls-platform-verifier` 0.7.1 and its official Android support artifact 0.2.0 (MIT OR Apache-2.0). The Maven source is pinned to upstream revision `1aa691352a5e0c215210dfc9a3c2f2078639dc91`; Gradle checks the support version against the workspace lockfile. JNI initialization precedes native application startup, and release shrinking retains the verifier bridge. This is trust-store integration, not a certificate-validation bypass.


Test with disposable application data or keep existing data in place. An uninstall is not a safe default for an upgrade. Use an emulator or a device that is explicitly available for testing; device availability does not authorize reading its private files.

## Behavioral verification

A change to relay selection needs a first-configuration test with no saved custom URL, not just a parser test. An Android layout change needs a narrow viewport, long translated labels, scrolling, keyboard visibility and touch-target inspection. A plugin action needs permission-denied, disconnected, running, cancel and failure states. A packaging change needs a staged executable and helper lookup, not only compilation.

Record the exact command, revision/worktree state, toolchain and result for meaningful claims. Compilation and unit tests do not prove a desktop window rendered or an Android application launched. A historical successful device run does not validate a later build.

## GitHub dependency and security automation

- `.github/dependabot.yml` proposes weekly updates for the Cargo workspace, pinned GitHub Actions and the Android Gradle project. Minor/patch version updates are grouped per ecosystem; major updates remain separate. Updates are not merged automatically and must pass the normal checks. Cargo includes indirect dependencies; no private registry credentials are configured.
- The full quality gate audits dependencies on pushes and pull requests. `Dependency advisory audit` also runs daily and on manual dispatch, so a newly published advisory can fail the audit without a source change. It uses the same `cargo xtask audit` policy: warnings remain errors. It does not build the application or upload a dependency snapshot.
- These configuration files become active only after publication to the repository's default branch. Local YAML/schema validation is not a successful hosted workflow run. Scheduled jobs use Actions runner time.
- Dependency graph, Dependabot alerts, security updates and Dependabot runner selection are repository settings, not enabled by `dependabot.yml`. Use GitHub-hosted runners unless a real private-network requirement justifies registered self-hosted runners. Selecting a labeled runner without a matching runner prevents those jobs from running.
- Automatic dependency submission is separate from Dependabot update PRs. Gradle's resolved transitive graph can use automatic submission on GitHub-hosted runners; Cargo does not use GitHub's automatic-submission workflow. Do not register a runner merely to silence an unused-label warning.
- CodeQL/code-scanning and secret-scanning availability depends on repository ownership, visibility and the licensed products. Do not enable a paid product, grant new workflow write permissions, or claim coverage merely because its settings page exists. Verify actual analysis runs before calling it active. Never expose secret-scanning alert payloads in logs.

Reference: [Dependabot options](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference), [automatic dependency submission](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/submit-dependencies-automatically), [CodeQL](https://docs.github.com/en/code-security/concepts/code-scanning/codeql/codeql-code-scanning).

### Local dependency compatibility patches

The dependency gate is not currently claimed green. Each local patch retains its published source's license files and records the upstream revision, exact change and removal condition in `vendor/<package>/PATCHES.md`:

- `netwatch`: keep its transport-facing API while backporting the maintained netlink set and the cross-target `netdev` requirement. Network transport regression tests pass; that is not an end-to-end connectivity claim.
- `sctk-adwaita`: backport the upstream `skrifa` title renderer without changing the existing Wayland toolkit API. All eleven renderer/font-preference tests pass; native rendering and platform builds still need verification.
- `freya-core`, `freya-components`, `pulp`, `rav1e`: retain upstream implementation and image-format support; use the maintained `pastey` package under the existing dependency alias. The desktop UI consumer compiles with these patches. Platform behavior verification remains pending.
- `zbus` 6: migrate the application's direct Linux sleep and desktop-theme watchers to the maintained integrated wire-format API. Isolated private-bus sleep and initial/live theme tests pass. The root lockfile's strict dependency audit now passes locally; this is not hosted CI or a complete platform-runtime result.
- `mundy`: preserve portal appearance and accessibility preferences while migrating the Linux dependency and nested-variant match to zbus 6. Non-Linux implementation and upstream licensing are unchanged. See [patch provenance](../vendor/mundy/PATCHES.md); seven unit tests, including nested-variant wire decoding, pass. Actual desktop portal preference behavior remains unverified.
- Accessibility ports: retain `atspi`, `atspi-common`, `atspi-proxies`, `accesskit_atspi_common` and `accesskit_unix`, migrating wire imports and runtime features without disabling assistive-technology integration. Keep `zbus-lockstep` and its macros, using maintained `zbus_xml` 6 for XML signal-signature validation. All eight port manifests declare the required Rust 1.89 minimum. The desktop UI compiles; 1,141 common protocol/event tests, 23 XML validator tests and the proxy/adapter/cache suites pass. A private-bus registration regression verifies the Embed handshake, duplicate-registration semantics, cache installation and an actual cross-connection method call. Full screen-reader/platform behavior remains unverified. Each vendor directory records upstream revision, licenses and removal conditions.
- `keyring` 3.6.3: the application retains the existing Apple/Windows native and synchronous encrypted Secret Service backends. A [local feature-surface patch](../vendor/keyring/PATCHES.md) removes the unused asynchronous backend and its optional legacy serialization dependencies. Credential APIs, native implementations, lookup precedence and the legacy default-collection fallback are retained. This internal copy intentionally does not offer upstream async features; the application's configured features are preserved. A supported workspace lockfile refresh alone did not remove these optional dependencies. The regenerated root lockfile passes `cargo xtask audit` (`cargo audit --deny warnings`) locally with no vulnerabilities or warnings. Targeted regressions pass: two mock identity tests, five relay-token tests, sixteen upload tests, ten About/license tests and four locale/catalog tests. The permission-gated native credential test remains ignored; complete platform-runtime and hosted CI verification are not claimed.

These patches remove dependency chains rather than adding audit exceptions. The strict root-lockfile audit passes locally without suppressions. Application About, README and prototype security notes stay synchronized with the patch inventory and verification limits. A local passing audit is not hosted CI, an independent security assessment, or proof of complete runtime compatibility.

## Website

See [website staging](website.md). The website build is separate from the desktop and Android release-artifact workflow. Catalog signatures are checked in the application; static-site validation alone does not establish signature authenticity.
