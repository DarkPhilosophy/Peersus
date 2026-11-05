#!/usr/bin/env python3
"""Build an unsigned AppImage from the staged Linux tree (target/distribution/linux).

What this does and does not claim:
  * It lays out an AppDir (AppRun, the desktop entry, the icon, usr/bin, usr/share) from what `scripts/stage-linux.py` staged, checks the desktop
    entry, and builds a squashfs of it.
  * It joins that to the AppImage type 2 runtime. The runtime is NOT in this repository and is not fetched by this script: pass its path and the
    SHA-256 that was published for it, and the script refuses to go on if they differ. Nothing unverified is ever put in the output.
  * The graphical libraries (EGL, GL, Vulkan, Wayland, X11, fontconfig) are the host's, as for every AppImage: they must match the machine's own
    graphics stack. This script does not bundle them, and says so in the file it writes next to the image.
  * It does NOT sign, publish or upload anything. Whether the result runs on another distribution is not shown by building it: run it there.

Usage: build-appimage.py --runtime PATH --runtime-sha256 HEX [--stage DIR] [--output DIR]
"""
from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

APP_ID = "io.github.DarkPhilosophy.Peersus"
ROOT = Path(__file__).resolve().parent.parent

APP_RUN = """#!/bin/sh
# The AppImage entry point: run the interface from the image, with the image's own data directories found first.
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:$PATH"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "$HERE/usr/bin/peersus-ui" "$@"
"""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def require(tool: str) -> str:
    found = shutil.which(tool)
    if not found:
        sys.exit(f"build-appimage: `{tool}` is needed and was not found")
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runtime", required=True, type=Path, help="the AppImage type 2 runtime for this architecture")
    parser.add_argument("--runtime-sha256", required=True, help="the SHA-256 published for that runtime")
    parser.add_argument("--stage", type=Path, default=ROOT / "target/distribution/linux")
    parser.add_argument("--output", type=Path, default=ROOT / "target/distribution/appimage")
    args = parser.parse_args()

    stage = args.stage.resolve()
    for needed in ("bin/peersus-ui", "bin/peersus", f"share/applications/{APP_ID}.desktop"):
        if not (stage / needed).is_file():
            sys.exit(f"build-appimage: {stage / needed} is missing; run scripts/stage-linux.py first")

    # The runtime is checked BEFORE anything is built from it.
    if not args.runtime.is_file():
        sys.exit(f"build-appimage: no runtime at {args.runtime}")
    actual = sha256(args.runtime)
    if actual != args.runtime_sha256.strip().lower():
        sys.exit(f"build-appimage: the runtime is not the one named: its SHA-256 is {actual}, not {args.runtime_sha256}")

    mksquashfs = require("mksquashfs")
    validate = require("desktop-file-validate")

    out = args.output.resolve()
    appdir = out / f"{APP_ID}.AppDir"
    if appdir.exists():
        shutil.rmtree(appdir)
    (appdir / "usr").mkdir(parents=True)
    shutil.copytree(stage / "bin", appdir / "usr/bin")
    shutil.copytree(stage / "share", appdir / "usr/share")

    desktop = appdir / "usr/share/applications" / f"{APP_ID}.desktop"
    subprocess.run([validate, str(desktop)], check=True)
    # AppImage tools look for the entry and the icon at the top of the directory.
    shutil.copy2(desktop, appdir / f"{APP_ID}.desktop")
    icon = appdir / f"usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg"
    if not icon.is_file():
        sys.exit(f"build-appimage: the icon {icon} is missing")
    shutil.copy2(icon, appdir / f"{APP_ID}.svg")
    run = appdir / "AppRun"
    run.write_text(APP_RUN)
    run.chmod(run.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    squashfs = out / "image.squashfs"
    if squashfs.exists():
        squashfs.unlink()
    subprocess.run([mksquashfs, str(appdir), str(squashfs), "-root-owned", "-noappend", "-comp", "zstd", "-quiet"], check=True)

    image = out / "Peersus-x86_64.AppImage"
    with image.open("wb") as target:
        target.write(args.runtime.read_bytes())
        target.write(squashfs.read_bytes())
    image.chmod(0o755)
    squashfs.unlink()

    (out / "Peersus-x86_64.AppImage.sha256").write_text(f"{sha256(image)}  {image.name}\n")
    (out / "README-appimage.txt").write_text(
        "Unsigned. Built from the staged Linux tree and the runtime whose SHA-256 was given.\n"
        "The graphical libraries (EGL/GL/Vulkan/Wayland/X11/fontconfig) are the host's, not bundled.\n"
        "That it runs on another distribution is not shown by building it: run it there.\n"
    )
    print(f"built {image} ({image.stat().st_size} bytes), sha256 written next to it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
