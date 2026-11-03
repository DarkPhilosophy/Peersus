# Development and verification

## Reproducible entry points

The toolchain is pinned in `rust-toolchain.toml`; `Cargo.toml` declares the minimum supported Rust version separately. CI reads the toolchain file rather than choosing a moving stable channel. Keep the lockfile when reproducing a failure.

| Command | What it checks or creates |
| --- | --- |
| `cargo xtask fmt` | Workspace rustfmt check |
| `cargo xtask metadata` | AppStream/desktop metadata and Python tooling tests |
| `cargo xtask check` | Desktop workspace and all-target compilation |
| `cargo xtask clippy` | Desktop workspace/all-target linting, warnings denied |
| `cargo xtask test` | Desktop workspace unit, integration and documentation tests |
| `cargo xtask audit` | Dependency advisory audit, warnings denied |
| `cargo xtask gate` | The above gates in that order, stopping on failure |
| `cargo xtask build` | Release desktop programs, bundled extension programs and native host |
| `cargo xtask native-host` | Native extension helper only |
| `cargo xtask dev` | Development-profile desktop programs, bundled extensions and native host |
| `cargo xtask android-dev` | Community native libraries and host-signed debuggable APK; no deployment |
| `cargo xtask android` | Android release libraries, both flavors, unsigned packages and inspection |
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

The metadata gate needs `appstreamcli` and `desktop-file-validate`. The audit gate needs `cargo-audit` (the CI workflow pins its version). Linux rendering builds need the compiler and graphics prerequisites listed by `scripts/install-linux-deps.sh`. Never assume a missing validator means the metadata passed.

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

## Android

Install Java 17, SDK 36, build-tools 36.0.0, NDK 27.2.12479018, `cargo-ndk` 4.1.2 and the `aarch64-linux-android`/`x86_64-linux-android` Rust targets as described by `.github/workflows/artifacts.yml`. Set `ANDROID_HOME`, `ANDROID_NDK_HOME` and `ANDROID_NDK` for the actual installation. A desktop Skia build cannot be reused as an Android native library.

`cargo xtask android` builds the Play and community libraries into `target/android/jniLibs/{play,community}/`, enables `native-extensions` only for community, builds community native hosts, invokes Gradle for both unsigned release APKs/AABs, and inspects native artifacts. It does not retrieve signing keys. Never publish unsigned development packages as installable releases.

Test with disposable application data or keep existing data in place. An uninstall is not a safe default for an upgrade. Use an emulator or a device that is explicitly available for testing; device availability does not authorize reading its private files.

## Behavioral verification

A change to relay selection needs a first-configuration test with no saved custom URL, not just a parser test. An Android layout change needs a narrow viewport, long translated labels, scrolling, keyboard visibility and touch-target inspection. A plugin action needs permission-denied, disconnected, running, cancel and failure states. A packaging change needs a staged executable and helper lookup, not only compilation.

Record the exact command, revision/worktree state, toolchain and result for meaningful claims. Compilation and unit tests do not prove a desktop window rendered or an Android application launched. A historical successful device run does not validate a later build.

## Website

See [website staging](website.md). The website build is separate from the desktop and Android release-artifact workflow. Catalog signatures are checked in the application; static-site validation alone does not establish signature authenticity.
