# Maintained token-pasting dependency

Baseline: published freya-core 0.6.0-alpha.2, [revision `747c22dca8fc`](https://github.com/marc2332/freya/commit/747c22dca8fc031d4a5c7d24fcbf2c5458238a9b). Original source and license files are retained. All local changes below carry forward; the multi-root layout change in [#2547](https://github.com/marc2332/freya/pull/2547) still has an inner-only invalidation that stops at the changed node.

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

# Text stays inside its box

Local change in `src/elements/label.rs`: a label's default maximum width is 100% of its parent, and its default text overflow is an ellipsis. Text therefore wraps inside its box instead of growing past it (a single word longer than the box is broken), and a label given `max_lines` ends with `…` when cut. Explicit widths, overflow and line limits set by a caller still win.

Removal: drop when upstream bounds label width by default, or replace with an equivalent theme default; re-check every screen for text extending past its container.

Correction in `src/elements/label.rs`: a one-line label (`max_lines(1)`, default alignment) whose width follows its own text (`max_width(Size::Inner)`) is laid out at unbounded width even though the ellipsis is now on by default. Measuring an inner-sized box offers the label almost no width, so the default ellipsis cut such words to "P…" or to nothing (the transfer filter chips, a row that scrolls sideways). Removal: with the patch above.

# Focused nodes are revealed entirely

Local change in `src/data.rs` and `src/accessibility/tree.rs`: focusing a node scrolls its scrolling ancestors unless the node is entirely inside every viewport clipping it (upstream scrolled only when it was completely out of view). A field the soft keyboard half covers is therefore brought fully into view.

Correction in `src/accessibility/tree.rs` (`scroll_to`), found with a log on the ROG: (1) a focused node that is itself a scroll view reveals nothing (the scroll views ask for the focus on every move of a drag, and the rule above made a scroll view partly under a bar or the keyboard never "entirely visible", so every move sent it a wheel of the hidden part: 410 wheels of source `Custom` in a few minutes of dragging, 153 of 189 jumps of the page right after one); (2) a reveal that moves nothing is not sent (a wheel of zero makes the scroll view set the position it last drew, which undoes the move of the drag since the last frame; 255 of them). Revealing a field above the keyboard is unchanged.

Narrowing in `src/accessibility/tree.rs` (`scroll_to`), found with the same log: with no finger down, 10 wheels of source `Custom` came about 10 ms after a lift, each replacing the fling that had just started by a small move that showed the element under the finger entirely (the page stopped "as if at its end", in the part of Settings that is dense with switches and buttons). The entire-reveal rule now applies only to text fields (`is_ime_role`: the keyboard is what covers them); any other focused node scrolls its ancestors only when it is completely out of view, as upstream does. Also only along an axis the scroll view can scroll (`inner_sizes` larger than `area`): several of those wheels had `dy = 0` and a sideways delta, from rows a few pixels wider than the page, which moves nothing in a page that scrolls only vertically but still stops the fling the scroll view runs (its wheel handler stops the animation for a `Custom` wheel); the owner's description was a page that goes from the 8th to the 9th part of Settings and back to the 8th, as if held.

Landing in `src/accessibility/tree.rs` (`process_updates`): the focused node is revealed only when the focus landed on the node that was asked for (a request for a node that cannot take the focus leaves it where it was). Upstream revealed whatever node had the focus after ANY request, and the scroll views ask for the focus on every move of a drag; so when the scroll view itself did not take the focus, each move revealed the node that had it before: the last switch, button or field touched. The page was pulled back to it as soon as it was out of view, and a very strong scroll passed it: the owner's "force field" at the 8th part of Settings, where the device lists are. Reported from the phone; not logged.

Removal: with the rule above.

# Focus can use an inset viewport without clipping the page

The split desktop dock paints over a full-size scroll viewport so the page shows in its gaps. The same viewport cannot tell keyboard or assistive focus that a control is covered by the dock. `src/elements/extensions.rs` adds the opt-in `focus_viewport_insets` builder; `src/data.rs` carries the optional focus ancestors, and `src/tree.rs` reads their inset bounds from the existing layout. `src/accessibility/tree.rs` reveals new focus in the clear region and refuses activation that still lies outside it; `src/events/measurer.rs` guards delivery to the focused target without filtering global keys. No caller without an inset changes focus behavior, and an inset does not affect layout, drawing or pointer hits.

Regression: `tests/focus_viewport.rs` is run from the Peersus headless suite (this vendor crate is outside the workspace), against the same toolkit event router. It checks non-integer display scale, absent insets, reveal before activation, an unreachable target after the inset changes, and a page already scrolled down that has to scroll back to clear a top inset. The last uses upstream's positive scroll offsets ([#2587](https://github.com/marc2332/freya/pull/2587)); the reveal limit once assumed negative ones and silently refused the move.

Removal: use the upstream equivalent when it can separate the keyboard/assistive viewport from the painting and pointer viewport.

# An inner-layout change reaches the ancestors

Local change in `src/tree.rs` (`mark_layout`, exported from the prelude with `DiffModifies` as `LayoutDiff`): a change in how a node arranges its children (spacing, padding, alignment) is marked dirty with no reason instead of `DirtyReason::InnerLayout`. torin stops at a node marked `InnerLayout` without marking its parent, yet an auto-sized node changes size with its arrangement; and a child added in the same render stopped its own propagation at that already dirty node. Ancestors kept stale sizes and following siblings overlapped until something forced a full relayout. A plain mark still propagates only through parents whose size depends on their content; a reorder keeps `DirtyReason::Reorder`.

Regression: `crates/peersus-ui/src/layout_regression.rs` reproduces it with torin's own test tree through `mark_layout` (a card stayed 30 px instead of 242 px before the change).

Removal: drop when upstream torin propagates `InnerLayout` to ancestors that depend on their content, keeping that test passing.

# A custom painter for an image

`src/elements/image.rs` lets a caller substitute its painter without changing image sizing, clipping or sampling. Peersus uses it for continuously updated canvas rasters: the painter writes new pixels into the same GPU texture rather than passing every raster image to Skia's image upload cache. Regular image elements still use the original path.

Removal: drop when Freya offers an image draw callback with the same layout and clipping guarantees.

`WindowGpuDevice` provides an optional lease for a native device used by a component's retained Skia resources. The window renderer updates the weak owner when the driver changes.

Removal: drop the lease when Freya provides an equivalent way to retain a device for cached resources across driver recovery and window teardown.
