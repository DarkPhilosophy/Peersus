#!/usr/bin/env python3
"""Plan a one-plugin source PR from Git paths only; execute no submitted code."""
import argparse
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


def plan(paths):
    directories = set()
    for raw in paths:
        path = PurePosixPath(raw)
        if len(path.parts) < 3 or path.parts[0] != 'extension' or any(p in ('.', '..') for p in path.parts):
            raise ValueError('A source submission may change only extension/<plugin>/**')
        name = path.parts[1]
        if not re.fullmatch(r'[a-z][a-z0-9_-]{0,79}', name):
            raise ValueError('Invalid plugin source directory')
        directories.add(name)
    if len(directories) != 1:
        raise ValueError('Submit exactly one plugin per PR; all platform implementations belong in that directory')
    name = directories.pop()
    if not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', name.removeprefix('peersus-')):
        raise ValueError('Invalid stable plugin ID')
    return {'directory': f'extension/{name}', 'id': name.removeprefix('peersus-')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', required=True)
    parser.add_argument('--github-output', type=Path)
    args = parser.parse_args()
    try:
        for revision in (args.base, args.head):
            if not re.fullmatch(r'[0-9a-f]{40}', revision):
                raise ValueError('Use complete pinned commit SHAs')
        changed = subprocess.check_output(['git', 'diff', '--no-renames', '--name-only', '-z',
                                           args.base, args.head], cwd=args.repo)
        result = plan([p.decode('utf-8') for p in changed.split(b'\0') if p])
        print(json.dumps(result))
        if args.github_output:
            with args.github_output.open('a') as stream:
                for name, value in result.items():
                    stream.write(f'{name}={value}\n')
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'Source submission refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
