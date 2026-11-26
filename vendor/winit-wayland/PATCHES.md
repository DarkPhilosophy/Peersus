# Keep tablet tools usable as mouse pointers

Baseline: published `winit-wayland` 0.31.0-beta.4, [revision `d597ced770f3`](https://github.com/rust-windowing/winit/commit/d597ced770f3263f66ab901cc27bf7984e314465), with its upstream license.

Winit now delivers tablet input natively ([`f046e778aa0d`](https://github.com/rust-windowing/winit/commit/f046e778aa0d)). The old `vendor/winit` compatibility implementation is gone. `src/types/wp_tablet_input_v2.rs` retains per-motion positions, a default cursor shape on proximity entry, and the established right/middle/back aliases for the three side buttons (tip contact stays left); upstream treats the first side button as contact. The tool's active grab serial and seat let `src/window/state.rs` start a window move, resize or menu without a mouse serial. The independent touch path is upstream ([`cdca2632e9da`](https://github.com/rust-windowing/winit/commit/cdca2632e9da)).

Removal: drop when Winit preserves the button roles and cursor, and uses the tablet seat/serial for window operations. Check an actual or virtual tablet under Wayland for click, drag, resize, and cursor visibility.
