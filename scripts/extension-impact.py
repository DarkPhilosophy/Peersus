#!/usr/bin/env python3
"""Prepare package sizes and, on request, the two reviewed Linux WASM inputs for `cargo xtask bench`."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile

# The limits of one package (`scripts/validate-extension.py` enforces them on the submission; here they only bound the
# reading of an archive that came from a release).
MAX_FILES = 4096
MAX_TOTAL = 256 * 1024 * 1024


def archive_file(directory, name):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.tar\.gz', name):
        raise ValueError('Invalid archive basename')
    path = directory / name
    if path.is_symlink() or not path.is_file():
        raise ValueError('A listed package is missing')
    return path


def measured(directory):
    """The sizes of every package `review.json` in `directory` lists, by what each is for."""
    review = json.loads((directory / 'review.json').read_text())
    packages = {}
    for package in review['packages']:
        archive = archive_file(directory, package['archive'])
        sizes = []
        with tarfile.open(archive, 'r:*') as members:
            for member in members:
                if member.isfile():
                    sizes.append(member.size)
                if len(sizes) > MAX_FILES or sum(sizes) > MAX_TOTAL:
                    raise ValueError('A package is larger than a package may be')
        target = '-'.join([package['execution']['kind'], package['architecture'], '+'.join(sorted(package['platforms']))])
        if not re.fullmatch(r'[a-z0-9_+-]{1,128}', target) or target in packages:
            raise ValueError('Packages must differ in what they are for')
        packages[target] = {
            'file_bytes': archive.stat().st_size,
            'unpacked_bytes': sum(sizes),
            'largest_file_bytes': max(sizes, default=0),
            'files': len(sizes),
        }
    return {'schema': 1, 'id': review['id'], 'version': review['packages'][0]['version'], 'package': packages}


def stage_wasm(directory, output):
    """Rebind the selected archive to the review, then copy two files to caller-chosen paths."""
    review = json.loads((directory / 'review.json').read_text())
    plugin = review['id']
    if not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', plugin):
        raise ValueError('Invalid stable plugin ID')
    wasm = {'kind': 'wasm', 'abi': 1}
    eligible = [package for package in review['packages'] if package['execution'] == wasm
                and package['architecture'] == 'any' and 'linux' in package['platforms']]
    if len(eligible) != 1:
        raise ValueError('Expected exactly one reviewed Linux WASM package')
    package = eligible[0]
    source = archive_file(directory, package['archive'])
    with source.open('rb') as archive:
        if hashlib.file_digest(archive, 'sha256').hexdigest() != package['archive_sha256']:
            raise ValueError('Linux WASM archive differs from the reviewed digest')
    spec = importlib.util.spec_from_file_location('extension_validator', Path(__file__).with_name('validate-extension.py'))
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)
    try:
        members = validator.read_members(source)
    except SystemExit as error:
        raise ValueError('Unsafe or oversized Linux WASM archive') from error
    manifest = members[f'{plugin}/plugin.json']
    parsed = json.loads(manifest)
    program = parsed.get('program', 'plugin')
    if parsed.get('execution') != wasm or '/' in program or '\\' in program or program in ('', '.', '..'):
        raise ValueError('Expected one declared WASM program with a plain filename')
    module = members[f'{plugin}/{program}']
    if not module.startswith(b'\0asm\x01\0\0\0'):
        raise ValueError('The reviewed program is not a WebAssembly module')
    output.mkdir(parents=True)
    (output / 'plugin.json').write_bytes(manifest)
    (output / 'guest.wasm').write_bytes(module)


def shown(size):
    return f'{size / 1048576:.2f} MiB' if size >= 1048576 else f'{size / 1024:.1f} KiB'


def alone(change):
    """The report of an extension the store does not have yet: its sizes, with nothing to compare them with."""
    lines = ['## Resource impact: INCONCLUSIVE — no published predecessor', '',
             f'Version {change["version"]} has no published predecessor: these are absolute sizes, not a comparison.', '',
             '| Metric / method | Baseline | After | Delta |', '|---|---:|---:|---:|']
    for target, sizes in sorted(change['package'].items()):
        for label, key in (('Package size', 'file_bytes'), ('Package unpacked', 'unpacked_bytes'),
                           ('Largest file of the package', 'largest_file_bytes')):
            lines.append(f'| {label} · `{target}` | none | {shown(sizes[key])} | n/a |')
    return '\n'.join(lines) + '\n'


def published(repository, plugin, into):
    """Download the packages of the newest version of `plugin` the store published into `into`; its version, or None."""
    admission = Path(__file__).with_name('verify-extension-admission.py')
    spec = importlib.util.spec_from_file_location('admission', admission)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    def fetch(path):
        return json.loads(subprocess.run(['gh', 'api', f'repos/{repository}/{path}'],
                                         check=True, capture_output=True, text=True, timeout=60).stdout)

    version = module.previous_version(fetch, plugin)
    if version is None:
        return None
    if not re.fullmatch(r'[0-9A-Za-z.+-]{1,64}', version):
        raise ValueError('The published version is not a version')
    # Captured: what this script prints is the version alone.
    subprocess.run(['gh', 'release', 'download', f'extension/{plugin}/v{version}', '--repo', repository, '--dir', str(into),
                    '--pattern', '*.tar.gz', '--pattern', 'review.json'], check=True, capture_output=True, timeout=300)
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate', type=Path, required=True, help='The reviewed packages of the submission, with review.json')
    parser.add_argument('--repository', help='owner/name: fetch the version the store has from its releases')
    parser.add_argument('--baseline', type=Path, help='Packages of the version the store has, already downloaded')
    parser.add_argument('--out', type=Path, required=True, help='Where extension-change.json, and extension-baseline.json or impact.md, are written')
    parser.add_argument('--runtime-inputs', type=Path, help='Stage reviewed Linux WASM inputs in change/ and, when published, baseline/')
    args = parser.parse_args()
    try:
        change = measured(args.candidate)
        if not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', change['id']):
            raise ValueError('Invalid stable plugin ID')
        baseline = args.baseline
        published_version = None
        if baseline is None and args.repository:
            if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository):
                raise ValueError('Invalid repository')
            baseline = args.out / 'published'
            baseline.mkdir(parents=True)
            published_version = published(args.repository, change['id'], baseline)
            if published_version is None:
                baseline = None
        args.out.mkdir(parents=True, exist_ok=True)
        if args.runtime_inputs is not None:
            stage_wasm(args.candidate, args.runtime_inputs / 'change')
        (args.out / 'extension-change.json').write_text(json.dumps(change, sort_keys=True, indent=2) + '\n')
        previous = None
        if baseline is not None:
            before = measured(baseline)
            if before['id'] != change['id']:
                raise ValueError('The baseline is another extension')
            previous = before['version']
            if published_version is not None and published_version != previous:
                raise ValueError('Published version and baseline review disagree')
            if args.runtime_inputs is not None:
                stage_wasm(baseline, args.runtime_inputs / 'baseline')
            (args.out / 'extension-baseline.json').write_text(json.dumps(before, sort_keys=True, indent=2) + '\n')
        elif args.runtime_inputs is None:
            (args.out / 'impact.md').write_text(alone(change))
        if args.runtime_inputs is not None:
            raw = (args.candidate / 'review.json').read_bytes()
            receipt = {'schema': 1, 'id': change['id'], 'version': change['version'],
                       'commit': json.loads(raw)['commit'], 'previous': previous,
                       'review_sha256': hashlib.sha256(raw).hexdigest()}
            (args.out / 'extension-input.json').write_text(json.dumps(receipt, sort_keys=True) + '\n')
        print(previous if previous is not None else 'new')
        return 0
    except (OSError, ValueError, KeyError, TypeError, IndexError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f'Extension benchmark inputs could not be prepared: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
