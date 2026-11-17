#!/usr/bin/env python3
"""Which dependencies have a newer release on crates.io, alpha and beta releases included.

Dependabot proposes stable releases, and a pre-release only for a crate already on one; it cannot see
a vendored copy at all. This report covers both:

- every non-path dependency of the root workspace, against the newest release and the newest stable one;
- every `[patch.crates-io]` copy under `vendor/`, against the newest release of the crate it copies:
  those are moved by hand, carrying the changes their `PATCHES.md` lists.

Read only: it writes Markdown to standard output (or `--out`) and changes nothing in the tree. Exit 0
with or without findings; a crate the registry did not answer for is listed as unknown, not skipped.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
import urllib.request
from pathlib import Path
from typing import Callable

ROOT = Path(__file__).resolve().parent.parent
REGISTRY = "https://crates.io/api/v1/crates/{}"
AGENT = "peersus dependency report (https://github.com/DarkPhilosophy/Peersus)"

Fetch = Callable[[str], dict | None]


def fetch(name: str) -> dict | None:
    """The registry's record of one crate, or None when it cannot be had."""
    request = urllib.request.Request(REGISTRY.format(name), headers={"User-Agent": AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except (OSError, ValueError):
        return None


VERSION = re.compile(r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$")


def key(version: str) -> tuple | None:
    """A sort key that follows SemVer precedence: a pre-release is below its release."""
    match = VERSION.match(version.strip())
    if not match:
        return None
    major, minor, patch, pre = match.groups()
    numbers = (int(major), int(minor or 0), int(patch or 0))
    if pre is None:
        return numbers + ((1,),)
    parts = tuple((0, int(part), "") if part.isdigit() else (1, 0, part) for part in pre.split("."))
    return numbers + ((0,) + parts,)


def requirement(spec: object) -> tuple[str, str] | None:
    """(package, version) of a workspace dependency, or None for a path dependency."""
    if isinstance(spec, str):
        return None, spec
    if isinstance(spec, dict) and "path" not in spec and "version" in spec:
        return spec.get("package"), spec["version"]
    return None


def newest(record: dict) -> tuple[str | None, str | None]:
    """(newest release, newest stable release), yanked releases left out."""
    versions = [v["num"] for v in record.get("versions", []) if not v.get("yanked")]
    ordered = sorted((v for v in versions if key(v)), key=key)
    stable = [v for v in ordered if "-" not in v.split("+")[0]]
    return (ordered[-1] if ordered else None, stable[-1] if stable else None)


def behind(current: str, candidate: str | None) -> bool:
    if not candidate:
        return False
    have, offered = key(current.lstrip("=^~ ")), key(candidate)
    return have is not None and offered is not None and offered > have


def locked(root: Path) -> dict[str, str]:
    """The newest version of each registry package the lockfile resolved."""
    try:
        packages = tomllib.loads((root / "Cargo.lock").read_text()).get("package", [])
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    found: dict[str, str] = {}
    for package in packages:
        if not str(package.get("source", "")).startswith("registry+"):
            continue
        name, version = package.get("name"), str(package.get("version", ""))
        if key(version) and (name not in found or key(version) > key(found[name])):
            found[name] = version
    return found


def report(root: Path, get: Fetch = fetch) -> str:
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    workspace = manifest.get("workspace", {}).get("dependencies", {})
    patched = manifest.get("patch", {}).get("crates-io", {})
    rows, copies, unknown = [], [], []
    resolved = locked(root)

    for name, spec in sorted(workspace.items()):
        found = requirement(spec)
        if found is None:
            continue
        package, current = found
        package = package or name
        record = get(package)
        if record is None:
            unknown.append(package)
            continue
        latest, stable = newest(record)
        # What is built is what the lockfile resolved; a short requirement ("1") is not behind by itself.
        have = resolved.get(package, current)
        if behind(have, latest):
            kind = "pre-release" if latest != stable else "release"
            rows.append(
                f"| `{package}` | `{current}` | `{have.lstrip('=^~ ')}` | `{latest}` ({kind}) | `{stable or '—'}` |"
            )

    for name, spec in sorted(patched.items()):
        if not isinstance(spec, dict) or "path" not in spec:
            continue
        copy = root / spec["path"] / "Cargo.toml"
        try:
            package = tomllib.loads(copy.read_text())["package"]
        except (OSError, KeyError, tomllib.TOMLDecodeError):
            unknown.append(f"{name} (vendored copy unreadable)")
            continue
        record = get(package.get("name", name))
        if record is None:
            unknown.append(name)
            continue
        latest, stable = newest(record)
        if behind(str(package.get("version", "0")), latest):
            copies.append(
                f"| `{name}` | `{package.get('version')}` | `{latest}` | `{spec['path']}/PATCHES.md` |"
            )

    out = ["# Dependency updates available", ""]
    if rows:
        out += [
            "Newer than what the workspace asks for. Stable ones also reach Dependabot; pre-releases only reach it for a crate already on one.",
            "",
            "| Crate | Asked | Locked | Newest | Newest stable |",
            "|---|---|---|---|---|",
            *rows,
            "",
        ]
    if copies:
        out += [
            "Vendored copies whose upstream moved. Dependabot cannot update these: each is re-made from the new release with the changes its `PATCHES.md` lists.",
            "",
            "| Copy | Vendored | Upstream newest | Carried changes |",
            "|---|---|---|---|",
            *copies,
            "",
        ]
    if unknown:
        out += ["The registry did not answer for: " + ", ".join(f"`{u}`" for u in unknown) + ".", ""]
    if not (rows or copies or unknown):
        out += ["Every dependency and vendored copy is at the newest release.", ""]
    return "\n".join(out)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args(argv)
    text = report(args.root)
    if args.out:
        args.out.write_text(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
