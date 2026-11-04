# Maintained title font renderer backport

Baseline: sctk-adwaita 0.10.1, upstream `https://github.com/PolyMeilex/sctk-adwaita`, revision `d5f343c45c04d4a026db57f76760819b7325db00`. Imported from the published crates.io source on 2026-11-04. The original MIT license and embedded Cantarell font remain unchanged.

Backport: use the `skrifa` title renderer and its regression tests from upstream sctk-adwaita 0.12.0, revision `1bddc2264a71ae3897903ec8a7d7b13b834ff1b1`. Keep smithay-client-toolkit 0.19 and tiny-skia 0.11, matching the existing winit Wayland API. The default renderer and winit's existing `wayland-csd-adwaita` feature now select skrifa instead of ab_glyph. The crossfont and no-title feature paths remain available.

Purpose: replace the unmaintained ttf-parser dependency (RUSTSEC-2026-0192) without removing Wayland window decorations or upgrading the entire windowing/Android stack. No advisory is suppressed.

Removal: remove this backport and the local winit feature adjustment when the application's compatible released windowing stack includes the maintained font renderer. Verify rendered glyphs, scale/color updates, the Wayland window title and decorations, desktop and Android builds, and the strict advisory audit. Unit tests are not a substitute for native Wayland runtime verification.
