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
| `cargo xtask build` | Release desktop programs (interface, owner, extension runtime, store fetcher) and native host; no extension |
| `cargo xtask native-host` | Native extension helper only |
| `cargo xtask dev` | The same programs and native host in the development profile; no extension |
| `cargo xtask android-dev` | Community native libraries and host-signed debuggable APK; no deployment |
| `cargo xtask android` | Android release libraries, both flavors, unsigned packages and inspection |
| `cargo xtask upload-wasm` | Format, lint and test the upload guest, build WASM, exercise the real isolated worker with synthetic host replies, and create a manual development archive; no upload or installation |
| `cargo xtask flatpak` | Local unsigned Flatpak bundle/repository from source |


### Root shell helpers

The root helpers reuse these tasks rather than maintaining another build implementation. `scripts/build_layout.py` supplies the shared directory and target selection; explicit `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET` overrides are preserved. Helpers work from another current directory:

| Helper | Purpose |
| --- | --- |
| `./dev.sh desktop` | Desktop development build with its sibling helpers; no extension is built or installed |
| `./dev.sh android` | Optimized Rust Android libraries for both ABIs, packaged as a debuggable community APK |
| `./dev.sh desktop --container NAME` / `android --container NAME` | Compile in an existing Distrobox; Android debug signing remains on the host |
| `./dev.sh deploy nx rog msi hp` | Explicitly update the selected devices: phones with the development APK (preflight all signers, deduplicate transports, verify installed bytes), desktops with the staged Linux tree (`msi` here, `hp` over ssh; `scripts/install-linux-user.py`, every installed program verified byte-for-byte). Every selected device is checked before any is changed, a desktop also for its C library: programs linked on a newer workstation need a newer glibc than an older desktop has, and are refused there before anything is installed. Build what an older desktop gets inside the older container (`./build.sh desktop` run in `peersus-ui-build`, or `./dev.sh desktop --container NAME`) |
| `./build.sh` / `./build.sh desktop` | Release desktop build; defaults Cargo output to `target/ui/` |
| `./build.sh android` | Existing unsigned Android release-preparation task; defaults Cargo output to `target/android/` |
| `./check.sh` | Full strict gate, including the advisory audit; defaults Cargo output to `target/ui/` |
| `./check.sh fmt` / `metadata` / `check` / `clippy` / `test` / `audit` | Run just that existing gate |
| `./store-demo.sh` | Build and open the real desktop UI with a fresh, isolated owner and the public signed test catalog |
| `./website.sh` / `./website.sh --empty-store` | Offline, loopback-only website preview with labelled store fixtures or the normal empty store |

All helpers support `--help` and install no prerequisites. Only the explicit `dev.sh deploy` command installs the application (APKs on phones, the staged tree on desktops); it never uninstalls, clears data or repairs a signer mismatch by replacing the application identity. Development packaging uses Gradle's existing local debug signer; release packaging remains unsigned. None of these commands commits, pushes, publishes a release or deploys the website. The store launcher stops only its own processes when its window closes or it receives an interrupt; it leaves its disposable data under `target/store-demo/`. See [the demo scope and extension submission flow](extension-store.md#a-local-store-ux-demonstration).

### Explicit repository publication

`python3 scripts/deploy.py dev` and `python3 scripts/deploy.py release --version v0.1.0` **only print a plan**. They do not stage files, create commits or tags, or publish anything unless `--publish` is explicitly supplied. Use a clean committed source checkout and run the quality gates before publication; this helper is not a build, CI approval, binary release upload or website deployment.

Before the first versioned release, development publication defaults to a fresh, signed, **single-commit `main` snapshot**. Its previous tip is preserved in a backup branch in the same atomic push, using an exact destination lease. The first release starts the release history; later releases append signed commits and signed version tags. A release tag must match the committed workspace version and cannot replace an existing tag.

After a versioned release exists, development publication defaults to `dev` and refuses to overwrite `main`; `--target BRANCH` chooses another development branch. The source may be any local branch, but only its committed `HEAD` tree is published. Existing signing configuration is reused with explicit signature verification; signing failure never falls back to unsigned commits. Global configuration, the caller's branch, index, working tree and untracked files are left untouched.

Review the plan before adding `--publish`. A rejected lease or atomic push is an error, not permission to force an update. If a network failure leaves the outcome uncertain, inspect the actual remote refs before retrying.


The metadata gate needs `appstreamcli` and `desktop-file-validate`. The audit gate needs `cargo-audit` (the CI workflow pins its version). Linux rendering builds need the compiler and graphics prerequisites listed by `scripts/install-linux-deps.sh`. Never assume a missing validator means the metadata passed.

The test gate enables application integration features only in its second Cargo invocation.
This exercises plugin processes, Store fixtures and an owner with an in-memory identity.
That invocation uses a `test-identity/` subdirectory of the selected Cargo target directory,
so Cargo cannot replace the ordinary desktop executable with the ephemeral-identity test
executable. It does not enable test identity in ordinary development or release builds.
A bare `cargo test --workspace` omits some of this coverage and is not equivalent to the test gate.

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

### Installing the staged tree for one user (Linux)

`scripts/install-linux-user.py STAGED_TREE [--prefix DIR]` puts what `stage-linux.py` printed into a user's own prefix (`~/.local` by default): the programs in `bin/`, each replaced in one step so a running program keeps the file it was started from, and the desktop entry, the AppStream metadata, the icon and the licences under `share/`. The entry's `Exec` line is rewritten to the installed window's full path. It installs, approves and removes no extension, touches no data or preferences, and starts or stops nothing: restart the application afterwards.

The window announces the application id `io.github.DarkPhilosophy.Peersus` (`peersus_ui::APPLICATION_ID`): the Wayland application id and the X11 class. A desktop matches a window to its entry, and so to its name and icon, by that id; the entry is named by it and carries it as `StartupWMClass`, and a test holds the three together. A window with another id is shown by the dock as an unknown application with no icon, whatever is installed.

### The macOS bundle

`scripts/stage-macos.py` makes `Peersus.app` on a Mac, from the programs built there, into `<target>/distribution/macos/`. It is what a person opens: one icon, the name in the menu bar, the window and the owner with its helpers side by side in `Contents/MacOS` (the window starts the owner that lies next to it). It runs with macOS's own `sips`, `iconutil` and `codesign` and refuses without them.

```sh
cargo xtask build                                                      # the four programs and the native helper, release
python3 scripts/stage-macos.py                                         # a Mac with Metal
python3 scripts/stage-macos.py --environment FREYA_RENDERER=software   # a virtual machine: no Metal device
```

What it is not: a release. The signature is ad hoc (no identity), nothing is notarized, another Mac's Gatekeeper refuses it, and there is no disk image. `cargo xtask build` passes on macOS as on Linux (Apple's toolchain links the native helper position independent by default and has no `-z` options, so `xtask` leaves those two flags out there; Skia's build needs `ninja` on `PATH`, which macOS does not provide), and the bundle carries all five programs: the script refuses when one is missing. The native helper carries no embedded `Info.plist` of its own, unlike the Rust programs. `--environment` writes `LSEnvironment`, which macOS applies only when the bundle is opened as an application (Finder, Dock, `open`), not when the program inside is run by hand. `--profile debug` stages a development build.

**What a system monitor shows for each program.** Linux reads a program's publisher from the package it came from, so there it is the AppStream file (`packaging/linux/…metainfo.xml`), which lists all five programs and what each does. Windows and macOS read it from the program file, and `build-support/program_identity.rs` writes it there when each program is linked (each program's `build.rs` names it and says in one line what it does): on Windows with the MSVC toolchain, a version resource in every `.exe` (Task Manager's description, the file's Details: publisher, product, the crate's version and `PEERSUS_BUILD_NUMBER` as the fourth part); on macOS, an `Info.plist` embedded in each helper beside the bundle's main program (`peersus`, `peersus-wasm-host`, `peersus-store-fetch`: identifiers `io.github.DarkPhilosophy.Peersus.core`, `.wasm-host`, `.store-fetch`, which is also what an ad-hoc signature names them by), the window's being the bundle's own. Nothing is written for other targets, and the GNU Windows toolchain gets no resource.

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

Install Java 17, SDK 36, build-tools 36.0.0, NDK 27.2.12479018, `cargo-ndk` 4.1.2 and the `aarch64-linux-android`/`x86_64-linux-android` Rust targets as described by `.github/workflows/artifacts.yml`. `./dev.sh android` and `./build.sh android` take the SDK from `ANDROID_HOME` (or `ANDROID_SDK_ROOT`, or Gradle's `android/local.properties`) and derive `ANDROID_NDK_HOME`, `ANDROID_NDK`, the platform and the build tools from it (`scripts/build_layout.py`); a value already set is kept, and with no SDK named both refuse before building. Bare `cargo xtask android` reads only what is exported. A desktop Skia build cannot be reused as an Android native library.

`cargo xtask android` builds the Play and community libraries into `target/android/jniLibs/{play,community}/`, enables `native-extensions` only for community, builds community native hosts, invokes Gradle for both unsigned release APKs/AABs, and inspects native artifacts. It does not retrieve signing keys. Never publish unsigned development packages as installable releases.

`android-dev-native` enables the `development-diagnostics` Cargo feature explicitly, including the isolated WASM worker. Optimized development libraries therefore emit the same reviewed `peersus::dev` events as desktop debug builds. Release preparation does not enable it. Production subscribers cap output at warnings/errors even if `PEERSUS_LOG=trace` is set; development events are additionally gated at their emission sites.

### Diagnostic contract

Development builds default to `warn,peersus::dev=trace`; `PEERSUS_LOG=warn` disables development detail. What a `PEERSUS_LOG` value names is added to the warnings of everything, not put in their place: `PEERSUS_LOG=peersus_app::runtime=debug` shows that module's detail and still every warning and error; a level given for everything (`debug`, `off`) replaces the warning level. The development target is shown under a value only when the value names it (`peersus::dev=trace`). Each event identifies a fixed stage and process, with numeric broker connection/generation/call/ticket IDs, service/method ordinals, closed outcome categories, byte counts or elapsed times. Broker admission, refusal, routing, reply, expiry and disconnect correlate with deferred platform work, interpreter validation/entry/traps, data handles and stream read/write/shutdown failures. The input adapter advertises supported pointer modes through the generic API rather than a plugin-specific host branch.

Desktop diagnostics go to stderr (the owner launched by the UI retains its log). Only the trusted WASM interpreter inherits that diagnostic sink in development; arbitrary native/process plugin stderr remains discarded. Android owner and isolated worker use native Logcat tags. Development guests can opt into the scalar-only `diagnostic` import documented in the module ABI: closed stage/outcome vocabulary, bounded counters and 64 events per second per isolated generation, with no fuel renewal. Production hosts never emit those decisions. No payloads, clipboard text, typed input, filenames, device identities, endpoints, credentials or bearer claims are emitted. Unknown guest error text is reduced to a fixed category, never printed.

The desktop writer has a bounded 1,024-line queue and may drop records under overload; `Guard::dropped_lines()` exposes the count. Android Logcat is also bounded by the OS. These are diagnostic traces, not a lossless audit trail. Capture a bounded reproduction window, keep it local, and do not attach private logs to issues. No diagnostic path grants permissions, bypasses consent or opens a network listener.

On Linux/Wayland, independent pointers and their device-name labels are drawn by the owner, not by an open application window. If the inherited environment omits Xwayland authentication, the owner resolves the same-user local display socket's server and its exact authority file without changing global environment variables or the server's access policy. Authentication is never logged or persisted. A failed renderer is discarded and retried on subsequent input instead of leaving an invisible pointer cached for the remainder of the session.

### Sandboxed upload extension

The standalone guest in `peersus-upload/wasm` uses the same WASM runtime on Linux and Android. Both adapters use the application's shared file/HTTP authority: explicit file selection, caller-scoped immutable file snapshots, origin-bound upload consent and opaque credential/session handles. Neither credential values nor native file paths are returned to the guest. HTTP redirects and automatic retries are disabled. Cancellation and permission withdrawal release outstanding host work. Platform UI and encrypted credential storage remain platform-specific; the native upload extension's existing credential scope is not borrowed or migrated.

Extension sources are not part of `main`. Each extension is submitted on its own `submission/<id>-<date>` branch, under `extension/<name>/`, as a pull request against the `extension` base branch, and reaches a device only through the store (see [extension store](extension-store.md)). A working copy for local development sits in `plugins/<name>/`, which `main` ignores: nothing there is tracked, built or shipped by the application. Extensions are not application workspace members or application build dependencies. The host gate uses the generic SDK guest; each extension's own recipe tests and packages its source independently. With the upload extension's source present in `plugins/`, run its gate explicitly with `cargo xtask upload-wasm`. Build its guest with `cargo build --manifest-path plugins/peersus-upload/wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown --target-dir target/upload-wasm`, then package with `python3 scripts/package-native-extension.py --source plugins/peersus-upload/wasm --sdk . --target-dir target/upload-wasm --out target/extensions/upload-wasm --platform linux --id upload`. Packaging never installs or approves an artifact.

For a manual CLI installation, unpack the validated archive into a fresh directory and pass its `upload/` directory to `peersus plugin-install PATH_TO_UNPACKED/upload`; this command accepts a package directory, not a `.tar.gz` file. It does not approve permissions or read credentials. Restart the application with the updated host before testing, then approve the extension and configure its API key through its explicit action. A native and a WASM build use the same extension ID: they are alternative implementations, not two extensions to install side by side.

Run without options, `plugin-install` installs nothing: it prints the warning for a package from outside the store, the package's licence and notice, and the command that accepts them (`--accept <digest> --accept-outside-store-risk`). The digest is of the licence and notice as printed, so a package whose text differs by one word needs its own acceptance.

`scripts/deploy-plugin.py ID` installs through the running application instead (this computer's, and a phone's over adb through the debuggable package's own control socket), for development: it builds and packages the extension for each device's system, stages the archive, confirms it and gives back the permissions the replaced extension had, on this device and per paired device. It accepts nothing by itself: without `--accept DIGEST` it stops after packaging, and a package from outside the store also needs `--accept-outside-store-risk`, exactly as the command line. The digest is the one `plugin-install` prints for that extension; it is the same on every system, because it is of the licence and notice only. A sleeping phone does not answer; wake it first.

A phone refuses a development package whose `versionCode` is lower than the installed one (`INSTALL_FAILED_VERSION_DOWNGRADE`); `./dev.sh deploy` then installs nothing. A phone that once took a numbered build (an update trial, see [application updates](app-update.md)) needs the same or a higher number: `PEERSUS_BUILD_NUMBER=N ./dev.sh android`, where the installed `versionCode` is `N + 1`.

The manual installer refuses a different package already installed under the same ID; it does not update or overwrite that package in place. Use a disposable profile for an isolated trial, or explicitly review and approve removal of the old implementation before switching. Do not rename the ID to install duplicate providers for the same service. Whatever way a package comes in, it starts with nothing allowed. Removed in the application's window, or replaced there or by the store, what the old package was granted is forgotten in the same step. `plugin-remove` only takes the package out of the store, and `plugin-install` marks the package it puts in place (`new-install` in its folder, the application's own like `disabled`): at the next start, before anything is registered, the application forgets what a removed name was granted and what a marked name was granted, then takes the mark away. Removed and installed again from the command line, the new package therefore does not inherit the old one's grants, and both commands say so. A running application does not start a package that was put under a name it already runs until it is itself started again: every launch, the restarts included, checks that the folder is still what was registered. To replace a development build of one's own extension and keep what it was allowed, use `scripts/deploy-plugin.py` below, which gives the grants back through the running application.

The application carries no extension. No package is built into the APK or installed at first start: every extension, upload and clipboard included, is installed by the person, from the store or by hand, through the ordinary package manager and its approval screen, with all host permissions initially denied. The archives a recipe produces are for that: they are also available for manual development installation, and they are not signed store submissions. Automated worker tests use synthetic file, consent and provider responses, not real credentials or a live upload. Device picker/consent behavior and live-upload verification remain separate acceptance checks.

Host interpreter tests include compiled generic SDK fixtures and bounded malformed/spam cases. Independent extension acceptance additionally runs their actual WASM guests through the host and device paths. Extension-specific integration tests take explicit module and manifest paths (`PEERSUS_CLIPBOARD_MANIFEST`, `PEERSUS_UPLOAD_MANIFEST`); the main workspace neither embeds nor needs those sources to compile. The 64-bit guests require a nightly toolchain with a built standard library; absence of such a build is not 64-bit acceptance.

Android HTTPS uses the unmodified upstream `rustls-platform-verifier` 0.7.1 and its official Android support artifact 0.2.0 (MIT OR Apache-2.0). The Maven source is pinned to upstream revision `1aa691352a5e0c215210dfc9a3c2f2078639dc91`; Gradle checks the support version against the workspace lockfile. JNI initialization precedes native application startup, and release shrinking retains the verifier bridge. This is trust-store integration, not a certificate-validation bypass.


Test with disposable application data or keep existing data in place. An uninstall is not a safe default for an upgrade. Use an emulator or a device that is explicitly available for testing; device availability does not authorize reading its private files.

## Behavioral verification

A change to relay selection needs a first-configuration test with no saved custom URL, not just a parser test. An Android layout change needs a narrow viewport, long translated labels, scrolling, keyboard visibility and touch-target inspection. A plugin action needs permission-denied, disconnected, running, cancel and failure states. A packaging change needs a staged executable and helper lookup, not only compilation.

Record the exact command, revision/worktree state, toolchain and result for meaningful claims. Compilation and unit tests do not prove a desktop window rendered or an Android application launched. A historical successful device run does not validate a later build.

## Interface demo and snapshot replay

`peersus-ui --demo` (a `demo` feature build) opens the same screens on one fixed synthetic snapshot: no owner is looked for or started, no socket, saved state, credential store or platform service is touched, and every command the window issues is refused with an explicit read-only answer. The fixture's own device is named so the window cannot be taken for a real one. `--demo-page=send|receive|peers|transfers|settings` chooses the first screen; `--demo-peers=N` (at most 64) and `--demo-transfers=N` (at most 1024) size the fixture; `--demo-language=FILE` shows it in the language of that language file (`languages/<release>/ro.po` of a copy of the languages branch, or a test file under `crates/peersus-ui/src/i18n/fixtures/`). `--demo-replay=HZ` (at most 120) changes the snapshot that many times a second, going round eight deterministic frames in which the moving transfers progress. Any other argument fails before an owner would be found.

```
cargo run -p peersus-ui --features demo -- --demo
cargo run -p peersus-ui --features demo -- --demo --demo-page=peers --demo-peers=5
```

`--demo-extension=DIR` shows an extension as it really runs instead of the fixture; given several times (at most eight), it shows each of them, so that their pages side by side, and with Appearance's "Extension pages in one place" the shared Extensions place and its switcher, can be seen (`--demo-page=extensions` opens that place). `DIR` is the folder of its unpacked package (the `plugin.json` and its program, as `scripts/package-native-extension.py` makes it, unpacked). The demo then starts two owners of its own inside the window's process, "Demo device" and "Demo peer", paired with each other, each with an identity made in memory (never the credential store), listening on the loopback address only with no relay or discovery, and keeping everything in a temporary folder removed with the window. Each package is shown to the first as a package the person chose: the window shows its licences, notice and the warning for a package from outside the store, and the person accepts there and allows on its Permissions what they want, exactly as for an install. Nothing is accepted or allowed by the demo itself. The extension's own program runs, through the helpers beside the window's program; the application's platform services (files, clipboard, input, HTTPS) are not given to those owners. `--demo-language=FILE` also applies, imported into that owner; `--demo-replay` and `--demo-probe` measure the fixture and are refused with it. The person's own owner, settings and data are never reached.

```
./dev.sh desktop                                    # the window and the helpers that run an extension, side by side
scripts/package-native-extension.py --source plugins/peersus-monitor/wasm --sdk . --target-dir plugins/peersus-monitor/wasm/target --out target/extension-preview --platform linux --id monitor
tar -xzf target/extension-preview/monitor-linux.tar.gz -C target/extension-preview
eval "$(python3 scripts/build_layout.py desktop)"   # the same target directory, so the helpers are beside the demo window
cargo run -p peersus-ui --features demo -- --demo-extension=target/extension-preview/monitor --demo-page=settings
```

Run with bare `cargo` and no layout, the window is built in `target/debug`, where no helper is: the extension is then shown as one that could not start.

Building the program with the feature replaces the ordinary development executable in the same profile directory until the next ordinary build. The `demo_window` example opens exactly the same demo as a program of its own and leaves the product executable alone; the measuring tools below use it.

### Headless screens

The `headless` feature (which includes `demo`) opens the demo's real root component on the toolkit's own headless runner: real layout, pointer and keyboard events and CPU drawing into memory, with no display, GPU or window system. The gate runs its screen tests; `demo_shots` writes a PNG of every screen and Settings tab, at the desktop size and at an upright phone's width, for before/after review of an interface change.

```
cargo test -p peersus-ui --features headless --lib -- demo headless::
cargo run -p peersus-ui --example demo_shots --features headless -- target/demo-shots
```

A headless picture is the real component tree, not the desktop window: it proves nothing about a GPU surface, frame pacing, a platform dialog or a phone.

### Measuring

`./dev.sh bench` builds the measuring programs in release, runs them and writes `target/bench/latest.json`. With `--container NAME` compilation uses that Distrobox and the programs run on the host.

```
./dev.sh bench --container NAME --save before
./dev.sh bench --container NAME --compare before
```

- `replay_bench` replays deterministic instances of the fixture through `View::of`, `view::refresh` and the snapshot serde codec the owner/window link uses, and opens each screen headless to time building, laying out and drawing it from nothing.
- `crates/peersus-net/examples/benchmark.rs` measures encrypted loopback transfer and history updates; it is unrelated to the interface.
- `demo_window --demo --demo-probe=SECONDS [--demo-probe-out=PATH]` is the real window measuring itself: time to the first presented frame, each later frame by stage (tree update, layout, render, present), and on Linux the process's CPU time, resident memory, context switches and disk bytes. It writes one JSON report and ends. The bench runs it three times: `window_quiet` (a screen on which nothing moves: what an idle window costs), `window_still` (rows that show work going on, on a snapshot that does not change) and `window_replay` (the snapshot changing 30 times a second).

`frames_per_second` in a window report is how often the window chose to redraw, not how often its data changed. The private X server does not pace frames, so a running animation redraws there as fast as the CPU allows: the number shows that a screen redraws and bounds what it can cost, and on a paced display the same animation redraws at the refresh rate. A screen may open with a short burst of frames and then rest; `steady.settled` counts only the second half of the run, and is the figure that says whether an open, untouched screen keeps redrawing.

The window runs on a private in-memory X server (`Xvfb`) so that every run sees the same screen and the session is not shown a window; `--on-screen` uses the session's display and its real graphics driver instead, and `--no-window` leaves the window out. `--save NAME` keeps a run in the untracked `benchmark-results/`; `--compare NAME` prints each shared number before, now and the change, and judges nothing: run-to-run noise is not known to the tool, so repeat a run before reading a small change as real.

To see where the time of a window run goes, record the same program under a sampling profiler, for example `samply record -- target/ui/<rust-target-triple>/release/examples/demo_window --demo --demo-probe=10 --demo-page=peers` after `./dev.sh bench` has built it (the release profile keeps function names). Sampling as an ordinary user needs `kernel.perf_event_paranoid` at 1 or lower, a system setting of the workstation; the bench itself needs no such permission.

All of this is synthetic and local: no owner process, extension, saved state, network beyond loopback or device takes part, and allocations are not counted. The numbers compare revisions on one machine and one display server. They are development tools; a product build contains none of them (`demo` and `headless` are off by default), and none is evidence of behavior on a real device.

## Languages

English is written in the code. The template of every message is `crates/peersus-ui/languages/messages.pot` (gettext; the English text is the message id); a test fails when a screen hands the translator a literal that is not in it, so a new message is added there in the same change. Every other language is a PO file on the repository's `languages` branch, never on `main`: `languages/<release>/<tag>.po`, beside `languages/<release>/languages.json`, the offer the application reads (which languages there are, the name each gives itself in its `X-Language-Name` header, how many template messages it translates, its size, and `machine`). A file whose header says `X-Translation: ai` is offered and listed with `machine: true` and tagged "AI translated" on the Languages page; `person`, or no such header, is not. The tag is not permanent: once a fluent speaker has checked the whole file, a review changes `ai` to `person` and the offer is written again.

```
cargo xtask languages-offer [languages/<release>]   # writes languages.json from the files
cargo xtask check-languages [languages/<release>]   # every file fits the template, the offer is current
```

The branch is laid out as the `extension` branch is: at its root `.github/workflows/languages-check.yml` (a copy of the one on `main`, which runs only there) and `languages/<release>/`. That workflow checks out `main` for the template and the checker and runs `check-languages` on the branch's files on every push and pull request; nothing on the branch is executed. Because it needs the template on `main`, the copy is added to the branch only once `main` carries the template; until then the branch holds only `languages/<release>/`, and its files are checked locally with `check-languages`. Locally `/languages/` is git-ignored on `main`; a copy of the branch adds it with `git add -f languages`.

The application fetches the offer and the file of the person's language (chosen, else the device's) through the same fetcher as the store, keeps them under its data directory, and shows English for whatever a file leaves out or translates with different placeholders. A file of an earlier release is used until this release's arrives. Language files are NOT signed: they change only the words shown, and a translation that does not fit its English text is dropped where it is read. A person can also import a file (Settings → Languages), which is kept beside the downloaded ones and takes precedence; the same page saves the template and opens the upload page of the release's directory on the branch, where a maintainer reviews a shared translation before it is offered.

## GitHub dependency and security automation

- **Dependencies are kept current by CI, not by hand** (owner's rule of 2026-11-15); only this project's own patches to vendored copies and personal hotfixes are done by hand.
  - `.github/dependabot.yml` proposes weekly updates for the Cargo workspace, pinned GitHub Actions and the Android Gradle project, each ecosystem as ONE grouped pull request (major updates included) against the `dev` branch. Cargo includes indirect dependencies; no private registry credentials are configured.
  - `Dependency roll into dev` (`dependency-roll.yml`, `scripts/dependency-roll.py`) runs after Dependabot on Mondays, or by hand. It makes `dev` from `main` the first time, squashes every open Dependabot pull request for `main` or `dev` onto it in turn (oldest first; one that does not apply cleanly is left out and named, never merged by guessing), and writes ONE commit authored and committed as the owner given by the repository variables `DEV_COMMIT_NAME` and `DEV_COMMIT_EMAIL` (without them it does nothing). It pushes `dev` without force, comments on each folded pull request, and starts `Quality gates` on `dev`. It never touches `main`, tags or publishes.
  - `Dependency news` (`dependency-news.yml`, `scripts/dependency-news.py`) keeps one issue listing every dependency with a newer release, **alpha and beta included** (Dependabot offers a pre-release only for a crate already on one), compared with the version the lockfile resolved, and every vendored copy under `vendor/` whose upstream has a newer release (those are re-made by hand with the changes their `PATCHES.md` lists). It only reports.
  - Publishing this project's crates to crates.io is done only on the owner's explicit command; no workflow or script publishes.
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
- `freya-core`, `freya-components`, `rav1e`: retain upstream implementation and image-format support; use the maintained `pastey` package under the existing dependency alias. The desktop UI consumer compiles with these patches. Platform behavior verification remains pending.
- `zbus` 6: migrate the application's direct Linux sleep and desktop-theme watchers to the maintained integrated wire-format API. Isolated private-bus sleep and initial/live theme tests pass. The root lockfile's strict dependency audit now passes locally; this is not hosted CI or a complete platform-runtime result.
- `mundy`: preserve portal appearance and accessibility preferences while migrating the Linux dependency and nested-variant match to zbus 6. Non-Linux implementation and upstream licensing are unchanged. See [patch provenance](../vendor/mundy/PATCHES.md); seven unit tests, including nested-variant wire decoding, pass. Actual desktop portal preference behavior remains unverified.
- Accessibility ports: retain `atspi`, `atspi-common`, `atspi-proxies`, `accesskit_atspi_common` and `accesskit_unix`, migrating wire imports and runtime features without disabling assistive-technology integration. Keep `zbus-lockstep` and its macros, using maintained `zbus_xml` 6 for XML signal-signature validation. All eight port manifests declare the required Rust 1.89 minimum. The desktop UI compiles; 1,141 common protocol/event tests, 23 XML validator tests and the proxy/adapter/cache suites pass. A private-bus registration regression verifies the Embed handshake, duplicate-registration semantics, cache installation and an actual cross-connection method call. Full screen-reader/platform behavior remains unverified. Each vendor directory records upstream revision, licenses and removal conditions.
- `keyring` 3.6.3: the application retains the existing Apple/Windows native and synchronous encrypted Secret Service backends. A [local feature-surface patch](../vendor/keyring/PATCHES.md) removes the unused asynchronous backend and its optional legacy serialization dependencies. Credential APIs, native implementations, lookup precedence and the legacy default-collection fallback are retained. This internal copy intentionally does not offer upstream async features; the application's configured features are preserved. A supported workspace lockfile refresh alone did not remove these optional dependencies. The regenerated root lockfile passes `cargo xtask audit` (`cargo audit --deny warnings`) locally with no vulnerabilities or warnings. Targeted regressions pass: two mock identity tests, five relay-token tests, sixteen upload tests, ten About/license tests and four locale/catalog tests. The permission-gated native credential test remains ignored; complete platform-runtime and hosted CI verification are not claimed.

These patches remove dependency chains rather than adding audit exceptions. The strict root-lockfile audit passes locally without suppressions. Application About, README and prototype security notes stay synchronized with the patch inventory and verification limits. A local passing audit is not hosted CI, an independent security assessment, or proof of complete runtime compatibility.

## Website

See [website staging](website.md). The website build is separate from the desktop and Android release-artifact workflow. Catalog signatures are checked in the application; static-site validation alone does not establish signature authenticity.
