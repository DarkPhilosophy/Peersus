#!/usr/bin/env python3
"""Compile and run the nameless-font regression against an explicit built Skia."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Pinned Skia source directory")
    parser.add_argument("--library-dir", type=Path, required=True, help="Matching source-built Skia libraries")
    parser.add_argument("--symbol-font", type=Path, help="Font covering U+25BE, U+25B4 and U+2192; defaults to system DejaVu Sans")
    args = parser.parse_args()
    libraries = [args.library_dir.resolve() / name for name in
                 ("libskia.a", "libfreetype2.a", "libpng.a", "libzlib.a")]
    if not all(path.is_file() for path in libraries):
        parser.error("matching source-built renderer libraries are required")
    source = Path(__file__).with_suffix(".cpp").resolve()
    symbol_font = args.symbol_font or Path(subprocess.check_output(
        ["fc-match", "--format=%{file}", "DejaVu Sans:charset=25be"], text=True))
    latin_font = source.parent.parent / "crates/peersus-ui/assets/fonts/Quicksand[wght].ttf"
    if not symbol_font.is_file() or not latin_font.is_file():
        parser.error("real symbol and Latin font fixtures are required")
    flags = shlex.split(subprocess.check_output(
        ["pkg-config", "--cflags", "--libs", "fontconfig"], text=True))
    with tempfile.TemporaryDirectory(prefix="skia-fontconfig-regression-") as directory:
        binary = Path(directory) / "regression"
        subprocess.run([
            *shlex.split(os.environ.get("CXX", "clang++")), "-std=c++17",
            "-DSK_GANESH", "-DSK_GL", "-I" + str(args.source.resolve()), str(source),
            "-Wl,--start-group", *map(str, libraries), "-Wl,--end-group", *flags,
            "-lGL", "-lEGL", "-ldl", "-lpthread", "-o", str(binary),
        ], check=True, timeout=120)
        subprocess.run([str(binary), str(symbol_font.resolve()), str(latin_font)], check=True, timeout=5)
    print("PASS: nameless fonts neither block startup nor hide valid symbol fallback")


if __name__ == "__main__":
    main()
