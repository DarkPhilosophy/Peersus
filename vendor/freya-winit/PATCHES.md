# Input events are polled one by one again

Baseline: freya-winit 0.6.0-alpha.1, https://github.com/marc2332/freya, revision `6edb0dfbe887ee4a8f503403942286892ffbd002` (moved from 0.5.2 on 2026-11-15; the change below applied with an offset only). Original source and license files are retained.

Local change in `src/window.rs` (`process_buffered_events`): the runner is polled and the layout measured after EACH buffered input event, as in 0.5.1 and 0.5.0. 0.5.2 (commit 1445fa7f, "perf(winit): Dont poll runner and layout after each winit buffered event", #2528) handles the whole buffer first and polls once after it.

**Why:** the scroll views set the page from the position they drew at their last render, which the poll refreshes; with several touch moves in one buffer every move starts from the same stale position. Seen on a phone (ROG, frames of about 24 ms, so buffers of several moves): with the 0.5.2 behaviour a drag in Settings was "uncontrollable, jumps everywhere"; with this change and the accessibility fix of `freya-core` (see its `PATCHES.md`) the page followed the finger in 94 % of the steps of a drag, measured from a log (1479 steps). The change alone, without the accessibility fix, showed no difference: that fix hid it. The maintainer's reason for #2528 is a high-DPI desktop mouse; the cost here is one poll per touch event, which a phone reports a few hundred times a second at most.

Not done: a version limited to touch moves, which would keep #2528 for everything else (to be proposed upstream, with the measurements, if the owner agrees).

Removal: drop when upstream's scroll views do not depend on the position of the last render while a buffer is handled, or when #2528 is reverted for touch events.

# The file dialog crate one release ahead

Local change in `Cargo.toml`: `rfd` 0.18 instead of upstream's 0.17 (2026-11-15). The application asks for 0.18 (kept current at the owner's request); with upstream's 0.17 here both copies would be built. 0.18 changes no API this crate uses (its change log: a GTK4 backend behind a feature, GTK3 leak fixes, `set_show_hidden_files`, MSRV 1.92); default features are the same.

Removal: drop when upstream asks for `rfd` 0.18 or newer.
