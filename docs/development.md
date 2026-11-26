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
| `cargo xtask build` | Release desktop programs and their native host; rejects development diagnostics at compile time; no extension |
| `cargo xtask native-host` | Native extension helper only |
| `cargo xtask dev` | The same programs and native host in the development profile, with development diagnostics; no extension |
| `cargo xtask android-dev` | Community native libraries and host-signed debuggable APK; no deployment |
| `cargo xtask android` | Android release libraries, both flavors, unsigned packages and inspection |
| `cargo xtask upload-wasm` | Opt-in Upload-specific checks, including a worker test with synthetic host replies, before packaging a manual development archive; not a submission recipe, upload or installation |
| `cargo xtask flatpak` | Local unsigned Flatpak bundle/repository from source |


### Root shell helpers

The root helpers reuse these tasks rather than maintaining another build implementation. `scripts/build_layout.py` supplies the shared directory and target selection; explicit `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET` overrides are preserved. Helpers work from another current directory:

| Helper | Purpose |
| --- | --- |
| `./dev.sh desktop` | Desktop development build with its sibling helpers; no extension is built or installed |
| `./dev.sh android` | Optimized Rust Android libraries for both ABIs, packaged as a debuggable community APK |
| `./dev.sh desktop --container NAME` / `android --container NAME` | Compile in an existing Distrobox; Android debug signing remains on the host |
| `./dev.sh deploy msi hp nx rog` | Build and stage optimized desktop development programs, build the debug APK and the selected devices' installed extensions, then install and verify the app, restart it, and update only changed installed extensions |
| `./build.sh` / `./build.sh desktop` | Release desktop build; defaults Cargo output to `target/ui/` |
| `./build.sh android` | Existing unsigned Android release-preparation task; defaults Cargo output to `target/android/` |
| `./check.sh` | Full strict gate, including the advisory audit; defaults Cargo output to `target/ui/` |
| `./check.sh fmt` / `metadata` / `check` / `clippy` / `test` / `audit` | Run just that existing gate |
| `./store-demo.sh` | Build and open the real desktop UI with a fresh, isolated owner and the public signed test catalog |
| `./website.sh` / `./website.sh --empty-store` | Offline, loopback-only website preview with labelled store fixtures or the normal empty store |

All helpers support `--help` and install no prerequisites. `dev.sh deploy` uses `cargo xtask dev-deploy` for the optimized desktop development build and the Android development build tasks for the APK. It stages desktop binaries in a new temporary tree and takes the APK just produced by the build. `--container NAME` uses an existing Distrobox for Rust and WASM compilation (including for an older desktop that needs an older glibc); Android packaging and debug signing stay on the host. Cargo outputs must not be older than their dep-info inputs, and an older APK is not reused if the build fails to produce one.

Before any installation, deploy deduplicates ADB transports and checks the installed APK signers and the staged programs' glibc requirements on the selected online devices; incompatible signers or glibc stop installation. Offline devices are reported and the remaining targets continue. Android uses replace-install and verifies the installed APK; desktop deployment runs the per-user installer on MSI locally or HP over SSH and verifies the installed programs. It never uninstalls or clears application data. ROG is an installation target only, not a test device: deploy includes its restart, extension updates and installation checks, but no smoke tests or screenshots. VMs are for tests only, never deployment targets, and are stopped after use.

On Android the deploy stops and starts Peersus with its launcher intent. On Linux it restarts a current active login service or opens a window through the installed `peersus` launcher when no service owns it. Before installing on a desktop, deploy checks the login unit, the identity of the current owner and service process, and the graphical launch environment when a window needs to open. A preflight refusal leaves the old installation and processes untouched. Deploy never writes login units; the existing login switch is in the window. After a restart, deploy verifies the new process executables, checks the control socket and compares the installed library files with the staged bytes before reporting legacy files and updating extensions. `--app-only` skips the extension build and update, not the application restart or verification. See [extension deployment](#sandboxed-upload-extension) for consent flags.

For selected extension updates, the owner privately unpacks the staged archive before showing it. At confirmation it compares that reviewed manifest against the sound predecessor it already registered, and keeps only current grants whose declaration is unchanged, using the same comparison as a Store update. A refused or changed-on-disk predecessor keeps nothing; the result says so and names the dropped grants. The script provides no declaration, fingerprint or grant evidence. A forgotten and re-paired device never inherits its old grant. The replacement still needs its own agreement and outside-Store consent. Normal in-app review follows the ordinary manual replacement rule.

Development packaging uses Gradle's existing local debug signer; release packaging remains unsigned. None of these commands commits, pushes, publishes a release or deploys the website. The store launcher stops only its own processes when its window closes or it receives an interrupt; it leaves its disposable data under `target/store-demo/`. See [the demo scope and extension submission flow](extension-store.md#a-local-store-ux-demonstration).

### Explicit repository publication

`python3 scripts/deploy.py dev` and `python3 scripts/deploy.py release --version v0.1.0` **only print a plan**. They do not stage files, create commits or tags, or publish anything unless `--publish` is explicitly supplied. Use a clean committed source checkout and run the quality gates before publication; this helper is not a build, CI approval, binary release upload or website deployment.

Before the first versioned release, development publication creates a fresh, signed **single-commit `main` snapshot**. In one atomic push with exact leases, its predecessor becomes `backup/main-<old-short-sha>` and the new root becomes `main`. The helper verifies both refs; it never prunes backups. The first release starts the release history; later releases append signed commits and signed version tags. A release tag must match the committed workspace version and cannot replace an existing tag.

After a versioned release exists, development publication defaults to `dev` and refuses to overwrite `main`; `--target BRANCH` chooses another development branch. The source may be any local branch, but only its committed `HEAD` tree is published. Existing signing configuration is reused with explicit signature verification; signing failure never falls back to unsigned commits. Global configuration, the caller's branch, index, working tree and untracked files are left untouched.

`python3 scripts/sync-github.py` compares this checkout with GitHub in one go and, like the helper above, **only prints a plan** until `--publish` is supplied. It covers three things and nothing else: `main` (the development snapshot described above, taken from the committed `HEAD`; "in sync" when the remote tree is already that tree), every remote `submission/<id>-<date>` branch whose extension has a working copy in `plugins/peersus-<id>/` (one signed commit on the branch's tip that replaces only `extension/peersus-<id>/` with the working copy, without `target/` and other build output; a credential-like or linked file refuses the whole run), and `languages` (a fast-forward of the local `languages` branch, refused while its checkout has uncommitted files or does not continue the remote branch). Every push carries an exact lease and is read back; a branch that moved since the plan is left as it is. Local commits of a submission branch that only add to the remote one are kept under the new commit; no local branch, index or working tree is changed, so a checkout of a submission branch is brought up to date afterwards with a fast-forward. `--only main|extensions|languages` limits the run and `--message <id>="…"` gives a submission's commit its message. It builds and checks nothing: run the checks of what is published first.

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

Desktop deploy builds use a `dev-deploy/` child of the selected Cargo target directory (by default `target/ui/dev-deploy/<rust-target-triple>/release/`). This keeps their diagnostics-enabled programs out of the output directory used for release staging.

Android compilation already uses Cargo's target grouping: `target/android/{aarch64-linux-android,x86_64-linux-android}/release/`. Gradle inputs remain `target/android/jniLibs/{play,community}/<abi>/`; Gradle cache/intermediates/raw packages stay under `target/android/gradle/`. The ready-to-install community development APK is copied to `target/distribution/android/community/debug/app-community-debug.apk` only after packaging and the JVM transport checks pass. `dev.sh deploy` builds its own APK rather than accepting a path to an existing APK.

Bare Cargo retains its normal `target/<profile>/` behavior when no build environment is selected. For a helper-compatible staging shell, select the same environment first. No command migrates or removes older caches. Target directory names do not imply that Windows/macOS cross-compilation or packaging has been implemented.

Staging is deliberately fresh-output-only:

```sh
build_environment="$(python3 scripts/build_layout.py desktop)"
eval "$build_environment"
cargo xtask build
python3 scripts/stage-linux.py
```

If the default staging directory already exists, keep it: `python3 scripts/stage-linux.py --out NEW_DIRECTORY` stages the current binaries from the same build target without overwriting the previous distribution. This is for manual staging; `dev.sh deploy` always creates and uses its own fresh stage.

### Installing for one user (Linux)

`peersus install [--prefix DIR]` (`crates/peersus-app/src/user_install.rs`) installs the seven programs that lie beside it under `<prefix>/lib/peersus/`, each written beside the file it replaces and moved over it, reads them back byte for byte, and only then publishes `<prefix>/bin/peersus` as a relative symlink to the real CLI. The default prefix is `~/.local`. When the release's signed `SHA256SUMS` and `SHA256SUMS.sig` lie beside the programs, it verifies them as an update is verified, refuses unless every program is the one they name, and keeps them in `lib/peersus/` for the owner's check at each start ([putting damaged programs back](app-update.md#putting-damaged-programs-back)); without them it removes a list an earlier installation left. The desktop entry, the AppStream file, the icon and the licences are the ones the program was built with (compiled in), written under `share/`; the entry starts the symlink by its absolute path and forwards `%u`. It refuses from inside a Flatpak or an AppImage, from the installed copy itself, and when the launcher is something other than the link or a plain file; it starts or stops nothing and touches no application data. A plain-file launcher of an older layout becomes the link and is reported. `dev.sh deploy` installs each desktop with the staged tree's own `peersus install`. AppImage's `AppRun` runs its `usr/bin/peersus`; Flatpak and Nix start `peersus` from their package bin directory.

A first installation on a machine without Peersus is `site/install.sh`, served as `https://peersus.axxa.dev/install.sh` (`curl -fsSL … | sh`). It downloads the newest development release's `SHA256SUMS` and `SHA256SUMS.sig` that the site stages beside itself (see [application updates](app-update.md#a-release)), verifies the signature with the development channel's public key written in it (`openssl` 3), takes the one `peersus-<version>-<channel>.<build>-linux-<arch>.tar.gz` line, downloads that archive from the release `v<version>-<channel>.<build>`, checks it with `sha256sum -c`, unpacks it in a temporary folder and runs its `peersus install`. Nothing runs before every check holds.

`peersus` opens the adjacent window (including when given a supported store link). Use `peersus owner` for a headless session, or put optional preference flags before `owner`, for example `peersus --background owner`. The window starts `peersus owner` without a preference override. An unavailable Linux display is an error, not an implicit owner start.

On Linux/glibc, early window startup re-executes itself once with `MALLOC_MMAP_THRESHOLD_=4194304` if that variable is unset or empty. The default is set in the process's starting environment so it remains visible to the memory gate. Explicit values, including 4 MiB, are preserved. Only the window that applied the default marks its window-started owner; the two WASM hosts drop the injected default but keep a person's override. Native and standalone workers keep the inherited setting. An independently started owner receives no application default. An exec failure prints one warning and continues. Other platforms do not perform this re-exec. [E-156](engineering-log.md#e-156--extension-canvases-owner-memory-regression) measured 68.5–69.6 MiB window RSS after closing a canvas with the 4 MiB setting, versus 104.1 MiB with glibc defaults; measure again on the new startup path.

There is no cleanup of names left in `<prefix>/bin` by older layouts: they were development layouts only.

The window announces the application id `io.github.DarkPhilosophy.Peersus` (`peersus_ui::APPLICATION_ID`): the Wayland application id and the X11 class. A desktop matches a window to its entry, and so to its name and icon, by that id; the entry is named by it and carries it as `StartupWMClass`, and a test holds the three together. A window with another id is shown by the dock as an unknown application with no icon, whatever is installed.

### The macOS bundle

`scripts/stage-macos.py` makes `Peersus.app` on a Mac, from the programs built there, into `<target>/distribution/macos/`. It is what a person opens: one icon, the name in the menu bar, the window and the owner with its helpers side by side in `Contents/MacOS` (the window starts the owner that lies next to it). It runs with macOS's own `sips`, `iconutil` and `codesign` and refuses without them.

```sh
cargo xtask build                                                      # the four programs and the native helper, release
python3 scripts/stage-macos.py                                         # a Mac with Metal
python3 scripts/stage-macos.py --environment FREYA_RENDERER=software   # a virtual machine: no Metal device
```

What it is not: a release. The signature is ad hoc (no identity), nothing is notarized, another Mac's Gatekeeper refuses it, and there is no disk image. `cargo xtask build` passes on macOS as on Linux (Apple's toolchain links the native helper position independent by default and has no `-z` options, so `xtask` leaves those two flags out there; Skia's build needs `ninja` on `PATH`, which macOS does not provide), and the bundle carries all six programs: the script refuses when one is missing. The native helper carries no embedded `Info.plist` of its own, unlike the Rust programs. `--environment` writes `LSEnvironment`, which macOS applies only when the bundle is opened as an application (Finder, Dock, `open`), not when the program inside is run by hand. `--profile debug` stages a development build.

**What a system monitor shows for each program.** On Linux, the AppStream file (`packaging/linux/…metainfo.xml`) describes each shipped program. On Windows, the version resource identifies each Rust `.exe` to Task Manager. On macOS, the Rust helper binaries embed an `Info.plist` for their code-signing identifiers (`.core`, `.wasm-host`, `.wasm-canvas-host`, `.store-fetch`); the window uses the bundle's identity. `build-support/program_identity.rs` supplies those resources and uses `PEERSUS_BUILD_NUMBER` for the fourth Windows version component. The GNU Windows toolchain gets no resource.

The desktop owner chooses the Skia-linked `peersus-wasm-canvas-host` only for a WASM extension that imports `host` / `peersus.canvas` / `1` / `open`. Ordinary `peersus-wasm-host` has no Skia dependency. Both executables use the same interpreter and transport; neither runs inside the owner or window.

### Graphics renderer

In **Settings → Appearance → Graphics renderer**, choose **Automatic**, **Vulkan**, **OpenGL** or **Software** where the build includes them. The saved preference is read from the owner's snapshot when a window opens; changing it does not switch the existing desktop window. The row reports the backend actually in use and the reason for any fallback.

Automatic tries Vulkan → OpenGL → Software on Linux and Windows, the same order with OpenGL ES on Android, and Metal → Software on macOS. Android changes apply after restarting Peersus. An explicit choice is tried first, then the same automatic order without retrying it. `FREYA_RENDERER=software`, `opengl`, or `vulkan` only overrides **Automatic**; an explicit saved choice wins. Running a GPU API does not by itself prove hardware acceleration.

**Android renderer resize checks:** Read **In use now** in **Settings → Appearance → Graphics renderer** before testing; **Automatic** is a preference, not proof that Vulkan started. With Vulkan active, rotate portrait → landscape in both directions → portrait. Enter split screen, move its divider, and exit; on hardware that supports it, fold and unfold or resize the window. Lock and unlock in landscape. The UI should fill the available window with undistorted text, retain the current page, and respond to taps where controls are drawn. A successful native-library build does not verify these checks.

On desktop, **Compare renderers** opens one temporary window at a time, starting with the active backend as baseline. Each renders the same synthetic transfer-history replay, verifies a colored swatch in the warm-up, discards 12 warm-up frames, and measures 72 intervals between accepted presentations, main-thread CPU as a share of one core, and the process's resident memory (working set on Windows). Each child has an eight-second deadline; cancelling or leaving the page stops it. No owner, extensions or real content take part, and results are neither stored nor sent. The recommendation is for this scene and display only: drivers, their pacing, and the display server can change the result. Comparison is not offered in the Android UI.

For a single native sample on a disposable X display, use `xvfb-run -a env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET timeout 8s <peersus-ui> --renderer-sample Vulkan` (or `OpenGL` or `Software`). This private worker returns a JSON reading or an explicit failure; it does not accept legacy renderer names or reinterpret a fallback as a measurement of the requested backend.

For an isolated Android measurement, configure the SDK and NDK as described under [Android](#android), then run `FORCE_SKIA_BUILD=1 cargo ndk --platform 31 -t arm64-v8a build --locked --release -p peersus-ui-android --features renderer-probe --example renderer_probe`. This builds a NativeActivity library for a disposable debuggable APK, not the product APK. In the disposable APK's private `files/renderer-probe-mode`, write `Vulkan animated`, `OpenGL animated`, `Vulkan idle`, or `OpenGL idle` before launch so the same binary measures both backends. With no file the example defaults to animated Vulkan, or animated OpenGL ES if built with `PEERSUS_RENDERER_PROBE_GLES=1`. The animated mode samples 30 seconds after a 15-second warm-up and reports p50/p95/p99 intervals between accepted presents, render-thread CPU, resident memory and PSS when available. Both modes record the first accepted frame at launch, after surface recreation, and after a portrait/landscape transition. Read `files/renderer-probe.json` through `run-as` on the disposable APK; it stays alive for lifecycle and external CPU and graphics-memory measurements.

## Extension execution by distribution

This is the application's distribution policy. Do not describe all Android builds as WASM-only, and do not treat the Google Play restriction as a desktop restriction.

| Application distribution | External extension formats |
| --- | --- |
| Android from Google Play | **WASM only**, through the interpreter; no downloaded native `.so`, external process, DEX or JAR execution |
| Android APK from any other store or direct distribution | **Native `.so` and WASM**; use the external/community build with native-extension support enabled |
| Desktop | **WASM and native libraries (`.so` / `.dll` as available for the OS)**. The host can launch standalone executables where implemented; those are not a supported extension tier. |

The source of an Android download is not detected at runtime to grant extra privileges: the Play and external/community build flavors enforce their own execution policy. Every non-Play distribution must use the external/community flavor when providing the native-extension contract; changing a store name must not silently remove `.so` support or enable native code in a Play build.

This is not the [public store submission matrix](extension-store.md#what-a-package-must-be). That requires
WASM for Linux, Android and macOS; Windows WASM is optional. WASM is the only Store install format. Native
ELF libraries and standalone executables are optional manual-install alternatives, not recommended; process
extensions are unsupported. The project requires [more forms for its own extensions](#checking-first-party-extension-recipes).

Keep format support separate from verified platform readiness. The outstanding OS-specific implementation and acceptance checks are recorded in [release readiness](release-readiness.md); this contract is not evidence that every desktop OS has passed end-to-end testing. Native process/library separation does not by itself provide an adversarial OS sandbox; WASM resource access remains mediated by explicit host services and permissions.

### Checking first-party extension recipes

The six [first-party extensions](extension-store.md#first-party-extension-recipes) declare four WASM
archives (Linux, Android, macOS and Windows) plus the Linux native library and standalone process.
Location is the sole form exception: it ships four WASM archives and omits both Linux native ones
because its map requires host display-list rasterization, not just a native stream for finished frames.

From the SDK checkout, run `python3 scripts/check-first-party-extensions.py` before preparing a
submission. It checks the full known roster in the ignored `plugins/` working copies. To check just
the extension being submitted, pass `--source plugins/peersus-<id>`. The same command can read the
one source directory selected by an `extension` branch submission: the workflow could call the script
from its pinned trusted SDK on that directory after ownership verification, for first-party submissions
only. The workflow is not wired to call it today. The main checkout's gate runs its fixture tests, not
the live scan of ignored sources.

This check reads only `submission.toml` and `wasm/plugin.json`. It compares the declared artifact set and
ordered build, test and packaging arguments; it does not build, load or run a guest. The source build
still checks that every declared archive was produced, and the separate data-only validator checks its
contents. Neither a declared platform nor a validated archive substitutes for a run on that platform.

For a change to the guest protocol, check the exact SDK copies with
`python3 scripts/check-guest-glue.py plugins/peersus-<id>` and compile every execution form in the
recipe. Use scripted guest tests for partial grants, revocation while input is held, a neutral remote
refusal, and a genuine failed delivery. Verify that ticks and a restored grant do not replay the
stopped gesture. Test host arrangement geometry and saved placements separately from guest drawing;
those tests do not exercise the real capture portal or the native receivers.

## Android

Install Java 17, SDK 36, build-tools 36.0.0, NDK 27.2.12479018, `cargo-ndk` 4.1.2 and the `aarch64-linux-android`/`x86_64-linux-android` Rust targets as described by `.github/workflows/artifacts.yml`. `./dev.sh android` and `./build.sh android` take the SDK from `ANDROID_HOME` (or `ANDROID_SDK_ROOT`, or Gradle's `android/local.properties`) and derive `ANDROID_NDK_HOME`, `ANDROID_NDK`, the platform and the build tools from it (`scripts/build_layout.py`); a value already set is kept, and with no SDK named both refuse before building. Bare `cargo xtask android` reads only what is exported. A desktop Skia build cannot be reused as an Android native library.

`cargo xtask android` builds the Play and community libraries into `target/android/jniLibs/{play,community}/`, enables `native-extensions` only for community, builds community native hosts, invokes Gradle for both unsigned release APKs/AABs, and inspects native artifacts. It does not retrieve signing keys. Never publish unsigned development packages as installable releases.

`android-dev-native` enables the `development-diagnostics` Cargo feature explicitly, including the isolated WASM worker. The desktop `dev` and `dev-deploy` tasks enable the same support in the owner, window and both WASM hosts. Android development libraries and desktop deploy programs are optimized, but are not product releases. Both desktop and Android release preparation fail compilation if development diagnostics are enabled.

### Diagnostic contract

Development builds default to `warn,peersus::dev=trace`, including a desktop build installed by `./dev.sh deploy`. To explicitly enable the reviewed events, set `PEERSUS_LOG=peersus::dev=trace` in the **owner process's startup environment**. A newly launched window (`PEERSUS_LOG=peersus::dev=trace peersus-ui`) passes it to an owner that it starts, not to one already running. For an owner started by a login service, set it in that service's environment and restart the owner. `PEERSUS_LOG=warn` leaves only warnings and errors. A target-specific value adds to existing warnings and errors; a global level such as `debug` or `off` replaces that default. Product releases cannot emit these development events even with `PEERSUS_LOG=trace`.

Each reviewed event identifies a fixed stage and process, with numeric broker connection/generation/call/ticket IDs, service/method ordinals, closed outcome categories, byte counts or elapsed times. Broker admission, refusal, routing, reply, expiry and disconnect correlate with deferred platform work, interpreter validation/entry/traps, data handles and stream read/write/shutdown failures.

Desktop diagnostics go to stderr (the owner launched by the UI retains its log). Only the trusted WASM interpreter inherits that diagnostic sink in development; arbitrary native/process plugin stderr remains discarded. Development guests can opt into the scalar-only `diagnostic` import documented in the module ABI: closed stage/outcome vocabulary, bounded counters and 64 events per second per isolated generation, with no fuel renewal. Production hosts never emit those decisions. No payloads, clipboard text, typed input, filenames, device identities, endpoints, credentials or bearer claims are emitted. Unknown guest error text is reduced to a fixed category, never printed.

On an Android development build, read the buffered trace with `adb logcat -d -s 'peersus-dev:I'`. If the device suppresses INFO (for example `log.tag=S`), first run `adb shell setprop log.tag.peersus-dev INFO` on the development device; restore the previous tag setting after capture. The owner/UI and isolated WASM worker use this same tag; `role`, `process`, and system-wide monotonic `mono_ms` distinguish and order their events. The reviewed `peersus::dev` events still use the TRACE filter but are written at Android INFO. The sink accepts up to 64 ordinary records and reserves 8 canvas milestones per process in each one-second rate window; it may drop records under load. It does not open a file or grant any permission.

For a black canvas, look for `canvas-mount` and `canvas-first-frame` with the same `showing` number in the UI process. The first compositor draw reports elapsed milliseconds and the refused rasters and read errors observed by the UI before drawing. `canvas-present` in the worker reports the worker's own presentation outcomes, including refusals that never produce a raster; those are not counted as UI refusals.

The desktop writer has a bounded 1,024-line queue and may drop records under overload; `Guard::dropped_lines()` exposes the count. Android Logcat is also bounded by the OS. These are diagnostic traces, not a lossless audit trail. Capture a bounded reproduction window, keep it local, and do not attach private logs to issues. No diagnostic path grants permissions, bypasses consent or opens a network listener.

On desktop the owner started by a window shares `owner.log` with its trusted WASM workers. At about 16 MiB the writer copies the latest complete-line tail (up to 16 MiB) to `owner.log.1`, then truncates the active file **in place**, preserving inherited descriptors and the window's startup-error reader. Only one previous generation is kept. The 32 MiB combined target has the same 256 KiB size-check cadence and is not a hard cap on concurrent/direct stderr writes. If the archive cannot be written the active log is left intact and a later check retries. This retention works in release too.

Desktop development builds only: the owner, regular window and trusted Rust WASM hosts can write local text incidents in `diagnostics/` under the application's data directory. Keep the newest 20 panic/hang reports in total, each at most 4 KiB. On Unix the directory is `0700` and new files are `0600`; on Windows files inherit the user's local application-data permissions. Panic reports contain build information, a safe thread label and source location; raw messages and backtraces are not recorded. Desktop release builds still print the same place-only panic line to stderr without changing Rust's unwind or abort behavior. The development watchdog reports once when the owner loop or regular window event loop has made no progress for roughly 10 seconds; it includes the latest 32 reviewed trace stages when available, rearms on progress, and never kills or restarts anything.

**Crash capture (every build, opt-in).** Settings → Diagnostics → *Capture crashes* (`crash_capture`, CLI `--crash-capture`, off by default) makes the owner and each window start `peersus-diag watch <program> <data>/diagnostics` beside themselves and name it as tracer (`PR_SET_PTRACER`, which Yama `ptrace_scope` 1 requires); no signal handler or dump code runs in the programs. The helper sleeps in `waitpid` and wakes only for signals delivered to its parent. A signal the program catches is handed back untouched (Rust's handler turns a stack overflow into an abort); one that would end it (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGABRT`, `SIGSYS` with no handler) stops every thread, writes `crash-<program>-<ms>.dmp` (a minidump; read it with `minidump-stackwalk`) and a short `.txt`, then lets the program end exactly as it would have. Both files are `0600` in the `0700` folder; the newest 10 dumps are kept; nothing is sent anywhere. Linux only: elsewhere, under `ptrace_scope` 2/3, inside a sandbox that forbids `ptrace` (a Flatpak without `--allow=devel`) or with a debugger attached, the page says why capture is not running. While capture runs, a `SIGSTOP` reaches the program only as a pause that ends at once (`ptrace` gives no safe way to hold a group stop), and a debugger cannot attach. Release programs keep no debugging symbols, so a dump of one is read against the same build's symbols. `cargo test -p peersus-diag` drives real aborts and stack overflows through the same client the programs use.

On Linux/Wayland, independent pointers and their device-name labels are drawn by the owner, not by an open application window. If the inherited environment omits Xwayland authentication, the owner resolves the same-user local display socket's server and its exact authority file without changing global environment variables or the server's access policy. Authentication is never logged or persisted. A failed renderer is discarded and retried on subsequent input instead of leaving an invisible pointer cached for the remainder of the session.

### Sandboxed upload extension

The standalone guest in `peersus-upload/wasm` uses the same WASM runtime on Linux and Android. Both adapters use the application's shared file/HTTP authority: explicit file selection, caller-scoped immutable file snapshots, origin-bound upload consent and opaque credential/session handles. Neither credential values nor native file paths are returned to the guest. HTTP redirects and automatic retries are disabled. Cancellation and permission withdrawal release outstanding host work. Platform UI and encrypted credential storage remain platform-specific; the native upload extension's existing credential scope is not borrowed or migrated.

Extension sources are not part of `main`. Each is submitted from a `submission/<id>-<date>` branch under
`extension/<name>/`, as a pull request against the `extension` base branch. Normal distribution is through
the [Store](extension-store.md); manual laboratory installs are also supported. The ignored `plugins/<name>/`
directory holds the local working copy. Application builds do not build or ship those sources. The host
gate uses generic SDK guests; each extension's own recipe tests and packages its source independently.

For a local guest test and build, use the lockfile and a chosen target directory outside the extension source:

```sh
guest="$PWD/plugins/peersus-upload/wasm"
guest_target="$PWD/target/extension-upload"
cargo test --manifest-path "$guest/Cargo.toml" --locked --target-dir "$guest_target"
cargo build --manifest-path "$guest/Cargo.toml" --locked --release --target wasm32-unknown-unknown --target-dir "$guest_target"
python3 scripts/package-native-extension.py --source "$guest" --sdk "$PWD" --target-dir "$guest_target" \
  --out "$PWD/target/extensions/upload-wasm" --platform linux --id upload
```

Packaging does not install or approve the artifact. The separate opt-in `cargo xtask upload-wasm` helper
includes an isolated Linux worker/hub test of the compiled Upload guest with synthetic host responses.
It is not run by Upload's submission recipe and does not exercise the device picker or a live upload.

For a manual CLI installation, unpack the validated archive into a fresh directory and pass its `upload/` directory to `peersus plugin-install PATH_TO_UNPACKED/upload`; this command accepts a package directory, not a `.tar.gz` file. It does not approve permissions or read credentials. Restart the application with the updated host before testing, then approve the extension and configure its API key through its explicit action. A native and a WASM build use the same extension ID: they are alternative implementations, not two extensions to install side by side.

Run without options, `plugin-install` installs nothing: it prints the warning for a package from outside the store, the package's licence and notice, and the command that accepts them (`--accept <digest> --accept-outside-store-risk`). The digest is of the licence and notice as printed, so a package whose text differs by one word needs its own acceptance.

`dev.sh deploy` uses the same extension build and installer as `scripts/deploy-plugin.py all`. Before changing devices it reads each selected device's installed inventory, builds each required WASM once from its source under `plugins/*/wasm/`, and packages it for each needed platform. If a device's extension source, build, or packaging is missing or fails, deploy leaves that device's application unchanged. After the restart deploy replaces an installed extension only if the manifest or module bytes differ. A package with the same licence and notice already accepted on that device does not need a new decision. Review new text before passing `--accept-current-agreement`; deploy reports which package digest was accepted on which device. Review the outside-store warning before passing `--accept-outside-store-risk` on a device that has not yet accepted it. A sleeping phone may not answer; wake it before deployment.

For an extension-only operation, `scripts/deploy-plugin.py ID` builds and packages the selected extension by default and deploys through the running application (local, over SSH, or over ADB for debuggable Android). `--no-build` uses the existing build output and `--accept DIGEST` accepts only the agreement shown by `peersus plugin-install`. Both automated deployment commands report the owner’s permission decision and verify the installed bytes and running or disabled state. `--review-in-app` instead uses normal manual confirmation without development grant retention.

Android refuses a development package whose `versionCode` is lower than the installed one. Deploy builds with at least the highest `versionCode` found on the selected online phones, or with a higher number requested through `PEERSUS_BUILD_NUMBER` (`versionCode = PEERSUS_BUILD_NUMBER + 1`). If a phone changed after preflight and still refuses the APK, it is reported as failed without a restart or extension update.

**Memory acceptance after a deployment.** Run the Linux check on the installed release with the same launcher/allocator environment, extensions, effective renderer and window size on both builds. Open and close the same extension canvas once, return to Send with no contributed pages open, then start sampling when the youngest measured process is about five minutes old. Hold the page, window, and peer state fixed for the whole minute. Set `PX_RENDERER` to the active backend (`software`, `opengl`, or `vulkan`), not the configured preference, and `PX_WINDOW_SIZE` to the visible window content size in pixels (`WIDTHxHEIGHT`). Both are required with the page and peer conditions; the script does not query the app. For four paired devices and three online:

```sh
python3 scripts/memory-gate.py --check target/memory-baseline.json \
  --page send --extension-pages 0 --canvas opened-and-closed --paired 4 --online 3 \
  --renderer "$PX_RENDERER" --window-size "$PX_WINDOW_SIZE"
```

Only the installed executables are eligible (`--bin-dir`, defaulting to `~/.local/lib/peersus`). The owner must hold the selected profile's instance lock (`--data-dir`, defaulting to the current XDG profile); the window must be in the same owner/window ancestry and helpers must descend from the owner. If the window is detached from that tree or two candidates could fill a role, the check is inconclusive rather than guessing. Separate test copies do not take over a role.

The gate compares the median `Pss_Anon + SwapPss` from five samples 15 seconds apart against the baseline plus the larger of 2 MiB or 15%. RSS, PSS, `AnonHugePages` and starting ages are printed separately. If the range of huge-page readings at either tied median could tip a verdict, the check is inconclusive. Swapping or subtracting a whole huge page cannot grant a pass.

The gate refuses different stated conditions, profiles, role sets, starting ages more than two minutes apart for any role (no role younger than five minutes), or changed installed module/helper digests. It names a difference by role number, not by the extension's name or file path. It reads each verified process's startup environment only to distinguish the packaged 4 MiB `MALLOC_MMAP_THRESHOLD_` from no setting; it stores a boolean and refuses custom thresholds rather than storing their values. Keep any other allocator tuning unchanged. Overall build fingerprints still identify the owner/window builds, which may differ. `RESULT` and the exit status distinguish pass/recorded (0), growth (1), inconclusive (2), and a legacy RSS-only baseline (3).

Once the reference build and its workload are accepted, explicitly record it by replacing `--check` with `--record` above, using the observed conditions. Older baseline layouts lack the necessary comparison data and must be recorded again; `--check` never rewrites them. Phones are measured separately: `adb shell dumpsys meminfo <pid>` (PSS) for `ro.peersus` and each `ro.peersus:wasm:*` worker.

The offline command-line installer refuses a different package already installed under the same ID. Use a disposable profile for an isolated trial, or explicitly review removal before switching implementations; do not rename the ID to make a second provider for the same service. `plugin-remove` takes the package out of the store, and offline `plugin-install` marks its new folder `new-install`. At the next owner start, before registration, grants for removed and marked names are cleared and the marker is removed. Every worker launch checks that its package was not replaced outside this owner. For an owned development replacement use `scripts/deploy-plugin.py`; see the [grant rules](extension-store.md#manual-download-versus-store-identity).

The application carries no extension. No package is built into the APK or installed at first start: every extension, upload and clipboard included, is installed by the person, from the store or by hand, through the ordinary package manager and its approval screen, with all host permissions initially denied. The archives a recipe produces are for that: they are also available for manual development installation, and they are not signed store submissions. Automated worker tests use synthetic file, consent and provider responses, not real credentials or a live upload. Device picker/consent behavior and live-upload verification remain separate acceptance checks.

Host interpreter tests include compiled generic SDK fixtures and bounded malformed/spam cases.
Extension-specific integration tests take explicit module and manifest paths (for example,
`PEERSUS_UPLOAD_WASM` and `PEERSUS_UPLOAD_MANIFEST`); the application workspace does not need those
sources for ordinary builds. Such tests cover the exercised host path only; a real platform test is
still needed to claim device behavior. The 64-bit guests require a nightly toolchain with a built
standard library; absence of such a build is not 64-bit acceptance.

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

`./dev.sh bench` builds the measuring programs in release, runs them and writes `target/bench/latest.json`. With `--container NAME` compilation uses that Distrobox and the programs run on the host. `--sizes` also builds the product and adds the size of each of its programs.

```
./dev.sh bench --container NAME --sizes --save baseline
./dev.sh bench --container NAME --sizes --save change
cargo xtask bench-compare --base=baseline --head=change
```

- `replay_bench` replays deterministic instances of the fixture through `View::of`, `view::refresh` and the snapshot serde codec the owner/window link uses, and opens each screen headless to time building, laying out and drawing it from nothing. Beside each time is `allocated_bytes`, what one call asks of the allocator, and for a screen `retained_bytes_per_open`, what the heap still holds after a window of it was closed.
- `crates/peersus-net/examples/benchmark.rs` measures encrypted loopback transfer and history updates; it is unrelated to the interface.
- `demo_window --demo --demo-probe=SECONDS [--demo-probe-out=PATH]` is the real window measuring itself: time to the first presented frame, each later frame by stage (tree update, layout, render, present), the heap at the first frame, at the end and at its peak, and on Linux the process's CPU time, resident memory, context switches and disk bytes. It writes one JSON report and ends. The bench runs it three times: `window_quiet` (a screen on which nothing moves: what an idle window costs), `window_still` (rows that show work going on, on a snapshot that does not change) and `window_replay` (the snapshot changing 30 times a second).

Two kinds of number are in a report. The measuring programs report their own times and Rust heap, counted by the allocator in `crates/peersus-ui/examples/benchmark_support/counted_heap.rs`; the product binaries do not use it, and it does not count allocations made by graphics drivers. On Linux the benchmark also reads process CPU, RSS high-water mark, sampled peak PSS (shared pages divided between their users), peak anonymous memory, thread count, and window main-thread CPU from `/proc`. GPU metrics require driver counters.

`frames_per_second` in a window report is how often the window chose to redraw, not how often its data changed. The private X server does not pace frames, so a running animation redraws there as fast as the CPU allows: the number shows that a screen redraws and bounds what it can cost, and on a paced display the same animation redraws at the refresh rate. A screen may open with a short burst of frames and then rest; `steady.settled` counts only the second half of the run, and is the figure that says whether an open, untouched screen keeps redrawing.

The window runs on a private in-memory X server (`Xvfb`) so that every run sees the same screen and the session is not shown a window: there the CPU draws, no driver reports GPU memory, and drawing is in the CPU time and the memory. `--on-screen` uses the session's display and its real graphics driver instead, and `--no-window` leaves the window out. `--save NAME` keeps a run in the untracked `benchmark-results/`; `--compare NAME` prints the comparison with that kept run after the run and fails nothing.

To see where the time of a window run goes, record the same program under a sampling profiler, for example `samply record -- target/ui/<rust-target-triple>/release/examples/demo_window --demo --demo-probe=10 --demo-page=peers` after `./dev.sh bench` has built it (the release profile keeps function names). Sampling as an ordinary user needs `kernel.perf_event_paranoid` at 1 or lower, a system setting of the workstation; the bench itself needs no such permission.

The default replay is synthetic and local: no owner process, extension, saved state, network beyond loopback or device takes part. Its numbers compare revisions on one machine and one display server; `demo` and `headless` are off in a product build.

### Measuring a plugin canvas

On Linux, `cargo xtask bench --surface [--seconds=N]` builds `templates/canvas-guest` and runs the real desktop window, owner and WASM helper separately. It creates a disposable owner and memory-only identity, uses private Xvfb with the software compositor by default, and never installs an extension in the running application. Use `--programs=DIR` to measure the release directory from a container build without rebuilding (build the WASM template into `target/canvas-guest` first).

The benchmark opens the demo through the normal page offer, verifies that a drag produces a new guest image, and compares an idle interval with scenes of **30** and **1,500** stroked paths plus text. Both active intervals push the same wheel input at a **60 Hz target** and report actual distinct canvas frames presented, not input ticks or preview redraws. Linux `schedstat` provides CPU time for the main window thread and the window, owner, and helper processes. CPU per frame divides by the number of composited guest images; the helper number includes interpreter, native Skia, and transport work. It is not an isolated Skia timer.

The report is `target/bench/canvas.json` and the captured composed native window is `target/bench/canvas.png`. The result measures this local Linux software-renderer setup; it is not an Android, Windows, macOS, or hardware-GPU benchmark.

`--save=NAME` keeps the Canvas numbers for comparison. To include Canvas in the application suite, run `cargo xtask bench-fixture` once and put `templates/canvas-guest/plugin.json` and the resulting `canvas_guest.wasm` in a shared fixture directory. Pass `--with-surface=DIR` for both revisions. Both release directories must contain a Canvas-capable `demo_window` and `peersus-wasm-canvas-host`; use the same fixture bytes on both sides.

### Resource impact

`cargo xtask bench-compare --base=NAME[,NAME…] --head=NAME[,NAME…] [--markdown=PATH]` compares saved runs from the same machine. The Markdown table shows each metric's values, delta, observed spread and verdict. Run names must be distinct; use `PEERSUS_BENCH_RESULTS` to put the raw JSON and report in a dedicated directory.

| Overall verdict | Meaning |
| --- | --- |
| **PASS** | All scheduled, applicable metrics have complete samples and are within limits. |
| **REGRESSION** | At least one metric crossed a limit. A `REVIEW` row needs an explanation; a `REJECT` row blocks unless a maintainer explicitly accepts it. |
| **INCONCLUSIVE** | A baseline, run or required metric is missing, or the runs are not comparable. The command writes any available evidence and exits unsuccessfully. A missing sample is not zero and gets no delta. |

`xtask/src/judge.rs` is the source for thresholds and absolute noise floors. The comparison takes the best sample per side and suppresses relative changes within either side's observed spread. Some metrics (such as retained heap) use absolute growth limits instead. Spread is not a confidence interval. GPU counters unavailable on **both** sides of a software-rendered run and CPU per frame for a zero-frame idle phase do not apply. If a scheduled counter has only some of its samples, the result is inconclusive.

**Application PRs:** `Quality gates` runs the resource job after the Linux and Nix checks on open, reopen and each push of a same-repository PR to `main` or `dev`. Forks need the maintainer's `run:ci` label. The checkout's merge parents must match the event's base and head SHAs; those two exact revisions are built and measured, not the merge tree. Manual runs use the named `baseline` or the newest verified snapshot backup. The baseline's measuring tool is used if it supports the current comparison; the summary identifies a fallback.

**Runners and cost.** The repository is public, so GitHub's standard hosted runners cost nothing; every workflow uses `ubuntu-24.04`. The Linux gate restores Cargo dependencies (source-built Skia among them) and `cargo-audit` from the last `main` build (`Swatinem/rust-cache`, keyed on the toolchain and `Cargo.lock`). Only a manual run on `main` (Actions → Quality gates → Run workflow) saves it, so run one after a snapshot that changes `Cargo.lock` or the toolchain; PRs only read it, and a miss is a normal cold build. A maintainer may move only that job to another runner by setting the repository variable `CI_HEAVY_RUNNER` to its label (for example a free third-party runner whose benefit was measured on the same revision); without it, or after deleting it, the job runs on GitHub. The resource job, extension submissions and anything that holds a secret stay on GitHub's runner. Triage runs per PR: events on one PR fold into its newest pending run (the older one shows as cancelled), events on different PRs never cancel each other, and a review by a person (a thread reply, a comment) is skipped without a runner, since only a reviewer bot's review changes a label. Replies to several review threads therefore go in one submitted review, not one review per reply.

The two release builds are sampled three times each on the same runner, alternating which goes first. Canvas is scheduled only if both revisions expose the probe; a scheduled probe failure is inconclusive. There is no hosted hardware-GPU claim. The report appears in the job summary and with raw JSON in the run artifact. On same-repository PRs, the report also replaces the section between `<!-- peersus-benchmark:start -->` and `<!-- peersus-benchmark:end -->` in the author's PR description, or appends it when absent. It checks the live head and base first, does not change text outside the markers, and never posts a benchmark comment. Fork PRs keep the report in the job summary. The `impact:accepted` waiver of a measured blocking regression requires an explicit `run:ci` rerun; it does not waive a missing sample.

The updater skips ambiguous markers and any concurrent author edit it detects on re-reading the live PR. GitHub's PR update replaces the body as a whole and has no documented compare-and-swap; an author edit between the final read and update can still race.

The Program resources table reports the real `peersus-ui` window, the real `peersus-wasm-host` and (when both versions have it) `peersus-wasm-canvas-host`, and a `peersus-test-identity` owner built in release so no system credential store is used. It samples the isolated application with fixed repository WASM fixtures, a fresh profile, loopback only, and a private Xvfb window verified by `xwininfo`. The owner size is the ordinary `peersus` binary, not the test-identity build. Each entry keeps base, head, absolute delta, percent and verdict separate; a GPU field without driver accounting says why it was not measured. Results vary by runner. Extension submission runs put only the actual selected helper in this table; their instrumented demo roles and published package-size comparisons are shown separately.

The isolated executable directory does not contain the store/update fetcher, even when the measured release has a catalog key. The shipped fetcher's size is still measured from its original release directory.

To reproduce the executable workload, stage each side's ordinary release binaries first, then build `cargo build --locked --release -p peersus-app --features test-identity --bin peersus` in an isolated Cargo target directory and copy that output into the corresponding stage as `peersus-test-identity`. Run `cargo xtask bench-fixtures`, stage `plugin.json` and the built `.wasm` under `hello/` and (if supported on both sides) `canvas/`, and pass `--measure-programs=FIXTURE_DIR` alongside `--programs` to both sides. Never point the product sampler at a live profile or at third-party fixtures.

To reproduce with two local release directories, build each from its own checkout with `cargo xtask build bench-build`. From the checkout with this comparison tool, set `BASE_RELEASE` and `CHANGE_RELEASE` to the two absolute release directories, then run:

```sh
measure() { cargo xtask bench --programs="$1" --sizes --save="$2-$i"; }
for i in 1 2 3; do
  if [ "$i" = 2 ]; then
    measure "$CHANGE_RELEASE" change && measure "$BASE_RELEASE" baseline || break
  else
    measure "$BASE_RELEASE" baseline && measure "$CHANGE_RELEASE" change || break
  fi
done
cargo xtask bench-compare --base=baseline-1,baseline-2,baseline-3 \
  --head=change-1,change-2,change-3 --markdown=benchmark-results/impact.md
```

Use fresh run names or a fresh results directory. To check the workflows offline with an installed or cached Actionlint, run `actionlint .github/workflows/ci.yml .github/workflows/extension-validate.yml .github/workflows/extension-approve.yml`; this does not exercise GitHub event delivery, job permissions, artifact transfer or its approval UI.

**Extension submissions:** after the untrusted build and independent data-only validation, a separate job compares every package's size and measures the previous and submitted Linux WASM versions with three interleaved startup/idle samples on the same trusted release window, scratch owner and helper. This records startup to the authenticated helper, window main-thread and process CPU, helper CPU, and sampled RSS/PSS. It does not run submitted build code, native variants, extension actions, or grant capabilities; it does not measure an arbitrary extension's interactive pages, canvas animation, providers or devices. The app PR's Canvas benchmark is the fixed interactive workload. With no published predecessor, the verdict is **INCONCLUSIVE** and submitted absolute values are retained; a verified first release can proceed only if all three submitted samples completed. An update with missing samples cannot proceed.

For local extension comparison, `python3 scripts/extension-impact.py --candidate REVIEWED --baseline PREVIOUS --out RESULTS --runtime-inputs INPUTS` stages the exact reviewed Linux WASM from each archive and records all package sizes. Build the trusted harness with `cargo xtask bench-extension-build`; `cargo xtask bench --extension=INPUTS/change --package-report=RESULTS/extension-change.json --no-build --save=extension-change-1` records one sample. Repeat for `INPUTS/baseline` and both sides' remaining samples, then compare their saved names as above.

The protected extension admission run shows the version transition, source PR and commit, successful validation run, platform/execution/archive SHA-256 inventory, benchmark verdict and licence/notice comparison **before** requesting approval. It rechecks the exact evidence after approval; a newer published predecessor requires fresh validation. Approval signs and stages the reviewed artifacts; it never merges, publishes, installs or grants permission.

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
- The full quality gate audits dependencies on pushes and pull requests. `Security audit` (`security.yml`) runs `cargo xtask audit` (the same policy: warnings remain errors) and `cargo deny check` against `deny.toml` (advisories, allowed licences, registry-only sources) when a manifest, the lockfile or the policy changes, daily and on manual dispatch, so a newly published advisory fails without a source change. The scheduled and manual runs also repeat both checks after `cargo update`, to show what the next dependency update would bring in. It does not build the application or upload a dependency snapshot.
- These configuration files become active only after publication to the repository's default branch. Local YAML/schema validation is not a successful hosted workflow run. Scheduled jobs use Actions runner time.
- Dependency graph, Dependabot alerts, security updates and Dependabot runner selection are repository settings, not enabled by `dependabot.yml`. Use GitHub-hosted runners unless a real private-network requirement justifies registered self-hosted runners. Selecting a labeled runner without a matching runner prevents those jobs from running.
- Automatic dependency submission is separate from Dependabot update PRs. Gradle's resolved transitive graph can use automatic submission on GitHub-hosted runners; Cargo does not use GitHub's automatic-submission workflow. Do not register a runner merely to silence an unused-label warning.
- CodeQL/code-scanning and secret-scanning availability depends on repository ownership, visibility and the licensed products. Do not enable a paid product, grant new workflow write permissions, or claim coverage merely because its settings page exists. Verify actual analysis runs before calling it active. Never expose secret-scanning alert payloads in logs.
- For a CodeQL finding, record the scanned commit and analysis configuration, then trace the reported flow and check whether the path runs in the actual target and feature set. Distinguish shipped code (including vendored code), test fixtures and tools. Verify a fix with the relevant local tests and a fresh hosted analysis of the published commit; a local patch or a missing file on a different revision is not alert closure.

Reference: [Dependabot options](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference), [automatic dependency submission](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/submit-dependencies-automatically), [CodeQL](https://docs.github.com/en/code-security/concepts/code-scanning/codeql/codeql-code-scanning).

### Local dependency compatibility patches

The dependency gate is not currently claimed green. Each local patch retains its published source's license files and records the upstream revision, exact change and removal condition in `vendor/<package>/PATCHES.md`:

- `netwatch`: keep its transport-facing API while backporting the maintained netlink set and the cross-target `netdev` requirement. Network transport regression tests pass; that is not an end-to-end connectivity claim.
- `winit`: Wayland now uses the published `sctk-adwaita` 0.12; the old font-renderer backport is gone. The [local Winit manifest](../vendor/winit/PATCHES.md) selects `skrifa` as the default renderer for native window titles. The platform copies keep [Wayland tablet compatibility](../vendor/winit-wayland/PATCHES.md) and [Android NativeActivity teardown and recreation](../vendor/winit-android/PATCHES.md). Verify those behaviors on the respective platforms.
- `freya-core`, `freya-components`, `rav1e`: retain upstream implementation and image-format support; use the maintained `pastey` package under the existing dependency alias. The desktop UI consumer compiles with these patches. Platform behavior verification remains pending.
- `zbus` 6: migrate the application's direct Linux sleep and desktop-theme watchers to the maintained integrated wire-format API. Isolated private-bus sleep and initial/live theme tests pass. The root lockfile's strict dependency audit now passes locally; this is not hosted CI or a complete platform-runtime result.
- `mundy`: preserve portal appearance and accessibility preferences while migrating the Linux dependency and nested-variant match to zbus 6. Non-Linux implementation and upstream licensing are unchanged. See [patch provenance](../vendor/mundy/PATCHES.md); seven unit tests, including nested-variant wire decoding, pass. Actual desktop portal preference behavior remains unverified.
- Accessibility ports: retain `atspi`, `atspi-common`, `atspi-proxies`, `accesskit_atspi_common` and `accesskit_unix`, migrating wire imports and runtime features without disabling assistive-technology integration. Keep `zbus-lockstep` and its macros, using maintained `zbus_xml` 6 for XML signal-signature validation. All eight port manifests declare the required Rust 1.89 minimum. The desktop UI compiles; 1,141 common protocol/event tests, 23 XML validator tests and the proxy/adapter/cache suites pass. A private-bus registration regression verifies the Embed handshake, duplicate-registration semantics, cache installation and an actual cross-connection method call. Full screen-reader/platform behavior remains unverified. Each vendor directory records upstream revision, licenses and removal conditions.
- `keyring` 3.6.3: the application retains the existing Apple/Windows native and synchronous encrypted Secret Service backends. A [local feature-surface patch](../vendor/keyring/PATCHES.md) removes the unused asynchronous backend and its optional legacy serialization dependencies. Credential APIs, native implementations, lookup precedence and the legacy default-collection fallback are retained. This internal copy intentionally does not offer upstream async features; the application's configured features are preserved. A supported workspace lockfile refresh alone did not remove these optional dependencies. The regenerated root lockfile passes `cargo xtask audit` (`cargo audit --deny warnings`) locally with no vulnerabilities or warnings. Targeted regressions pass: two mock identity tests, five relay-token tests, sixteen upload tests, ten About/license tests and four locale/catalog tests. The permission-gated native credential test remains ignored; complete platform-runtime and hosted CI verification are not claimed.

These patches remove dependency chains rather than adding audit exceptions. The strict root-lockfile audit passes locally without suppressions. Application About, README and prototype security notes stay synchronized with the patch inventory and verification limits. A local passing audit is not hosted CI, an independent security assessment, or proof of complete runtime compatibility.

## Website

See [website staging](website.md). The website build is separate from the desktop and Android release-artifact workflow. Catalog signatures are checked in the application; static-site validation alone does not establish signature authenticity.
