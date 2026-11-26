# Input events are polled one by one again

Baseline: published freya-winit 0.6.0-alpha.2, [revision `747c22dca8fc`](https://github.com/marc2332/freya/commit/747c22dca8fc031d4a5c7d24fcbf2c5458238a9b). Original source and license files are retained. The patches below use Winit 0.31 and Freya renderer messages from [#2513](https://github.com/marc2332/freya/pull/2513) and [#2589](https://github.com/marc2332/freya/pull/2589).

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

Removal: drop with an equivalent upstream driver/context drop order and device lease. Verify cached canvas resources across recovery and window teardown.

# Report the active backend and its fallback to the window

`src/config.rs` exposes the built-in backend order and a reactive `State<RendererStatus>` in each native window's root context. `src/drivers/mod.rs` records failed/skipped starts and honours `FREYA_RENDERER` only for Automatic. An explicit choice is attempted once before automatic fallback. Linux, Windows and Android try Vulkan, then OpenGL (ES on Android), then software; macOS uses Metal or software. Metal setup is fallible so a VM without Metal can use the software window. `src/window.rs` and `src/renderer.rs` update the same report on recovery and native window recreation. The `AfterPresenting` hook marks a skipped or rejected frame so it cannot count as a renderer benchmark sample.

The existing Android `WindowResumed` event also reports when driver recreation began and which backend reattached. The opt-in renderer probe uses these fields to time the first accepted frame after recreation and to reject a fallback rather than measuring the wrong backend.

Removal: drop when the upstream window can expose the actual backend, failed attempts and changes to consumers, and has the same automatic order. Verify explicit selection and fallback in native windows, not just in the headless runner.

# Use the Vulkan backend on Android's native surface

`Cargo.toml` enables Android Ganesh Vulkan alongside GL. Ash and `ash-window` use the Winit `ANativeWindow`; neither the minimum SDK nor manifest filters change. `src/drivers/vulkan.rs` accepts Android RGBA swapchains, leaves rotation to the compositor where the identity transform is supported, and releases partial Vulkan resources before EGL fallback. `src/renderer.rs` uses Winit's surface callbacks: [#2570](https://github.com/marc2332/freya/pull/2570) now drops the driver in `destroy_surfaces` and creates a new one in `can_create_surfaces` without replacing the component tree. The local GPU lease/status and opt-in probe measurements build on those callbacks.

`src/drivers/vulkan.rs` requests FIFO and the surface's minimum image count on Android. It reuses one acquire fence and waits only for the acquired WSI image, not the previous rendering submission. Ganesh transitions from its actual image layout to `PRESENT_SRC_KHR` and signals the per-image present semaphore in the same flush, removing the empty queue submission. [Reacquiring and waiting for that same image](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html) proves the previous present wait is complete. Desktop keeps its existing synchronization. Android purges the unused Ganesh cache when giving up the native window; a component's explicitly leased texture can still hold its device until its owner is done.

Removal: drop the Android port when upstream ships the same Android Vulkan/GL build, surface lifecycle and synchronization. Reverify real presentations on a phone; compiling Ganesh alone is not evidence of Vulkan availability.

## Resize Android Vulkan buffers from the live window size

`src/drivers/vulkan.rs` chooses the current physical `Window::surface_size()` within Vulkan's image extent limits on Android. Android's [Vulkan WSI](https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-16.0.0_r1/vulkan/libvulkan/swapchain.cpp#948) queries the surface's cached default width and height, which [Surface refreshes when a buffer is queued](https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-16.0.0_r1/libs/gui/Surface.cpp#1188). A resize can reach layout first; recreating from stale capabilities can retain a portrait buffer under landscape layout.

The driver also checks the window size before acquiring an image, independently of Winit's resize event. A successful recreation records the requested size so duplicate notifications do not rebuild the swapchain. Android can report `VK_SUBOPTIMAL_KHR` on presentation for a rotation with the identity transform, but not for a size-only change: neither acquire nor present `SUBOPTIMAL` alone now triggers Android recreation. Resize, out-of-date and surface loss still do. The test in `src/drivers/vulkan.rs` exercises old capabilities on both portrait/landscape transitions and a size-only change; native rotation, window resizing, and resume need a device. The desktop extent policy is unchanged.

Removal: drop when upstream reconciles Android's live window size and Vulkan swapchain extent without recreating on every frame in landscape.
