# Engineering log

**Stage: private development, not a release.** The application and non-Test extensions remain at 0.1.0. A version number alone does not identify a development build: use the source revision and artifact digest.

[E-001–E-150: full historical log, measurements and unresolved items](engineering-log-through-E150.md). The archive preserves the working copy, including entries not yet published at rotation. Old results apply to their named builds, not automatically to this tree. For release decisions see [Go/No-Go](go-no-go.md); for reproducible commands see [development](development.md).

## E-151 — 2026-11-19 · Application UI and PR evidence

| Path | Verified | Limit |
|---|---|---|
| Platform watchers | A slow sample previously prevented the window from reading the last known value. The regression timed out before the fix and passed after moving sampling outside the channel write lock. | Not a measurement of phone scroll latency. |
| Peers and Send | Headless rendering on desktop and phone-sized layouts: presence remains separate, Peers shows each status below it, Send shows the first plus the number of others. Offline peers do not retain the text in the view model. | Synthetic state; not a two-device visual test. |
| Extension metadata | The installed package takes precedence over a catalog release. Missing local metadata never turns the offered version or author into installed metadata. | The UI test uses mismatched fixture versions. |
| Test submission | PR #17, `7b0388a`, hosted validation run `44460572553`: success, candidate and reviewed artifacts present. | Validation is not store publication, installation or release. |
| PR benchmark | A local comparison emitted the Markdown table. PR baseline = the base parent of the test merge; three release runs per side on the same runner. Missing samples are not zero, cannot produce a delta and do not pass as a comparison. | Hosted application workflow not exercised on a published revision containing these changes. Historical replay numbers are not today's before/after result. |

## E-152 — 2026-11-19 · Development milestone

- **Upload progress:** a TLS fixture reproduced a progress notification after an unusable response. After the fix, invalid responses and repeated acknowledged ranges do not advance the indicator. The guest also rejects a provider receipt with an inconsistent byte count. File bytes acknowledged and action completion are separate states.
- **Version alignment:** Control's local manifest, Cargo package and regenerated lock entry now say 0.1.0. Test remains 0.1.3. No downgrade of an installed extension was attempted.
- **Git snapshots:** `scripts/deploy.py dev` plans a fresh root commit and a `main-<old-short-sha>` backup in a single leased, atomic push. Tests cover collision refusal. Old backups are not automatically removed. No application snapshot was published during this work.
- **Artifact contract:** the build validator used to accept a WASM module plus a `.so` without a process binary. A regression using actual compiled archives reproduced that acceptance. The submission now requires a Linux library and process for at least one common architecture, independently of the store's WASM-only install policy.
- **Comparable structural analysis:** the pinned Codeflow analyzer (`b70af01`), with the workflow exclusions on both sides, scored the `f4caa9e` base 74/C (570 files, two high flags) and the working snapshot after E-152's progress and backup changes 84/B (578 files, no high flags). The count of layer flags rose from 85 to 86. This was a local working-tree comparison, not a hosted score or a runtime benchmark.

## E-153 — Know what the protected approval signs

Older deployment rows use the application's commit title and workflow run number, neither of which identifies a candidate. The updated flow names the requested `id@version`, PR, source commit and successful validation run. The preview checks the requested identity against every package in the pinned review before exposing the protected job. Its summary separates signing and staging from merging, publishing, installing and granting permissions.

Verified locally with the **hosted** review for PR #17 at `7b0388a`, source CI `44460572553`: six archives, `test@0.1.3`, previously published version `0.1.2`. The preview emitted no approval subject for an intentionally mismatched version. The Environment name and human-review protection are unchanged. No historical deployment was approved, rejected, cancelled or relabelled; presenting the updated screen on GitHub requires the workflow change on the default branch.

## E-154 — Offline map, placement consent and restart selection

- **Location 0.1.0:** built six archives (WASM for four platforms, plus a Linux library and process), checked their common contract and passed each agreement through `xtask check-manifest`. All 44 guest tests passed. A local socket harness then drove the compiled WASM, library and process independently through open, map on, zoom in, whole world and map off. All 15 returned pages passed the application's `Node::parse`; zoom changed the drawn path and reset restored it. No location lookup was requested. These are host/guest checks, not a device interaction or visual acceptance.
- **Navigation consent:** the owner's test now writes the actual global setting and submits the extension's primary-page offer. A manifest default of `true` without a saved choice is insufficient; explicitly on admits it and explicitly off refuses it.
- **Restart selection:** an edit to an existing extension preference used to put its ID on both sides of the restart list. Added, edited and removed entries are now each selected once, unchanged ones never, and the old values need not be copied. The owner applies the restart list only after the preferences have been saved successfully.
- **Structural score:** after the settings fix and consolidating 14 identical callback-identity comparisons, the same local scan still scores 84/B (578 files, 5,337 inferred connections, 56 of 7,566 functions marked unused, no high security flags). Its `calcHealth` starts at 100 and only subtracts. In particular, the unused list includes a JNI notification callback that Java actually invokes. Neither removing callbacks nor changing the scan exclusions is a valid fix.
- **Extension boundary:** the [architecture](architecture.md#what-belongs-in-an-extension) now explains how a guest owns a feature and exports it to other clients, using the offline map to distinguish local computation from privileged host I/O. This describes existing capabilities; it introduces no new ABI, storage or permission.
- **Measured guest work, not a whole-app claim:** an open, unchanged world map recalculated coastline geometry on every timer event. The guest now retains only the last projected outline (at most 16,000 characters); zoom or pan invalidates it and turning the map off drops it. In six runs per build on the same Linux release WASM worker, 180 timer events with synthetic time and no device positions used median **810 → 105 ms of worker CPU**. Median per-run round-trip p50 was 5.59 → 0.75 ms and p95 10.54 → 2.28 ms; peak worker RSS was about 5.3 MiB in both. Every sequence returned identical pages. No UI, GPU or live location provider participated.
- **Installed versus rebuilt guest:** on the test workstation the navigation setting saved both values but the installed guest never offered its page. Given the same synthetic control input, it issued no startup configuration read and only listed peers on opening the page. The rebuilt guest read its setting and, when true, issued `peersus.pages.set` for the correct primary copy. Replacing the application executable alone does not update an independently installed extension.
- **Upload 0.1.0:** the anonymous action and the keyed provider are guest policies. The host owns file selection, consent, HTTPS and saved credentials. A result-only origin does not authorize a request there. The compiled guest passed both local TLS pipelines (keyed multipart/chunks and anonymous single request), with no external upload. The six archives were validated; a Windows archive is not evidence of a Windows run.
- **Broker admission:** serialized control messages still have the same 48 KiB limit, including JSON escaping. The size check now counts exactly what the serializer writes instead of building and discarding another byte vector under the hub lock. A release microbenchmark (six interleaved samples per build) measured median 125 → 67 ns for a small query and 2,714 → 1,686 ns for a 64-row event. This is the cost of the check, not end-to-end call latency. The exact boundary and one byte over are covered.
- **Disabled means stopped:** an installed extension that is off can keep new settings without starting a worker. Restart is not an alias for enable. Tests exercise the real supervisor with an isolated identity and check that only explicit enable brings the worker back.
- **Independent multi-caller check:** on the working tree, 16 registered broker channels made 192 interleaved, authorized reads across eight methods. Before the dispatch change, 192 answers requested a global rebuild. Afterward all 192 replies still matched their callers and none requested a rebuild. Withholding one action updated the watcher and only that extension's action list; repeating it requested no rebuild. This test has no frontend, worker processes or real network traffic, so it does not establish whole-system stress latency.
- **Release owner plus 16 release WASM processes:** six concurrent rounds completed all 96 actions and 2,304 configuration reads. The sampled interval including startup and shutdown was 1.312 s; CPU was 115.61 ms for the owner and 219.82 ms summed across workers. Sampled peak RSS was 20.75 MiB for the owner and 66.94 MiB as the sum of worker peaks, not a claim about simultaneous total memory. No GUI or GPU was involved; this is not a before/after comparison.
- **Linux session lifecycle:** on a disposable profile with an in-memory identity and an isolated manager stand-in, an existing owner was not interrupted, the waiting service took over after orderly shutdown, two independent clients connected, and the service stopped cleanly. A separate regression checks that a temporary manager failure neither claims ownership nor exits as if startup were disabled.
- **Custom status, displayed rather than inferred:** two isolated identities paired over loopback with relays disabled. A sender published a test label, the receiving owner reported it on its connected peer, and the installed desktop viewer displayed it below `ONLINE` in both Send and Peers. Clearing it removed the row without changing presence. Separately, the other physical desktop received the test extension's active status from the workstation; its screen was not captured. This tests remote status presentation, not a plugin-independent personal-status editor.
- **Application deployment:** all five installed Linux executables matched the release stage on both selected desktops. The first Android APK had version code 1 versus installed code 24 and was refused; the numbered development build (code 25) installed and the APK read back from each selected phone matched the build. No downgrade, uninstall or data clear was used. Application deployment did not install optional extensions.

## E-155 — Globe, regional detail and native rendering cost

The offline Location guest projects its own land, boundaries, regional roads and cities. No private fixes or network map services participated. In three interleaved local release runs of the actual WASM worker, the median regional zoom reply was 2.93 ms (p95 3.12 ms); returning to a cached view took 1.33 ms (p95 1.39 ms). Corresponding worker CPU for 40 changes was 120 ms and 50 ms. Median worker peak RSS was 11.50 MiB, versus 5.32 MiB for the globe without the regional atlas. The opt-in cache holds up to eight previous views on this installation, independently of the active drawing; clearing it preserved the active frame and the next tick still reported zero retained bytes.

At 920 × 600, the optimized Freya CPU renderer drew the regional scene in 1.90 ms median (2.15 ms p95) with cartographic paths batched into one raster when they fit. The same release executable, identical geographic scene and separate rasters took 3.31 ms median (3.77 ms p95): three interleaved trials, 120 forced settled frames per scene and trial. Globe rotation in the compiled worker was a separate measurement: 4.95 ms median, 5.13 ms p95 for changing geometry, 60 drag events per trial. These are not compositor, GPU or phone frame times. Regional transport data is not street-address coverage.

## E-156 — Extension canvases: owner memory regression

Status: **fixed on desktop and in isolated Android tests; phone verification pending.** Extensions now render their own pixels in their own process (WASM `canvas`, Skia in the helper). After this landed on the workstation, the owner's memory rose from ~40–50 MiB to 142–290 MiB RSS once Location's canvas page had been opened, and stayed there after the page was closed (owner's observation, then smaps: ~242 MiB anonymous mappings in the owner, 0 live raster bytes after close in a reproduction; window ~46 MiB anonymous retained). The extension Monitor was checked by the owner and is not the cause (owner 49 → 49.2 MiB, then flat). No earlier Monitor growth incident is recorded in these logs.

Measured on the workstation before the fix, with ~22 MB frames (3392 × 1600), 30 wheel steps through the local page-event path: owner 510 ms CPU, window 370 ms, Location helper 430 ms. The cause: the owner relayed every full frame from helper to window, allocating a new raster per frame across its threads, and glibc kept the freed arena pages. Separately, helpers that never draw rose from ~5 to ~10–12 MiB RSS (PSS ~2 MiB: shared Skia pages). `scripts/memory-gate.py` is a manual, Linux-only acceptance check run on the workstation after a deployed build has started (it measures running processes, so it cannot run before installation); phones are measured separately with `dumpsys meminfo` PSS. A build that fails it is not kept as the accepted state.

**Fix (desktop):** the owner now only authorizes a direct helper → window frame stream with a one-use token and never reads pixels; the window enforces frame bounds, rate and the 64 MiB retention itself; two reusable buffers; `MALLOC_MMAP_THRESHOLD_=4194304` set once in `packaging/linux/peersus-launch` so frame-sized blocks return to the OS. Matched 30-input run at 3000 × 1500: owner CPU 2,270 → 60 ms; owner RSS after two open/close cycles 294.0 → 54.0 MiB with zero raster-sized allocations; window back to its fresh 68.5–69.6 MiB after closing (default allocator: 104.1 MiB). Helpers that never open a canvas run `peersus-wasm-host` without Skia (3.27 MB): RSS 9.27–9.45 → 4.08–4.41 MiB, PSS 1.76–1.86 → 1.01–1.06 MiB. On the workstation after deployment and restart, before opening a canvas: owner 42.2, window 101.7, light helpers ~6 MiB RSS.

**Fix (Android and GPU, in isolated tests):** Android shares the two-buffer pool (zero retained raster bytes after unmount); the compositor updates one texture per canvas in place and purges unlocked GPU resources on unmount; the window's Skia GPU budget is 64 MiB (was 256). Skia budgeted memory after unmount in a 30-raster test: OpenGL 6.32 → 0.29 MiB, Vulkan 6.04 MiB → 4 KiB. On the owner's phone before this fix: main process 219 MiB PSS after leaving the canvas, 94.5 MiB of it Graphics. The phone has not been re-measured with the fix.

## E-157 — Extension runtime cost at the first reply

Built the six local extensions and both runnable templates as release `wasm32-unknown-unknown` guests. `templates/guest` is shared ABI source, not a third runnable template. Samples used fresh Linux x86-64 release helpers on an i7-9700KF (Rust 1.96.1), with source-built Skia in the canvas helper. The local benchmark selects the helper from the manifest's `peersus.canvas/open` import, as the desktop launcher does. The last Test row in each table deliberately forces the canvas helper for the **same** non-canvas guest and request.

**Startup:** from invoking process spawn to receiving the first complete, ticket-matched answer, with the request queued immediately after the control handshake. Times and helper memory are medians of 15 interleaved runs per row. OS caches were not cleared.

| Extension / template | Helper | First call | Startup (ms) | RSS / PSS (KiB) |
|---|---|---|---:|---:|
| Clipboard | Light | `share` (refused without a peer) | 10.20 | 4,628 / 2,688 |
| Control | Light | Open `layout` | 25.18 | 5,980 / 4,031 |
| Location | Canvas | Open `where` | 30.23 | 31,212 / 25,774 |
| Monitor | Light | Open `mon` | 30.21 | 5,144 / 3,182 |
| Test | Light | Open `test` | 19.34 | 5,176 / 3,223 |
| Upload | Light | Open `key` | 9.31 | 4,720 / 2,775 |
| Canvas template | Canvas | Open `demo` | 19.66 | 10,556 / 5,157 |
| Hello template | Light | `greet` | 7.30 | 4,324 / 2,396 |
| Test (same guest, forced helper) | Canvas | Open `test` | 25.52 | 10,236 / 4,834 |

**First page open:** from sending `Open` to receiving its first complete answer on a separate fresh helper, after startup messages had settled. RSS/PSS is an immediate `/proc/<pid>/smaps_rollup` sample at the reply, not a peak. Latency and memory use the uninstrumented helpers. Fuel is the sum of interpreter turns from delivery through the next receive after the reply, including turns split by host-service responses. A separate release replay used the existing Location harness's opt-in `Transport::turn_usage`; all three fuel runs per row agreed and returned the same page as the timing runs. Each reply passed `Node::parse`.

| Extension / template · declared page | Helper | First reply (ms) | Fuel | RSS / PSS (KiB) |
|---|---|---:|---:|---:|
| Control · `layout` | Light | 12.25 | 13,477,943 | 5,980 / 4,063 |
| Control · `remote` (Android-only route, replayed on Linux) | Light | 13.29 | 21,172,948 | 6,112 / 4,128 |
| Location · `where` | Canvas | 3.86 | 1,558,812 | 31,340 / 25,805 |
| Monitor · `mon` | Light | 16.30 | 27,893,238 | 5,120 / 3,186 |
| Test · `test` | Light | 16.31 | 27,567,819 | 5,180 / 3,219 |
| Upload · `key` | Light | 3.03 | 871,698 | 4,672 / 2,707 |
| Canvas template · `demo` | Canvas | 4.37 | 840,069 | 10,564 / 5,134 |
| Test · `test` (same guest, forced helper) | Canvas | 18.94 | 27,567,819 | 10,264 / 4,845 |

- **Helper comparison:** for the same Test page the canvas run used 5,084 KiB more RSS and 1,626 KiB more PSS at the reply, with identical guest fuel. Its first-reply measurements overlapped the light run's (9.37–30.05 vs 7.82–34.04 ms) on a busy workstation; these timings are not regression thresholds. Location's page-open PSS included a median 22,804 KiB of anonymous PSS.
- **Scope:** Clipboard and Hello declare no page. The fixture uses no paired devices, saved credentials, live system or location readings, clipboard contents, or network; Control Layout receives one synthetic display. This measures the Element reply, not rasterization: no canvas size or frame request was sent. It neither measures the application/window nor establishes Android performance.
- **Local evidence:** `target/e157/runtime.py`, `measure.rs`, `build-probe.sh`, `runtime.json` (255 timed replies and 24 fuel replays), and `build.sha256` (both release helpers and all eight guests; digests checked after the run).

## E-158 — Renderer selection and matched Android measurements

**Observed on Linux:** The window reported the active backend and the fallback when Xvfb could not start a requested Vulkan window. On Xwayland, the Settings comparison measured Vulkan, OpenGL and Software against the active Vulkan baseline; cancellation stopped its temporary worker. Saving Software left Vulkan active until the window reopened, then the row reported Software. The local strict `./check.sh` passed.

**Android product compiled; physical probe observed:** The ARM64 product library and the disposable probe linked the same source-built Skia configuration with Vulkan and OpenGL ES. On NX, a no-permission test APK ran the same synthetic scene in fresh processes, alternating three Vulkan and three OpenGL ES runs. The table reports medians of the per-run statistics. The APK was uninstalled and confirmed absent.

| Backend | Frame interval p50 / p95 | Render thread (% of one core) | RSS / PSS (MiB) |
|---|---:|---:|---:|
| Vulkan | 16.36 / 19.07 ms | 18.74% | 184.85 / 48.41 |
| OpenGL ES | 16.46 / 18.58 ms | 20.46% | 184.32 / 49.37 |

For this scene, Vulkan used 8.4% less render-thread CPU; OpenGL ES had a 0.49 ms lower p95. This is a single-device comparison, not a general ranking or a product memory baseline. The Android product APK was not installed or exercised; the comparison UI is desktop-only. See [the renderer development notes](development.md#graphics-renderer) for the probe and sampling method.
