# Input events are polled one by one again

Baseline: freya-winit 0.6.0-alpha.1, https://github.com/marc2332/freya, revision `6edb0dfbe887ee4a8f503403942286892ffbd002` (moved from 0.5.2 on 2026-11-15; the change below applied with an offset only). Original source and license files are retained.

Local change in `src/window.rs` (`process_buffered_events`): the runner is polled and the layout measured after EACH buffered input event, as in 0.5.1 and 0.5.0. 0.5.2 (commit 1445fa7f, "perf(winit): Dont poll runner and layout after each winit buffered event", #2528) handles the whole buffer first and polls once after it.

**Why:** the scroll views set the page from the position they drew at their last render, which the poll refreshes; with several touch moves in one buffer every move starts from the same stale position. Seen on a phone (ROG, frames of about 24 ms, so buffers of several moves): with the 0.5.2 behaviour a drag in Settings was "uncontrollable, jumps everywhere"; with this change and the accessibility fix of `freya-core` (see its `PATCHES.md`) the page followed the finger in 94 % of the steps of a drag, measured from a log (1479 steps). The change alone, without the accessibility fix, showed no difference: that fix hid it. The maintainer's reason for #2528 is a high-DPI desktop mouse; the cost here is one poll per touch event, which a phone reports a few hundred times a second at most.

Not done: a version limited to touch moves, which would keep #2528 for everything else (to be proposed upstream, with the measurements, if the owner agrees).

Removal: drop when upstream's scroll views do not depend on the position of the last render while a buffer is handled, or when #2528 is reverted for touch events.

# The file dialog crate one release ahead

Local change in `Cargo.toml`: `rfd` 0.18 instead of upstream's 0.17 (2026-11-15). The application asks for 0.18 (kept current at the owner's request); with upstream's 0.17 here both copies would be built. 0.18 changes no API this crate uses (its change log: a GTK4 backend behind a feature, GTK3 leak fixes, `set_show_hidden_files`, MSRV 1.92); default features are the same.

Removal: drop when upstream asks for `rfd` 0.18 or newer.

# Release the graphics driver ahead of window components

`AppWindow` in `src/window.rs` drops its driver before the runner, tree and pending render callbacks. OpenGL makes its own context current and releases or abandons it on driver drop. A component retaining a Skia GPU image can then be destroyed during window teardown without deleting a texture in another window's current GL context.

Vulkan explicitly lends its shared device lifetime to retained Skia resources through `WindowGpuDevice`. It stores only a weak reference in the window context, and updates it whenever the driver changes. Pending component and render callback resources drop before the last lease destroys the Vulkan device and Skia allocator, including on renderer recovery.

# Report the active backend and its fallback to the window

`src/config.rs` exposes the built-in backend order and a reactive `State<RendererStatus>` in each native window's root context. `src/drivers/mod.rs` records failed/skipped starts and honours `FREYA_RENDERER` only for Automatic. An explicit choice is attempted once before automatic fallback. Linux, Windows and Android try Vulkan, then OpenGL (ES on Android), then software; macOS uses Metal or software. Metal setup is fallible so a VM without Metal can use the software window. `src/window.rs` and `src/renderer.rs` update the same report on recovery and native window recreation. The `AfterPresenting` hook marks a skipped or rejected frame so it cannot count as a renderer benchmark sample.

Removal: drop when the upstream window can expose the actual backend, failed attempts and changes to consumers, and has the same automatic order. Verify explicit selection and fallback in native windows, not just in the headless runner.

# Use the Vulkan backend on Android's native surface

The Android engine build enables Skia's Ganesh Vulkan feature alongside GL. Ash's Android loader and `ash-window` use the Winit `ANativeWindow`; there is no new minimum SDK requirement or manifest filter. Android accepts RGBA swapchains and leaves rotation to the compositor when it supports the identity transform. Before a failed startup falls back to EGL, Vulkan must destroy any instance, device and surface it created. On Android `Suspended` drops the graphics driver within the Winit callback, as required by the native-window lifecycle; `Resumed` creates a new surface without replacing the component tree.

`src/drivers/vulkan.rs` waits for each acquired image in Skia before using it and lets Skia transition from the actual layout to `PRESENT_SRC_KHR`, even after pixel readback. Each present semaphore belongs to one swapchain image, so finishing rendering alone cannot allow reuse of a semaphore still owned by presentation.

Removal: drop the Android port when upstream ships the same Android Vulkan/GL build, surface lifecycle and synchronization. Reverify real presentations on a phone; compiling Ganesh alone is not evidence of Vulkan availability.
