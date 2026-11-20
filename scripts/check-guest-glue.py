#!/usr/bin/env python3
"""Refuse an extension whose copies of the guest glue differ from the one copy in this source.

    check-guest-glue.py <extension source directory>...

A guest reaches the application through two files it carries a copy of: `wasm/src/host.rs` (the `peersus_v1`
imports behind two traits, with the scripted host its tests use) and `wasm/src/native.rs` (the same functions
through the native host's table). The one copy of each is `templates/guest/` in this source: a guest that carries
a file of that name carries exactly that file, so a rule of the boundary is changed in one place and every guest
has it. A guest that carries neither (the smallest ones speak to `peersus_v1` in their own few lines) is not this
check's concern. Nothing is built, imported or run.
"""
import sys
from pathlib import Path

GLUE = Path(__file__).resolve().parent.parent / 'templates' / 'guest'
NAMES = ('host.rs', 'native.rs')


def differing(source: Path) -> list[str]:
    """The glue files of `source` that are not the one copy, as sentences."""
    found = []
    for name in NAMES:
        carried = source / 'wasm' / 'src' / name
        if carried.is_symlink():
            found.append(f'{carried} is a link, not the file')
        elif carried.is_file() and carried.read_bytes() != (GLUE / name).read_bytes():
            found.append(f'{carried} differs from templates/guest/{name}')
    return found


def main() -> int:
    sources = [Path(argument) for argument in sys.argv[1:]]
    if not sources:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    try:
        found = [sentence for source in sources for sentence in differing(source)]
    except OSError as error:
        print(f'The guest glue could not be compared: {error}', file=sys.stderr)
        return 1
    for sentence in found:
        print(sentence, file=sys.stderr)
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
