#!/usr/bin/env python3
"""Stage `Peersus.app` from built binaries: the bundle a person opens on macOS, with the name, icon and identity of the application.

Local only. The bundle is signed ad hoc (no identity, no certificate), so it is not a release: it is not notarized and
another Mac's Gatekeeper refuses it. It runs with macOS's own tools (`sips`, `iconutil`, `codesign`) and refuses without them.

Usage: stage-macos.py [--profile release|debug] [--environment KEY=VALUE ...]

`--environment` puts a variable in the bundle's `LSEnvironment`, for a machine that needs one to run at all: a virtual
machine has no Metal device and needs `FREYA_RENDERER=software`. A bundle for a real Mac takes none.
"""
import argparse
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

from build_layout import cargo_artifact_directory

NAME = "Peersus"
IDENTIFIER = "io.github.DarkPhilosophy.Peersus"
WINDOW = "peersus-ui"
# The window starts the owner that lies next to it, and the owner its helpers: they share one directory.
PROGRAMS = (WINDOW, "peersus", "peersus-wasm-host", "peersus-store-fetch", "peersus-native-host")
ICON = "crates/peersus-ui/assets/peersus-icon.png"
# The sizes `iconutil` names, up to the source's own 512 pixels: nothing is enlarged.
ICON_SIZES = ((16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1))
TOOLS = ("sips", "iconutil", "codesign")


def fail(message):
    sys.exit(f"stage-macos: {message}")


def version(root):
    package = re.search(r"^\[workspace\.package\]\n(.*?)(?:^\[|\Z)", (root / "Cargo.toml").read_text(), re.S | re.M)
    found = package and re.search(r'^version\s*=\s*"([^"]+)"', package.group(1), re.M)
    if not found:
        fail("Cargo.toml names no workspace version")
    return found.group(1)


def information(version, build, environment):
    """What `Info.plist` says. No minimum system version is claimed: none has been measured."""
    information = {
        "CFBundleName": NAME,
        "CFBundleDisplayName": NAME,
        "CFBundleIdentifier": IDENTIFIER,
        "CFBundleExecutable": WINDOW,
        "CFBundleIconFile": NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": build,
        "LSApplicationCategoryType": "public.app-category.utilities",
        "NSHighResolutionCapable": True,
        "NSPrincipalClass": "NSApplication",
    }
    if environment:
        information["LSEnvironment"] = environment
    return information


def run(*command):
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        fail(f"`{command[0]}` failed: {(result.stderr or result.stdout).strip()}")


def icon(source, target):
    with tempfile.TemporaryDirectory() as temporary:
        iconset = Path(temporary) / f"{NAME}.iconset"
        iconset.mkdir()
        for points, scale in ICON_SIZES:
            pixels = str(points * scale)
            name = f"icon_{points}x{points}{'@2x' if scale == 2 else ''}.png"
            run("sips", "-z", pixels, pixels, str(source), "--out", str(iconset / name))
        run("iconutil", "--convert", "icns", "--output", str(target), str(iconset))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--profile", choices=("release", "debug"), default="release")
    parser.add_argument("--environment", action="append", default=[], metavar="KEY=VALUE")
    arguments = parser.parse_args()
    environment = {}
    for pair in arguments.environment:
        key, separator, value = pair.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key) or key in environment:
            fail(f"`{pair}` is not one KEY=VALUE")
        environment[key] = value
    for tool in TOOLS:
        if shutil.which(tool) is None:
            fail(f"`{tool}` is needed and was not found; a bundle is staged on macOS")

    root = Path(__file__).resolve().parents[1]
    build = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    if not build.is_absolute():
        build = root / build
    programs = cargo_artifact_directory(arguments.profile)
    if not programs.is_absolute():
        programs = root / programs
    missing = [name for name in PROGRAMS if not (programs / name).is_file()]
    if missing:
        fail(f"{', '.join(missing)} not built in {programs}")

    bundle = build / "distribution/macos" / f"{NAME}.app"
    contents = bundle / "Contents"
    (contents / "MacOS").mkdir(parents=True, exist_ok=False)
    (contents / "Resources").mkdir()
    for name in PROGRAMS:
        shutil.copy2(programs / name, contents / "MacOS" / name)
    for name in ("LICENSE", "NOTICE"):
        shutil.copy2(root / name, contents / "Resources" / name)
    icon(root / ICON, contents / "Resources" / f"{NAME}.icns")
    with open(contents / "Info.plist", "wb") as file:
        plistlib.dump(information(version(root), os.environ.get("PEERSUS_BUILD_NUMBER", "1"), environment), file)
    # Ad hoc: seals the bundle so that macOS treats it as one application. It names no one and proves nothing to another Mac.
    run("codesign", "--force", "--deep", "--sign", "-", str(bundle))
    print(bundle)


if __name__ == "__main__":
    main()
