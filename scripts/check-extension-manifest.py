#!/usr/bin/env python3
"""Run the application's manifest check (execution declaration, licence and notice, translations).

Single entry point for scripts and CI: `cargo xtask check-manifest` reads a package folder
(`plugin.json` or `plugin.toml` at its root or under `wasm/`) or a manifest file, and exits
non-zero with a one-sentence reason when the manifest does not explicitly declare its execution,
lacks a valid licence-and-notice agreement, or when a `languages/<tag>.po` beside the manifest
is not that language. Words of the
manifest a translation leaves in English are printed as notes (the contract allows them); an
extension that wants them refused runs `cargo xtask check-manifest --complete-translations` in
its own recipe. This wrapper only locates the checkout this script lives in and forwards its verdict.
This checks one manifest and does not require any sibling execution kind. WASM coverage is
checked by `validate-extension-build.py` on the built submission.
"""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def check(target: Path) -> int:
    if not target.exists():
        print(f'Manifest check refused: not a package folder or manifest file: {target}', file=sys.stderr)
        return 1
    command = ['cargo', 'run', '--locked', '-p', 'xtask', '--', 'check-manifest', str(target)]
    if os.environ.get('PEERSUS_MANIFEST_CHECK_VERBOSE'):
        command.insert(2, '--verbose')
    return subprocess.run(command, cwd=ROOT).returncode


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    return check(Path(sys.argv[1]).resolve())


if __name__ == '__main__':
    raise SystemExit(main())
