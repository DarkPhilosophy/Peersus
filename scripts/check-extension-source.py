#!/usr/bin/env python3
"""Build and test ONE source submission; never approve, sign, install or publish it.

Run only on a disposable runner with no secrets or persisted checkout credentials.
Package validation MUST run in a separate fresh job, never after untrusted commands here.
Only the recipe's declared artifacts are collected. The separate validator checks that WASM
covers the required platforms and validates any optional native artifacts that were built.
The submitted commands are arbitrary, untrusted code, not a sandboxed DSL.
"""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tarfile
import tomllib


def recipe(source):
    data = tomllib.loads((source / 'submission.toml').read_text())
    required = {'schema', 'id', 'build', 'test', 'package', 'artifacts'}
    if not required <= data.keys() or data.keys() - required - {'workspace_member'}:
        raise ValueError('Unexpected submission recipe fields')
    if type(data['schema']) is not int or data['schema'] != 1:
        raise ValueError('Unsupported source submission schema')
    if not isinstance(data['id'], str) or not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', data['id']):
        raise ValueError('Invalid stable extension ID')
    for phase in ('build', 'test', 'package'):
        commands = data[phase]
        if not isinstance(commands, list) or not commands:
            raise ValueError(f'{phase} needs explicit commands')
        for command in commands:
            if not isinstance(command, list) or not command or not all(
                isinstance(arg, str) and arg and '\x00' not in arg for arg in command
            ):
                raise ValueError('Commands must be nonempty argument arrays, not shell strings')
    artifacts = data['artifacts']
    if not isinstance(artifacts, list) or not artifacts:
        raise ValueError('At least one built package is required')
    for name in artifacts:
        if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.tar\.gz', name):
            raise ValueError('Artifacts must be archive basenames')
    if len(set(artifacts)) != len(artifacts):
        raise ValueError('Duplicate artifact name')
    if 'workspace_member' in data and not re.fullmatch(r'plugin/[a-z][a-z0-9-]*', data['workspace_member']):
        raise ValueError('Invalid workspace member')
    return data


# What a source may give the store to show before anyone installs it: an icon and up to eight screenshots, PNG or WebP,
# each at most 1 MiB, in its `store/` folder (site/build.py and the website read the same rule).
PICTURE = re.compile(r'(icon|screenshot-[1-8])\.(png|webp)')


def store_pictures(source):
    folder = source / 'store'
    if not folder.exists():
        return []
    if not folder.is_dir():
        raise ValueError('store/ must be a folder of pictures')
    names = []
    for path in sorted(folder.iterdir()):
        if not path.is_file() or not PICTURE.fullmatch(path.name):
            raise ValueError(f'store/ holds only icon.png|webp and screenshot-1..8.png|webp: {path.name}')
        body = path.read_bytes()
        png = body.startswith(b'\x89PNG\r\n\x1a\n')
        webp = len(body) > 12 and body[:4] == b'RIFF' and body[8:12] == b'WEBP'
        if not 0 < len(body) <= 1024 * 1024 or not (png if path.suffix == '.png' else webp):
            raise ValueError(f'store/{path.name} is not the kind of picture its name says, or is larger than 1 MiB')
        names.append(path.stem)
    if len(set(names)) != len(names):
        raise ValueError('Two pictures in store/ have the same name')
    return names


def check(source, sdk, output):
    source, sdk, output = source.resolve(), sdk.resolve(), output.resolve()
    data = recipe(source)
    if source.name.removeprefix('peersus-') != data['id']:
        raise ValueError('The source directory and stable extension ID disagree')
    if output.exists():
        raise ValueError('Output must be a new directory')
    if any(path.is_symlink() for path in source.rglob('*')):
        raise ValueError('Source symlinks are not accepted')
    store_pictures(source)
    gate = sdk / 'scripts/check-extension-manifest.py'
    if not gate.is_file():
        raise ValueError('The SDK does not carry the manifest check')
    # The application's own manifest check (licence-and-notice agreement, translations under `languages/`) runs
    # before any submitted command: a manifest it refuses can never be installed, so nothing may be built.
    checked = subprocess.run([sys.executable, str(gate), str(source)],
                             capture_output=True, text=True, timeout=300)
    if checked.returncode:
        reason = (checked.stderr.strip().splitlines() or ['the manifest check failed'])[-1]
        raise ValueError(f'the manifest check refused the package: {reason}')
    # A submission that carries guest glue must use the SDK's own copies, including optional Canvas glue.
    glue = sdk / 'scripts/check-guest-glue.py'
    if not glue.is_file():
        raise ValueError('The SDK does not carry the guest glue check')
    compared = subprocess.run([sys.executable, str(glue), str(source)],
                              capture_output=True, text=True, timeout=300)
    if compared.returncode:
        reason = (compared.stderr.strip().splitlines() or ['the guest glue check failed'])[-1]
        raise ValueError(f"the guest glue is not the SDK's own: {reason}")
    # This is not a sandbox. Run the whole build job on a disposable runner.
    # Validation belongs to a DIFFERENT job, with a fresh trusted checkout.
    with tempfile.TemporaryDirectory(prefix='extension-build-') as temporary:
        work = Path(temporary)
        workspace = work / 'sdk'
        snapshot = work / 'sdk.tar'
        with snapshot.open('wb') as archive:
            subprocess.run(['git', 'archive', '--format=tar', 'HEAD'], cwd=sdk, stdout=archive, check=True)
        workspace.mkdir()
        with tarfile.open(snapshot) as archive:
            archive.extractall(workspace, filter='data')
        member = data.get('workspace_member')
        if member:
            members = tomllib.loads((workspace / 'Cargo.toml').read_text())['workspace']['members']
            if member not in members:
                raise ValueError('The SDK does not declare this workspace member')
            if Path(member).name != source.name or not any(
                (workspace / member / name).is_file() for name in ('plugin.json', 'plugin.toml')
            ):
                raise ValueError('Only the submitted plugin member may replace a bundled SDK plugin')
            plugin = workspace / member
            if plugin.exists():
                shutil.rmtree(plugin)  # Disposable SDK copy only.
            shutil.copytree(source, plugin)
        else:
            # Independent plugin workspaces refer to the pinned host SDK at ../../.
            # Their source is not a member of, or dependency of, the application.
            plugin = workspace / 'plugin' / source.name
            plugin.parent.mkdir(exist_ok=True)
            shutil.copytree(source, plugin)
        products = work / 'products'
        products.mkdir()
        replacements = {'{source}': str(plugin), '{sdk}': str(workspace), '{out}': str(products)}
        for phase in ('build', 'test', 'package'):
            print(f'::group::{phase}', flush=True)
            for command in data[phase]:
                expanded = []
                for argument in command:
                    for token, value in replacements.items():
                        argument = argument.replace(token, value)
                    expanded.append(argument)
                subprocess.run(expanded, cwd=plugin, check=True, timeout=1200)
            print('::endgroup::', flush=True)
        for name in data['artifacts']:
            archive = products / name
            if not archive.is_file() or archive.is_symlink():
                raise ValueError('A declared built package is missing or is a symlink')
        output.mkdir(parents=True)
        for name in data['artifacts']:
            shutil.copyfile(products / name, output / name)
        (output / 'build.json').write_text(json.dumps({
            'schema': 1, 'id': data['id'], 'artifacts': data['artifacts'],
            'status': 'built-not-validated',
        }, indent=2) + '\n')
    return data['artifacts']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--sdk', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        check(args.source, args.sdk, args.out)
        print('Source build and tests passed; separate package validation and maintainer approval are still required.')
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'Source submission refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
