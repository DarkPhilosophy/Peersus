# Maintained token-pasting dependency

Baseline: freya-components 0.6.0-alpha.1, https://github.com/marc2332/freya, revision `6edb0dfbe887ee4a8f503403942286892ffbd002`. Original source and license files are retained. (Moved from 0.5.2 on 2026-11-15 at the owner's request. Re-applied as a patch: offsets only, except four places merged by hand where upstream had changed the same lines: in `input.rs` the shared edit callback, since the field's value became optional in 0.6 (`InputValue`); in `scrollview.rs` and `virtual_scrollview.rs` the release, move and press of a touch drag, where upstream only renamed `set` to `set_if_modified` on the state this copy replaces with `touch_drag`. #2538 is still not merged upstream, so the copy below is still needed.)

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

# Input role and re-press hook

Local change in `src/input.rs`: an optional explicit accessibility role (`Input::role`, default unchanged), so a field can say it is a search, address or secret field independently of masking; `InputPressedAgain`, an optional global-context callback run when an already focused input is pressed again (the application's Android adapter provides it to reopen a soft keyboard the person dismissed while the field kept focus; without it nothing changes); and an edit menu opened by holding a press on any input (Select all, Copy, Cut, Paste; a masked input offers only Select all and Paste). Each menu entry runs the same edit as its keyboard shortcut through the input's single edit path, so validation and masking rules are identical. Labels default to English; `InputEditLabels` in the global contexts translates them.

Removal: drop when upstream exposes an input role and a re-press or keyboard-request hook.

# Edit menu on a right click

Local change in `src/input.rs`: the edit menu above also opens on a right click (`on_secondary_down`), as a desktop person expects, with the same entries and rules as the held press; the lift that ends the click keeps it open and a later lift closes it, exactly as for the hold. Nothing else about the input changes.

Removal: drop when upstream's input has a context menu of its own.

# A touch lift after a pause flings nothing

Local change in `src/scrollviews/use_scroll_controller.rs` (`release_drag`): a drag released more than `FLING_RELEASE_TIMEOUT` (100 ms) after its last move does not fling. The touchpad path (`scroll_gesture_phase`) already had this check and the touch path did not, so a finger held still for a second and then lifted threw the page by the speed it had before the pause (seen in a log on the ROG: about 630 px at the lift after 0.8 s of rest).

Removal: drop when upstream's touch release has the same check.

# One scroll view per touch drag

Local change in `src/scrollviews/touch_drag.rs` (new), `scrollview.rs` and `virtual_scrollview.rs`: the scroll views of a window agree on which one of them a touch drags. Upstream 0.5.2 lets every scroll view under a touch start its own drag: `pointerdown` bubbles through all of them, each then listens to `CaptureGlobalPointerMove`, and the first to handle a movement calls `prevent_default()`, which cancels that same event for the other scroll views (`EventName::get_cancellable_events` contains the event itself). The order between capture listeners is not defined, so a scroll view inside another one starves its parent.

Seen on the ROG (diagnostic log of 2026-11-12, one touch held for nine seconds on a row of a device list, each row being a horizontal scroll view inside the vertical page): the row, which had nothing to scroll, handled 525 of 551 movements and its position stayed at 0; the page handled 26, about one a second, and at each one moved by everything the finger had travelled since the previous one (for example from -6891 to -6675 when the finger had gone from 337 to 553). On the screen: the page does not follow the finger, then jumps. Touches that started outside a row scrolled the page one to one.

The change: a touch that lands on scroll views is one gesture of the window (a state in the root scope). Each scroll view under it registers as `pointerdown` bubbles, so innermost first. When the touch has moved 2 px, the drag is given to one of them (rules below). Only that scroll view follows the touch, cancels the movement for children and takes the lift; the others do nothing for that touch. A scroll view with nothing to scroll no longer swallows the press of a child when the finger slides a little.

Upstream: proposed as marc2332/freya#2536 (2026-11-12). The maintainer closed it on 2026-11-13 and reopened the same commit, with the attribution kept, as marc2332/freya#2538, with three commits of his own on top: the scroll view's accessibility id identifies it instead of a counter, `press` takes the content and viewport sizes, the tests are one table in `tests/scrollview.rs`. Same rules, shorter code. #2538 is open, not merged; marc2332/freya#2467 (a descendant taking a touch over from the scroll views) is stacked on it.

State of this copy since 2026-11-13: the four files in `src/scrollviews/` and `tests/scrollview.rs` are #2538 at its commit `43b1fb88`, plus one local addition that is not upstream. It was offered to the maintainer as a patch in a comment on #2538 on 2026-11-13 (not as commits); no answer when this was written. The owner tried this build on the ROG the same day and found no scroll fault; the NX did not get it (not reachable over adb).

- **A scroll view that is at its end does not take a drag that goes further that way.** In #2538 the innermost scroll view that overflows along the axis of the drag takes it, also when it is already at that end: a list inside a page, scrolled to its bottom, holds every touch that starts on it and pushes up, and the page under it cannot be moved from there. Here each scroll view registers how far it can still go back and forth (from the position it was rendered at), and the drag goes to the innermost one that can still scroll the way the touch moves; when none can, the rules of #2538 apply unchanged (innermost that overflows along the axis, then the other axis). The owner is still chosen once per touch: reaching the end in the middle of a drag does not pass it on.

Tests: upstream's `tests/scrollview.rs` (with three rows added to `nested_scroll_drag_ownership` for the addition) and `tests/scrollview_nested.rs`, run by this workspace through `peersus-ui` (`cargo test -p peersus-ui --features headless --lib -- toolkit_`), because the vendored crates are not workspace members. In upstream's workspace: on #2538 alone the first added row fails (the page stays at -200 while the finger pulls the inner view, already at its start, further back); with the addition all 136 `freya-components` tests pass.

Removal: drop the copy when a toolkit release contains #2538. The addition then either is upstream too or has to be carried on that release.

# A touch across a slider scrolls the page

Local change in `src/slider.rs` (and `DRAG_THRESHOLD` in `src/scrollviews/touch_drag.rs` made visible to the crate): a touch on a slider no longer sets the value where it lands and stops the press. It joins the window's touch gesture (`touch_drag`, above) as a candidate that moves only along the slider, so a drag along it is the slider's and moves it as a mouse does, a drag across it is the page's, which scrolls and leaves the value alone, and a touch lifted where it landed is a tap that puts the value there. The mouse, keyboard and wheel are unchanged.

Seen on the NX (2026-11-17): flicking Settings up and down with the finger starting on the interface size slider set the interface to 185 %, and the page did not scroll.

Tests: `tests/slider_touch.rs` (new; the drag across fails on the unchanged slider: the page stays at 0) and upstream's `tests/slider.rs`, both run through `peersus-ui` like the scroll view tests.

Removal: drop when upstream's slider gives a touch to the page that scrolls it.
