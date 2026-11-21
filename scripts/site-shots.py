#!/usr/bin/env python3
"""Draw the pictures of the application the website shows (site/shots/*.webp) with the application itself.

Runs the headless demo's picture test (`site_shots` in crates/peersus-ui/src/headless/tests.rs), which opens the real window on
the demo's made-up data, drawn dark at twice a phone's density, and writes PNG files; each is then encoded as WebP with `cwebp`.
Nothing of a person's is read: the demo has no owner and no files. Run from the repository root, with the desktop build's
environment (FORCE_SKIA_BUILD=1, CARGO_TARGET_DIR on disk; see AGENTS.md).
"""
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "shots"
QUALITY = "82"


def main():
    if shutil.which("cwebp") is None:
        sys.exit("cwebp is needed to encode the pictures (libwebp)")
    with tempfile.TemporaryDirectory() as folder:
        environment = dict(os.environ, PEERSUS_SITE_SHOTS=folder)
        subprocess.run(
            ["cargo", "test", "-p", "peersus-ui", "--all-features", "--lib", "--", "--ignored", "--exact", "headless::tests::site_shots"],
            cwd=ROOT, env=environment, check=True,
        )
        pictures = sorted(Path(folder).glob("*.png"))
        if not pictures:
            sys.exit("The picture test wrote nothing")
        OUT.mkdir(exist_ok=True)
        for picture in pictures:
            target = OUT / f"{picture.stem}.webp"
            subprocess.run(["cwebp", "-quiet", "-q", QUALITY, "-m", "6", str(picture), "-o", str(target)], check=True)
            print(f"{target.relative_to(ROOT)}: {target.stat().st_size} bytes")


if __name__ == "__main__":
    main()
