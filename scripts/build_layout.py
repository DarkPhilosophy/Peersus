#!/usr/bin/env python3
"""Canonical helper layout: Cargo's own <target-triple>/<profile> under a surface root."""
import argparse
from functools import lru_cache
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
# The NDK the Android build is made with: the one the artifact workflow installs (`.github/workflows/artifacts.yml`).
ANDROID_NDK = "ndk/27.2.12479018"
ANDROID_NAMES = ("ANDROID_HOME", "ANDROID_NDK_HOME", "ANDROID_NDK", "ANDROID_PLATFORM", "ANDROID_BUILD_TOOLS_VERSION")

@lru_cache(maxsize=1)
def host_triple():
    result = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True, timeout=15)
    match = re.search(r"^host: ([a-zA-Z0-9_-]+)$", result.stdout, re.MULTILINE)
    if not match:
        raise ValueError("rustc did not report a host target triple")
    return match.group(1)


def android_sdk(result, root=ROOT):
    """The Android SDK the environment names, else the one Gradle's `android/local.properties` names; None for neither."""
    sdk = result.get("ANDROID_HOME") or result.get("ANDROID_SDK_ROOT")
    properties = root / "android/local.properties"
    if not sdk and properties.is_file():
        for line in properties.read_text().splitlines():
            if line.startswith("sdk.dir="):
                sdk = line.split("=", 1)[1].strip()
    return sdk or None


def environment(surface, original=None, root=ROOT):
    result = dict(os.environ if original is None else original)
    result.setdefault("CARGO_TARGET_DIR", "target/ui" if surface == "desktop" else "target/android")
    if surface == "desktop" and "CARGO_BUILD_TARGET" not in result:
        # An explicit build target makes Cargo provide the triple directory itself.
        result["CARGO_BUILD_TARGET"] = host_triple()
    sdk = android_sdk(result, root) if surface == "android" else None
    if sdk:
        # What the NDK's Cargo build scripts and Gradle read; a value already given is kept.
        result.setdefault("ANDROID_HOME", sdk)
        result.setdefault("ANDROID_NDK_HOME", str(Path(sdk) / ANDROID_NDK))
        result.setdefault("ANDROID_NDK", result["ANDROID_NDK_HOME"])
        result.setdefault("ANDROID_PLATFORM", "android-36")
        result.setdefault("ANDROID_BUILD_TOOLS_VERSION", "36.0.0")
    return result


def artifact_directory(surface, profile, original=None):
    return cargo_artifact_directory(profile, environment(surface, original))


def cargo_artifact_directory(profile, original=None):
    """Resolve Cargo's actual environment; bare Cargo retains its standard target/ default."""
    result = os.environ if original is None else original
    directory = Path(result.get("CARGO_TARGET_DIR", "target"))
    if result.get("CARGO_BUILD_TARGET"):
        directory /= result["CARGO_BUILD_TARGET"]
    return directory / profile


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("surface", choices=("desktop", "android"))
    parser.add_argument("--artifacts", choices=("debug", "release"))
    args = parser.parse_args()
    if args.artifacts:
        print(artifact_directory(args.surface, args.artifacts))
    else:
        configured = environment(args.surface)
        if args.surface == "android" and "ANDROID_HOME" not in configured:
            sys.exit("build_layout: set ANDROID_HOME to the installed Android SDK; prerequisites are never installed automatically")
        for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", *ANDROID_NAMES):
            if name in configured:
                print(f"export {name}={shlex.quote(configured[name])}")
