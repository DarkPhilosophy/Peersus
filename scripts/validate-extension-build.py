#!/usr/bin/env python3
"""Validate built artifacts as DATA on a fresh runner; never execute submitted code."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

VALIDATOR = Path(__file__).with_name('validate-extension.py')


def validate(directory, plugin_id, commit, sdk, output):
    if not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', plugin_id):
        raise ValueError('Invalid stable plugin ID')
    if any(not re.fullmatch(r'[0-9a-f]{40}', value) for value in (commit, sdk)):
        raise ValueError('Source and SDK must be pinned commits')
    if output.exists():
        raise ValueError('Review output must be a new directory')
    paths = list(directory.iterdir())
    if any(path.is_symlink() or not path.is_file() for path in paths):
        raise ValueError('Build artifacts must be regular files, not links or directories')
    archives = sorted(path for path in paths if path.name.endswith('.tar.gz'))
    if not 1 <= len(archives) <= 32 or any(path.name != 'build.json' and path not in archives for path in paths):
        raise ValueError('Expected built archives and an optional build.json, nothing else')
    reports, targets, identity = [], set(), None
    for archive in archives:
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.tar\.gz', archive.name):
            raise ValueError('Invalid archive basename')
        result = subprocess.run([sys.executable, str(VALIDATOR), str(archive)],
                                check=True, capture_output=True, text=True, timeout=60)
        report = json.loads(result.stdout)
        if report['id'] != plugin_id:
            raise ValueError('Every platform package must keep the same stable plugin ID')
        current = tuple(report[key] for key in ('id', 'version', 'author', 'link', 'description'))
        if identity is not None and identity != current:
            raise ValueError('All platform builds must describe the same plugin version and author')
        identity = current
        architecture, platforms = report['architecture'], report['platforms']
        if not platforms or architecture not in ('any', 'aarch64', 'x86_64', 'arm', 'x86'):
            raise ValueError('Every artifact needs verifiable platform and architecture information')
        if report['execution']['kind'] in ('native_library', 'process') and (
            architecture == 'any' or any(platform not in ('linux', 'android') for platform in platforms)
        ):
            raise ValueError('This native validator currently supports ELF platform builds only')
        for platform in platforms:
            if any(os == platform and (arch == architecture or arch == 'any' or architecture == 'any')
                   for os, arch in targets):
                raise ValueError('Two artifacts overlap the same platform/architecture')
            targets.add((platform, architecture))
        reports.append({'archive': archive.name, **report})
    review = {'schema': 1, 'id': plugin_id, 'commit': commit, 'sdk': sdk, 'packages': reports}
    # Stage all outputs only after every archive passed the independent validator.
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='extension-review-', dir=output.parent) as temporary:
        staged = Path(temporary) / 'review'
        staged.mkdir()
        for archive in archives:
            shutil.copyfile(archive, staged / archive.name)
        raw = (json.dumps(review, sort_keys=True, indent=2) + '\n').encode()
        (staged / 'review.json').write_bytes(raw)
        staged.rename(output)
    return hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--id', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--sdk', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        digest = validate(args.directory, args.id, args.commit, args.sdk, args.out)
        print(f'Validated one plugin and all its platform artifacts. Review SHA-256: {digest}')
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'Built extension refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
