# Use the maintained Wayland title renderer by default

Baseline: published `winit` 0.31.0-beta.4, [revision `d597ced770f3`](https://github.com/rust-windowing/winit/commit/d597ced770f3263f66ab901cc27bf7984e314465). The upstream Apache-2.0 license is retained.

`Cargo.toml` and `Cargo.toml.orig` map `wayland-csd-adwaita` to `winit-wayland/csd-adwaita-skrifa`. This keeps window decorations and avoids the unmaintained `ttf-parser` dependency (RUSTSEC-2026-0192) even when Freya enables Winit defaults. The old `sctk-adwaita` source backport and its compiler-lifetime adjustment are not needed: Winit ships with the published `sctk-adwaita` 0.12 line ([dependency update](https://github.com/rust-windowing/winit/commit/a68f1a664b79), [font selection](https://github.com/rust-windowing/winit/commit/06d449d7613b)).

Removal: drop when the compatible Winit defaults use the maintained title renderer, or all consumers can turn off the old default without losing Wayland support. Check the strict audit and real Wayland title/decoration rendering.

## Moved and retired platform backports

- Window move, resize and menu use a finger's serial upstream ([`cdca2632e9da`](https://github.com/rust-windowing/winit/commit/cdca2632e9da)).
- Graphic tablets deliver pointer events upstream ([`f046e778aa0d`](https://github.com/rust-windowing/winit/commit/f046e778aa0d)); cursor, button aliases and window-operation serials still need the [`winit-wayland` patch](../winit-wayland/PATCHES.md).

The Android loop and destruction patches now live in [`winit-android`](../winit-android/PATCHES.md), where Winit 0.31 implements the Android backend.
