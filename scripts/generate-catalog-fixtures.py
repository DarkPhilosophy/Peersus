#!/usr/bin/env python3
"""Build functional Linux demo archives, signed ONLY with the committed public TEST seed.

Requires a C compiler and Python cryptography. Writes into a new --output directory;
never reads production credentials or changes the application's production trust root.
"""
import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def generate(output):
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise SystemExit('Generate the committed native fixtures on Linux x86_64')
    # Public, intentionally nonsecret fixture seed already used by store/catalog tests.
    signer = Ed25519PrivateKey.from_private_bytes(bytes([7]) * 32)
    output.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('fixture_metadata', ROOT / 'scripts/embed-metadata.py')
    metadata = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(metadata)
    with tempfile.TemporaryDirectory(prefix='catalog-build-') as temporary:
        scratch = Path(temporary)
        for sequence, version, description in [(1, '1.0.0', 'Echoes.'),
                                                (2, '1.0.1', 'Echoes, now louder.')]:
            package = {'name': 'Echo', 'version': version, 'author': 'Someone',
                       'link': 'https://example.org/echo', 'description': description}
            manifest = {'manifest_version': 1, 'capabilities': {}, 'imports': [], 'exports': [],
                        'program': 'echo.so', 'execution': {'kind': 'native_library', 'abi': 1},
                        'package': package,
                        'agreements': {'pages': [
                            {'kind': 'licence', 'title': 'MIT', 'explains': 'You may use and share it.',
                             'changes': 'Nothing changes.', 'spdx': 'MIT',
                             'links': [{'label': 'Licence', 'url': 'https://opensource.org/license/mit'}]},
                            {'kind': 'notice', 'title': 'What it does', 'explains': 'It echoes.',
                             'changes': 'Nothing changes.'}]}}
            manifest_path = scratch / 'plugin.json'
            manifest_bytes = json.dumps(manifest, separators=(',', ':')).encode()
            manifest_path.write_bytes(manifest_bytes)
            record = ','.join(str(byte) for byte in metadata.record(manifest_path))
            source = scratch / 'metadata.c'
            source.write_text('#include "peersus_plugin.h"\n'
                              'PEERSUS_METADATA_RECORD(' + record + ');\n')
            module = scratch / 'echo.so'
            subprocess.run([os.environ.get('CC', 'cc'), '-shared', '-fPIC', '-O2', '-nostdlib',
                            '-fno-stack-protector', '-Wl,--build-id=none',
                            '-I', str(ROOT / 'crates/peersus-plugin-runtime/sdk'),
                            str(ROOT / 'crates/peersus-plugin-runtime/sdk/echo.c'),
                            str(source), '-o', str(module)], check=True, timeout=30)
            data = io.BytesIO()
            with gzip.GzipFile(fileobj=data, mode='wb', mtime=0, filename='') as zipped:
                with tarfile.open(fileobj=zipped, mode='w', format=tarfile.USTAR_FORMAT) as archive:
                    for name, payload in [('echo/echo.so', module.read_bytes()),
                                          ('echo/plugin.json', manifest_bytes)]:
                        entry = tarfile.TarInfo(name)
                        entry.size = len(payload)
                        entry.mode = 0o644
                        archive.addfile(entry, io.BytesIO(payload))
            archive_bytes = data.getvalue()
            catalog = {'schema': 1, 'sequence': sequence, 'entries': [{
                'name': 'echo', 'version': version, 'author': package['author'],
                'description': description, 'link': package['link'], 'archive': 'echo.tar.gz',
                'size': len(archive_bytes), 'sha256': hashlib.sha256(archive_bytes).hexdigest()}]}
            catalog_bytes = json.dumps(catalog, sort_keys=True, separators=(',', ':')).encode()
            destination = output / ('catalog' if sequence == 1 else 'catalog-v2')
            destination.mkdir()
            (destination / 'echo.tar.gz').write_bytes(archive_bytes)
            (destination / 'catalog.json').write_bytes(catalog_bytes)
            (destination / 'catalog.sig').write_bytes(signer.sign(catalog_bytes))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    generate(parser.parse_args().output)
