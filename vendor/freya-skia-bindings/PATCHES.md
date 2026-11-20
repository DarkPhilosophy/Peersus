# Nameless system fonts must not block startup

Baseline: published `freya-skia-bindings` 0.105.0-beta.1, https://github.com/marc2332/rust-skia, package revision `74941313f00897060d958bc4ff145239c2ceb439`, Skia pin `m156.5` (moved from 0.104.0 / `m153-0.101.2` on 2026-11-15; both texts the font fix replaces are present exactly once in m156.5's `src/ports/SkFontMgr_fontconfig.cpp`). Original MIT license retained; fetched Skia retains its own BSD license. Downloaded native sources and build products are not vendored.

The source build fixes two consequences of incomplete Fontconfig patterns. `GetFamilyNames` stops on `FcResultNoMatch` as well as `FcResultNoId`, because a missing property returns NoMatch at every index. Before any enumeration or matching, the factory removes patterns lacking both family and charset from its owned configuration's system/application font sets. Fontconfig can otherwise merge requested charset values into such a pattern and select a font file with no corresponding glyph. Nameless fonts with a real charset remain eligible for fallback. Valid aliases and the relative order of retained patterns are preserved. Both build hunks are checked before writing; unexpected source revisions fail explicitly.

Regression: run `python3 scripts/test-skia-fontconfig.py --source vendor/freya-skia-bindings/skia --library-dir <matching-build-output>/skia` in the Linux build environment. `--symbol-font` can explicitly select a font covering U+25BE, U+25B4 and U+2192; otherwise the runner asks Fontconfig for DejaVu Sans. The first fixture exercises startup with a nameless pattern and two valid family aliases. The second uses actual symbol/Latin fonts and an incomplete pattern pointing at the Latin font; fallback must return real glyphs rather than .notdef. The original renderer hung during startup; the termination-only patch completed startup but failed the glyph assertion.

Removal: drop this vendor override when the compatible pinned renderer handles missing family properties upstream; rerun the native regression and actual Linux window startup. No font cache, system library or user settings need changing.

# Android bindings name libc++'s headers

`build_support/platform/android.rs`, `additional_clang_args`: for NDK 22 and later, bindgen is also given `-isystem <NDK toolchain>/sysroot/usr/include/c++/v1`. Bindgen runs the host's libclang, which need not be the NDK's clang; Homebrew clang 23 adds no libc++ include for an Android target, so a full Android build stopped after compiling Skia with `'cassert' file not found` in `src/bindings.cpp` (2026-11-16). The same flag is what the pre-22 branch already passes for its own layout. Regression: a full `./dev.sh android` (Skia rebuilt) with that libclang first on the path.

Removal: drop it when upstream passes the libc++ include itself, after a full Android build with a non-NDK libclang.
