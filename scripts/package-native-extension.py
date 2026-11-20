#!/usr/bin/env python3
"""Package one built extension artifact; never sign, install, publish or run it.

Nothing here names a plugin. The recipe says which one this is (`--id`), the manifest says what it runs as
(`execution.kind`) and where (`platforms`), and this only packs what was built.

`--target-dir` is where the build left the program, and it means different things by kind:
  native_library or process  the directory the built library, executable and any companions are directly in.
  wasm     the Cargo target ROOT, `<target>`: the program is looked for under `wasm32-unknown-unknown/release` there,
           which is where `cargo build --release --target wasm32-unknown-unknown` puts it.
Run once per declared artifact. The store submission needs WASM; a native library (not recommended)
and a standalone executable (not recommended, unsupported) are independent optional archives.
"""
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
         '--format-version', '1', '--filter-platform', host], cwd=sdk))
    packages = {package['id']: package for package in metadata['packages']}
    # Both sides resolved: a source under a linked folder (`/home` -> `/var/home`) is still the same member.
    wanted = (source / 'Cargo.toml').resolve()
    roots = [package['id'] for package in packages.values()
             if Path(package['manifest_path']).resolve() == wanted]
    if len(roots) != 1:
        raise ValueError('The submitted source must be the built SDK workspace member')
    # `cargo metadata` unifies features across the entire workspace: traversing
    # that graph incorrectly includes unrelated UI codecs and dev dependencies.
    # Ask Cargo for this package's actual target-linked graph instead.
    tree = subprocess.check_output(
        ['cargo', 'tree', '--manifest-path', str(sdk / 'Cargo.toml'), '--locked',
         '-p', roots[0], '--edges', 'normal,no-proc-macro',
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


def guest_licenses(sdk, source, name):
    """Attribution for a WebAssembly guest. It is built for no host, so the host-only filter above would list the wrong
    dependencies; its own lockfile's closure is what ships, and every one of them must carry a license.

    `--offline`, as for the host graph: attribution is read from what the lockfile already fetched, never a network."""
    manifest_path = source / 'Cargo.toml'
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--manifest-path', str(manifest_path), '--locked', '--offline',
         '--format-version', '1'], cwd=sdk))
    files, notices = {}, [f'Third-party source licenses for the {name} WebAssembly package.', '']
    for package in sorted(metadata['packages'], key=lambda item: (item['name'], item['version'])):
        if package['source'] is None:
            continue
        root = Path(package['manifest_path']).resolve().parent
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


PLATFORMS = ('linux', 'android', 'macos', 'windows')


def valid_id(value):
    """The application's identifier grammar, also what the validator demands of the archive's top directory."""
    return (0 < len(value) <= 128 and not value.startswith('.')
            and all(c.isascii() and (c.isalnum() or c in '._-') for c in value))


def package(source, sdk, target, output, platform, licenses=None, plugin_id=None, native_library=None, process=None):
    source, sdk, target = source.resolve(), sdk.resolve(), target.resolve()
    # The id is what the submission recipe declares. It is never guessed from a directory name, and no plugin is
    # named here: any plugin that obeys the identifier grammar can be packaged.
    if plugin_id is None:
        plugin_id = source.name.removeprefix('peersus-')
    if not valid_id(plugin_id) or platform not in PLATFORMS:
        raise ValueError('The plugin id or platform is not valid')
    manifest = json.loads((source / 'plugin.json').read_text())
    if 'execution' not in manifest:
        raise ValueError('plugin.json must explicitly declare `execution`')
    if native_library is not None and process is not None:
        raise ValueError('A package is one native library or one process, not both')
    if process is not None:
        # The guest linked into the native host as an optional self-contained program. Not a supported tier.
        # Only ELF platforms here.
        if platform not in ('linux', 'android') or Path(process).name != process:
            raise ValueError('A process is packaged for Linux or Android, by its plain file name')
        manifest['execution'] = {'kind': 'process'}
        manifest['program'] = process
    if native_library is not None:
        # The same guest built as a native library (second tier, not recommended): its manifest is the guest's, with
        # the executor and the program file the native build produced. Only ELF platforms can load one here.
        if platform not in ('linux', 'android') or Path(native_library).name != native_library:
            raise ValueError('A native library is packaged for Linux or Android, by its plain file name')
        manifest['execution'] = {'kind': 'native_library', 'abi': 1}
        manifest['program'] = native_library
    kind = (manifest.get('execution') or {}).get('kind')
    if kind not in ('process', 'wasm', 'native_library') or platform not in manifest.get('platforms', []):
        raise ValueError('The source must explicitly support this platform with a process, native library or '
                         'WebAssembly program')
    manifest['platforms'] = [platform]
    # Settings (including their platform selectors) are part of the shared contract.
    # Only the top-level installation platform varies; the host filters settings when it uses them.
    for setting in manifest.get('contributions', {}).get('settings', []):
        targets = setting.get('platforms', [])
        if not isinstance(targets, list) or any(not isinstance(p, str) for p in targets):
            raise ValueError('Setting platforms must be a list of platform names')
    program = manifest['program']
    companions = manifest.get('files', [])
    # Where the build left the program. A process is built for the host, straight under the release directory it was
    # given. A guest is built for the WebAssembly target, one directory further under the Cargo target root it was given.
    built = target / 'wasm32-unknown-unknown/release' if kind == 'wasm' else target
    for name in [program, *companions]:
        if not isinstance(name, str) or Path(name).name != name or name in ('', '.', '..'):
            raise ValueError('Built program and companion names must be plain basenames')
        binary = built / name
        if not binary.is_file() or binary.is_symlink():
            raise ValueError('A built program or companion is missing or is a symbolic link')
    if kind == 'wasm' and not (built / program).read_bytes().startswith(b'\0asm\x01\0\0\0'):
        raise ValueError('The built program is not a WebAssembly module')
    if kind == 'native_library' and not (built / program).read_bytes().startswith(b'\x7fELF'):
        raise ValueError('The built native library is not an ELF shared object')
    if licenses is None:
        # A process linked from the guest has the guest's dependencies; a workspace process program has its own.
        files = guest_licenses(sdk, source, manifest['package']['name']) if kind != 'process' or process \
            else dependency_licenses(sdk, source)
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
    # The extension's own translations of its manifest's words, beside that manifest: `languages/<tag>.po`. The application
    # reads them from the installed package, checks each there and shows English where one is missing or unfit.
    languages = source / 'languages'
    if languages.is_symlink():
        raise ValueError('The language folder must not be a symbolic link')
    if languages.is_dir():
        for path in sorted(languages.iterdir()):
            if path.is_symlink() or not path.is_file() or path.suffix != '.po':
                raise ValueError('The language folder holds only plain <language>.po files')
            files[f'languages/{path.name}'] = path.read_bytes()
    # The official WebAssembly build keeps the plain platform name; any optional native archive
    # appends its execution kind so that multiple kinds for one platform do not collide.
    suffix = '' if kind == 'wasm' else f'-{kind}'
    archive = output / f'{plugin_id}-{platform}{suffix}.tar.gz'
    if archive.exists() or archive.is_symlink():
        raise ValueError('The output archive must not already exist')
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.native-extension-', dir=output) as temp:
        temp = Path(temp)
        embedded = temp / program
        if kind == 'wasm':
            # A guest has no sections to add to: the record becomes a custom section of the module.
            embedded.write_bytes(subprocess.check_output(
                ['python3', str(sdk / 'scripts/embed-metadata.py'), 'wasm', str(source / 'plugin.json'),
                 str(built / program)]))
        else:
            record = subprocess.check_output(['python3', str(sdk / 'scripts/embed-metadata.py'),
                                              'bin', str(source / 'plugin.json')])
            record_path = temp / 'metadata.bin'
            record_path.write_bytes(record)
            subprocess.run(['objcopy', '--add-section', f'peersus.meta={record_path}',
                            '--set-section-flags', 'peersus.meta=readonly,contents',
                            str(built / program), str(embedded)], check=True)
        files[program] = embedded.read_bytes()
        for name in companions:
            if name in files:
                raise ValueError('A companion conflicts with an attribution file')
            files[name] = (built / name).read_bytes()
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
    parser.add_argument('--platform', choices=PLATFORMS, required=True)
    parser.add_argument('--licenses', type=Path)
    parser.add_argument('--process', metavar='FILE',
                        help='Package an optional guest linked into a standalone executable (not recommended, '
                             'unsupported). --target-dir is the directory containing the program.')
    parser.add_argument('--native-library', metavar='FILE',
                        help='Package an optional native library (second tier, not recommended). '
                             '--target-dir is the directory containing the library.')
    parser.add_argument('--id', dest='plugin_id',
                        help="The plugin's id, as its submission recipe declares it. Without it the id is the "
                             "source directory's name without a 'peersus-' prefix, which only suits older recipes.")
    args = parser.parse_args()
    try:
        package(args.source, args.sdk, args.target_dir, args.out, args.platform, args.licenses, args.plugin_id, args.native_library, args.process)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Packaging failed: {error}\n')
    print('Packaged the built extension; independent validation and maintainer approval are still required.')


if __name__ == '__main__':
    main()
