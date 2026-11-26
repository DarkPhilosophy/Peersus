#!/usr/bin/env python3
"""Check the project's own extension manifests and recipes without running any recipe command."""
import argparse
from itertools import zip_longest
import json
from pathlib import Path
import sys
import tomllib


FIRST_PARTY = ('clipboard', 'control', 'location', 'monitor', 'test', 'upload')
PLATFORMS = ('linux', 'android', 'macos', 'windows')
GUEST = '{source}/wasm'
TARGET = '{sdk}/target'
RELEASE = TARGET + '/release'
CARGO_MANIFEST = ['--manifest-path', GUEST + '/Cargo.toml', '--locked']
CARGO_TARGET = ['--target-dir', TARGET]
EXCEPTIONS = {
    'location': {
        'omit_native': True,
        'reason': 'Location omits both Linux native forms: its map requires WASM canvas_present for PSD1 display '
                  'lists. The native Canvas stream accepts rendered PSF1 frames, not PSD1; Location has no native rasterizer.',
        # Accept this exact check until the recipe drops it; the shared source gate owns it now.
        'legacy_build_prefix': ['cmp', '{sdk}/templates/guest/canvas.rs', GUEST + '/src/canvas_sdk.rs'],
    },
    'clipboard': {
        'reason': 'Clipboard also checks translation completeness because it ships locale files.',
        'extra_test': ['cargo', 'run', '--manifest-path', '{sdk}/Cargo.toml', '--locked', '-p', 'xtask', *CARGO_TARGET,
                       '--', 'check-manifest', '--complete-translations', GUEST],
    },
}


def expected_recipe(plugin, policy):
    library = f'libpeersus_{plugin}_wasm'
    process = f'peersus-{plugin}-process'
    build = [['cargo', 'build', *CARGO_MANIFEST, '--release', '--target', 'wasm32-unknown-unknown', *CARGO_TARGET]]
    test = [['cargo', 'test', *CARGO_MANIFEST, *CARGO_TARGET]]

    def pack(platform, target, *form):
        return ['python3', '{sdk}/scripts/package-native-extension.py', '--source', GUEST, '--sdk', '{sdk}',
                '--target-dir', target, '--out', '{out}', '--platform', platform, '--id', plugin, *form]

    package = [pack(platform, TARGET) for platform in PLATFORMS]
    artifacts = [f'{plugin}-{platform}.tar.gz' for platform in PLATFORMS]
    if not policy.get('omit_native'):
        build.extend([
            ['cargo', 'build', *CARGO_MANIFEST, '--release', '--features', 'native', *CARGO_TARGET],
            ['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror', '-fstack-protector-strong', '-fPIE', '-pie',
             '-DPEERSUS_LINKED_MODULE', '{sdk}/crates/peersus-plugin-runtime/native/host.c', f'{RELEASE}/{library}.a',
             '-Wl,-z,relro,-z,now', '-lpthread', '-ldl', '-lm', '-o', f'{RELEASE}/{process}'],
        ])
        package.extend([pack('linux', RELEASE, '--native-library', f'{library}.so'),
                        pack('linux', RELEASE, '--process', process)])
        artifacts.extend([f'{plugin}-linux-native_library.tar.gz', f'{plugin}-linux-process.tar.gz'])
    if 'extra_test' in policy:
        test.append(policy['extra_test'])
    return {'build': build, 'test': test, 'package': package}, set(artifacts)


def read_text(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f'{path.name} must be a regular file')
    return path.read_text(encoding='utf-8')


def check(source):
    plugin = source.name.removeprefix('peersus-')
    if plugin not in FIRST_PARTY or source.name != f'peersus-{plugin}':
        raise ValueError('expected one of the known first-party peersus-<id> source directories')
    if source.is_symlink() or not source.is_dir():
        raise ValueError('missing or linked first-party source directory')
    data = tomllib.loads(read_text(source / 'submission.toml'))
    if set(data) != {'schema', 'id', 'artifacts', 'build', 'test', 'package'}:
        raise ValueError('submission.toml must use the standalone guest recipe fields')
    if type(data['schema']) is not int or data['schema'] != 1 or data['id'] != plugin:
        raise ValueError(f'submission.toml requires schema 1 and id {plugin}')

    wasm = source / 'wasm'
    if wasm.is_symlink():
        raise ValueError('wasm/ must not be a symbolic link')
    manifest = json.loads(read_text(wasm / 'plugin.json'))
    if not isinstance(manifest, dict):
        raise ValueError('wasm/plugin.json must be an object')
    execution = manifest.get('execution')
    if execution != {'kind': 'wasm', 'abi': 1} or type(execution['abi']) is not int:
        raise ValueError('wasm/plugin.json must declare execution as WASM ABI 1')
    program = f'peersus_{plugin}_wasm.wasm'
    if manifest.get('program') != program:
        raise ValueError(f'wasm/plugin.json must name {program} as its program')
    platforms = manifest.get('platforms')
    if (not isinstance(platforms, list) or len(platforms) != len(PLATFORMS)
            or any(not isinstance(platform, str) for platform in platforms) or set(platforms) != set(PLATFORMS)):
        raise ValueError(f'wasm/plugin.json must declare exactly these platforms: {", ".join(PLATFORMS)}')

    declared = data['artifacts']
    if (not isinstance(declared, list) or any(not isinstance(name, str) for name in declared)
            or len(set(declared)) != len(declared)):
        raise ValueError('artifacts must list distinct archive names')
    artifacts = set(declared)
    for platform in PLATFORMS:
        name = f'{plugin}-{platform}.tar.gz'
        if name not in artifacts:
            raise ValueError(f'wasm/plugin.json lists {platform}, but artifacts omits {name}')

    policy = EXCEPTIONS.get(plugin, {})
    commands, expected = expected_recipe(plugin, policy)
    if artifacts != expected:
        details = [f'missing {name}' for name in sorted(expected - artifacts)]
        details += [f'unexpected {ascii(name)}' for name in sorted(artifacts - expected)]
        raise ValueError('artifacts: ' + ', '.join(details))
    for phase, wanted in commands.items():
        actual = data[phase]
        if not isinstance(actual, list):
            raise ValueError(f'{phase} must be a list of argument vectors')
        if phase == 'build' and actual and actual[0] == policy.get('legacy_build_prefix'):
            # Keep diagnostic indexes relative to the actual TOML array.
            wanted = [policy['legacy_build_prefix'], *wanted]
        for index, (line, expected_line) in enumerate(zip_longest(actual, wanted), 1):
            if line != expected_line:
                if expected_line is None:
                    raise ValueError(f'{phase}[{index}] is an unexpected extra command')
                raise ValueError(f'{phase}[{index}] must be {json.dumps(expected_line)}')
    return len(artifacts)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    scope = parser.add_mutually_exclusive_group()
    scope.add_argument('--root', type=Path, help='Directory containing all six peersus-<id> sources (default: plugins/)')
    scope.add_argument('--source', type=Path, help='Check just one explicitly selected first-party source directory')
    args = parser.parse_args()
    root = args.root if args.root is not None else Path(__file__).resolve().parents[1] / 'plugins'
    sources = [args.source] if args.source is not None else [root / f'peersus-{plugin}' for plugin in FIRST_PARTY]
    count = 0
    failed = False
    for source in sources:
        try:
            count += check(source)
            if policy := EXCEPTIONS.get(source.name.removeprefix('peersus-')):
                print(policy['reason'])
        except (OSError, ValueError) as error:
            print(f'{source.name}: {error}', file=sys.stderr)
            failed = True
    if failed:
        return 1
    print(f'Checked {len(sources)} first-party recipes declaring {count} archives; no commands executed.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
