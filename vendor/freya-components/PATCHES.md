# Maintained token-pasting dependency

Baseline: published freya-components 0.6.0-alpha.2, [revision `747c22dca8fc`](https://github.com/marc2332/freya/commit/747c22dca8fc031d4a5c7d24fcbf2c5458238a9b). Original source and license files are retained. Layout and gesture code uses the upstream positive, `f64` scroll positions from [#2587](https://github.com/marc2332/freya/pull/2587).

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

# Input role and re-press hook

`src/input.rs` retains `Input::role`, the optional `InputPressedAgain` callback that reopens a dismissed Android keyboard, and the field-anchored toolbar on a held press. `InputEditLabels` localizes Select all, Copy, Cut and Paste; masked inputs offer only Select all and Paste. The toolbar and key bindings run through alpha.2's single asynchronous edit-and-validate path, so clipboard paste completes before validation and publication.

Removal: drop when upstream exposes an input role, a re-press hook, and a configurable held-press toolbar with equivalent masking and localization.

# Edit menu on a right click

[#2598](https://github.com/marc2332/freya/pull/2598) now supplies right-button routing. `src/input.rs` uses that routing for the same localized field-anchored toolbar as a held press, rather than showing both this toolbar and the upstream English cursor-positioned menu. The lift that opened it still keeps it open.

Removal: drop the menu override when the upstream input context menu can use the same presentation, actions and localization as the touch toolbar.

# A touch lift after a pause flings nothing

Local change in `src/scrollviews/use_scroll_controller.rs` (`release_drag`): a drag released more than `FLING_RELEASE_TIMEOUT` (100 ms) after its last move does not fling. The touchpad path (`scroll_gesture_phase`) already had this check and the touch path did not, so a finger held still for a second and then lifted threw the page by the speed it had before the pause (seen in a log on the ROG: about 630 px at the lift after 0.8 s of rest).

Removal: drop when upstream's touch release has the same check.

# One scroll view per touch drag

Dropped the local ownership implementation: [#2538](https://github.com/marc2332/freya/pull/2538) includes both single ownership and the direction-aware selection added downstream, including giving the outer view a drag when the inner view is already at the relevant end. The production scroll views and ownership tests now come from upstream. Peersus still runs its longer drag trajectories in `tests/scrollview_nested.rs`, migrated to positive offsets and checked by the headless `toolkit_` tests.

# A touch across a slider scrolls the page

`src/slider.rs` joins the upstream `touch_drag` controller, whose `DRAG_THRESHOLD` is exposed within this crate. A slider registers room in both directions along its axis, using positive `f64` scroll coordinates. A drag across it goes to the page; a drag along it or a stationary tap changes the value. Mouse, keyboard and wheel keep their existing paths.

Seen on the NX (2026-11-17): flicking Settings up and down with the finger starting on the interface size slider set the interface to 185 %, and the page did not scroll.

Tests: `tests/slider_touch.rs` (new; the drag across fails on the unchanged slider: the page stays at 0) and upstream's `tests/slider.rs`, both run through `peersus-ui` like the scroll view tests.

Removal: drop when upstream's slider gives a touch to the page that scrolls it.
