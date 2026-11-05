# Maintained token-pasting dependency

Baseline: freya-core 0.5.0-rc.13, https://github.com/marc2332/freya, revision `f796f8ff46f43864aa0258a9187fd73990a8a8c8`. Original source and license files are retained.

Local change: replace only the `paste` dependency with `pastey` 0.2.3 using its documented Cargo package alias. Source calls remain unchanged. This removes the unmaintained dependency covered by RUSTSEC-2024-0436 without suppressing the advisory or removing application functionality. The packaging manifest and its source manifest describe the same dependency.

Compatibility reference: https://docs.rs/pastey/0.2.3/pastey/ documents this exact alias migration. Compatibility still requires compiling the actual consumers and running their behavior tests; the documentation alone is not verification.

Removal: drop this local patch when a compatible released upstream package uses a maintained replacement. Run the strict cross-target advisory gate and desktop/Android regression checks after removal.

# Text stays inside its box

Local change in `src/elements/label.rs`: a label's default maximum width is 100% of its parent, and its default text overflow is an ellipsis. Text therefore wraps inside its box instead of growing past it (a single word longer than the box is broken), and a label given `max_lines` ends with `…` when cut. Explicit widths, overflow and line limits set by a caller still win.

Removal: drop when upstream bounds label width by default, or replace with an equivalent theme default; re-check every screen for text extending past its container.

Correction in `src/elements/label.rs`: a one-line label (`max_lines(1)`, default alignment) whose width follows its own text (`max_width(Size::Inner)`) is laid out at unbounded width even though the ellipsis is now on by default. Measuring an inner-sized box offers the label almost no width, so the default ellipsis cut such words to "P…" or to nothing (the transfer filter chips, a row that scrolls sideways). Removal: with the patch above.

# Focused nodes are revealed entirely

Local change in `src/data.rs` and `src/accessibility/tree.rs`: focusing a node scrolls its scrolling ancestors unless the node is entirely inside every viewport clipping it (upstream scrolled only when it was completely out of view). A field the soft keyboard half covers is therefore brought fully into view.

Removal: drop when upstream reveals partially visible focused nodes.

# An inner-layout change reaches the ancestors

Local change in `src/tree.rs` (`mark_layout`, exported from the prelude with `DiffModifies` as `LayoutDiff`): a change in how a node arranges its children (spacing, padding, alignment) is marked dirty with no reason instead of `DirtyReason::InnerLayout`. torin stops at a node marked `InnerLayout` without marking its parent, yet an auto-sized node changes size with its arrangement; and a child added in the same render stopped its own propagation at that already dirty node. Ancestors kept stale sizes and following siblings overlapped until something forced a full relayout. A plain mark still propagates only through parents whose size depends on their content; a reorder keeps `DirtyReason::Reorder`.

Regression: `crates/peersus-ui/src/layout_regression.rs` reproduces it with torin's own test tree through `mark_layout` (a card stayed 30 px instead of 242 px before the change).

Removal: drop when upstream torin propagates `InnerLayout` to ancestors that depend on their content, keeping that test passing.
