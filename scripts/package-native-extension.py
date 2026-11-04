#!/usr/bin/env python3
"""Package a built first-party process; never sign, install, publish or run it."""
import argparse
import gzip
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


def dependency_licenses(sdk, source):
    host = next(line.removeprefix('host: ') for line in
                subprocess.check_output(['rustc', '-vV'], text=True).splitlines()
                if line.startswith('host: '))
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--manifest-path', str(sdk / 'Cargo.toml'), '--locked',
         '--offline', '--format-version', '1', '--filter-platform', host], cwd=sdk))
    packages = {package['id']: package for package in metadata['packages']}
    roots = [package['id'] for package in packages.values()
             if Path(package['manifest_path']).resolve() == source / 'Cargo.toml']
    if len(roots) != 1:
        raise ValueError('The submitted source must be the built SDK workspace member')
    # `cargo metadata` unifies features across the entire workspace: traversing
    # that graph incorrectly includes unrelated UI codecs and dev dependencies.
    # Ask Cargo for this package's actual target-linked graph instead.
    tree = subprocess.check_output(
        ['cargo', 'tree', '--manifest-path', str(sdk / 'Cargo.toml'), '--locked',
         '--offline', '-p', roots[0], '--edges', 'normal,no-proc-macro',
         '--target', host, '--prefix', 'none', '--format', '{p}'], cwd=sdk, text=True)
    selected = {(fields[0], fields[1].removeprefix('v'))
                for line in tree.splitlines() if len(fields := line.split()) >= 2
                and fields[1].startswith('v')}
    root = packages[roots[0]]
    if (root['name'], root['version']) not in selected:
        raise ValueError('Cargo did not enumerate the requested package dependency graph')
    files, notices = {}, ['Third-party licenses for this native extension.', '']
    for package in sorted((p for p in packages.values() if (p['name'], p['version']) in selected),
                          key=lambda p: (p['name'], p['version'])):
        if package['source'] is None:
            continue
        root = Path(package['manifest_path']).parent
        licenses = sorted({path for pattern in ('LICENSE*', 'COPYING*', 'NOTICE*')
                           for path in root.glob(pattern) if path.is_file() and not path.is_symlink()})
        if not licenses or not package['license']:
            raise ValueError(f"Missing dependency license attribution: {package['name']}")
        notices.append(f"{package['name']} {package['version']}: {package['license']}")
        for path in licenses:
            if any(word in path.name.lower() for word in ('secret', 'private_key', 'api_key', 'token')):
                raise ValueError('Unexpected credential-like license filename')
            files[f"licenses/{package['name']}-{package['version']}/{path.name}"] = path.read_bytes()
    files['THIRD_PARTY_NOTICES.txt'] = ('\n'.join(notices) + '\n').encode()
    return files


def package(source, sdk, target, output, platform, licenses=None):
    source, sdk, target = source.resolve(), sdk.resolve(), target.resolve()
    plugin_id = source.name.removeprefix('peersus-')
    if source.name not in ('peersus-clipboard', 'peersus-upload') or platform != 'linux':
        raise ValueError('This first-party packager currently builds Linux process artifacts')
    manifest = json.loads((source / 'plugin.json').read_text())
    if manifest.get('execution') != {'kind': 'process'} or platform not in manifest.get('platforms', []):
        raise ValueError('The source must explicitly support this process platform')
    manifest['platforms'] = [platform]
    program = manifest['program']
    companions = manifest.get('files', [])
    for name in [program, *companions]:
        if not isinstance(name, str) or Path(name).name != name or name in ('', '.', '..'):
            raise ValueError('Built program and companion names must be plain basenames')
        binary = target / name
        if not binary.is_file() or binary.is_symlink():
            raise ValueError('A built program or companion is missing or is a symbolic link')
    if licenses is None:
        files = dependency_licenses(sdk, source)
    else:
        # A caller may supply an already collected attribution tree. In production
        # recipes this option is omitted and the actual Cargo dependency closure is used.
        files = {}
        for path in licenses.rglob('*'):
            if path.is_symlink():
                raise ValueError('Attribution files must not be symbolic links')
            if path.is_file():
                files[path.relative_to(licenses).as_posix()] = path.read_bytes()
        if 'THIRD_PARTY_NOTICES.txt' not in files:
            raise ValueError('The attribution tree needs its notices document')
    for name in ('LICENSE', 'NOTICE'):
        files[name] = (sdk / name).read_bytes()
    archive = output / f'{plugin_id}-{platform}.tar.gz'
    if archive.exists() or archive.is_symlink():
        raise ValueError('The output archive must not already exist')
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.native-extension-', dir=output) as temp:
        temp = Path(temp)
        record = subprocess.check_output(['python3', str(sdk / 'scripts/embed-metadata.py'),
                                          'bin', str(source / 'plugin.json')])
        record_path = temp / 'metadata.bin'
        record_path.write_bytes(record)
        embedded = temp / program
        subprocess.run(['objcopy', '--add-section', f'peersus.meta={record_path}',
                        '--set-section-flags', 'peersus.meta=readonly,contents',
                        str(target / program), str(embedded)], check=True)
        files[program] = embedded.read_bytes()
        for name in companions:
            if name in files:
                raise ValueError('A companion conflicts with an attribution file')
            files[name] = (target / name).read_bytes()
        manifest['files'] = sorted(name for name in files if name != program)
        files['plugin.json'] = (json.dumps(manifest, indent=2, ensure_ascii=False) + '\n').encode()
        staged_archive = temp / 'package.tar.gz'
        with staged_archive.open('xb') as raw:
            with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode='w', format=tarfile.PAX_FORMAT) as tar:
                    for name, data in sorted(files.items()):
                        member = tarfile.TarInfo(f'{plugin_id}/{name}')
                        member.size = len(data)
                        member.mode = 0o700 if name in [program, *companions] else 0o600
                        member.mtime = 0
                        tar.addfile(member, io.BytesIO(data))
        # Same-filesystem linking publishes complete bytes atomically and refuses
        # an existing archive, including one created by a concurrent caller.
        os.link(staged_archive, archive)
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('source', 'sdk', 'target-dir', 'out'):
        parser.add_argument(f'--{option}', type=Path, required=True)
    parser.add_argument('--platform', choices=['linux'], required=True)
    parser.add_argument('--licenses', type=Path)
    args = parser.parse_args()
    try:
        package(args.source, args.sdk, args.target_dir, args.out, args.platform, args.licenses)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Native packaging failed: {error}\n')
    print('Packaged the built native process; independent validation and maintainer approval are still required.')


if __name__ == '__main__':
    main()
