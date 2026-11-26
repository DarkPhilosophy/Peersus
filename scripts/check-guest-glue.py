#!/usr/bin/env python3
"""Refuse an extension whose copies of the guest glue differ from the one copy in this source.

    check-guest-glue.py <extension source directory>...

A guest may carry `wasm/src/host.rs` (control and data), `native.rs` (the native function table),
`canvas_sdk.rs` (the Canvas display-list writer) or `media_sdk.rs` (the media decoder bindings).
Each carried copy must be exactly its template in `templates/guest/`; the files are optional.
Nothing is built, imported or run.
"""
import sys
from pathlib import Path

GLUE = Path(__file__).resolve().parent.parent / 'templates' / 'guest'
NAMES = {
    'host.rs': 'host.rs',
    'native.rs': 'native.rs',
    'canvas_sdk.rs': 'canvas.rs',
    'media_sdk.rs': 'media.rs',
}


def differing(source: Path) -> list[str]:
    """The glue files of `source` that are not the one copy, as sentences."""
    found = []
    for name, template in NAMES.items():
        carried = source / 'wasm' / 'src' / name
        if carried.is_symlink():
            found.append(f'{carried} is a link, not the file')
        elif carried.is_file() and carried.read_bytes() != (GLUE / template).read_bytes():
            found.append(f'{carried} differs from templates/guest/{template}')
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
