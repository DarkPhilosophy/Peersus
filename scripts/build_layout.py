#!/usr/bin/env python3
"""Canonical helper layout: Cargo's own <target-triple>/<profile> under a surface root."""
import argparse
from functools import lru_cache
import os
from pathlib import Path
import re
import shlex
import subprocess


@lru_cache(maxsize=1)
def host_triple():
    result = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True, timeout=15)
    match = re.search(r"^host: ([a-zA-Z0-9_-]+)$", result.stdout, re.MULTILINE)
    if not match:
        raise ValueError("rustc did not report a host target triple")
    return match.group(1)


def environment(surface, original=None):
    result = dict(os.environ if original is None else original)
    result.setdefault("CARGO_TARGET_DIR", "target/ui" if surface == "desktop" else "target/android")
    if surface == "desktop" and "CARGO_BUILD_TARGET" not in result:
        # An explicit build target makes Cargo provide the triple directory itself.
        result["CARGO_BUILD_TARGET"] = host_triple()
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
        for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET"):
            if name in configured:
                print(f"export {name}={shlex.quote(configured[name])}")
