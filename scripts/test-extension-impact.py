#!/usr/bin/env python3
"""Exercise extension sizes and benchmark inputs on local packages; nothing is downloaded or run."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('impact', Path(__file__).with_name('extension-impact.py'))
impact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(impact)


def packages(directory, version, built):
    """Write `built` ({archive name: (kind, architecture, platforms, {file: size})}) and its review.json."""
    directory.mkdir()
    listed = []
    for name, (kind, architecture, platforms, files) in built.items():
        with tarfile.open(directory / name, 'w:gz') as archive:
            for file, size in files.items():
                member = tarfile.TarInfo(file)
                member.size = size
                archive.addfile(member, io.BytesIO(bytes(size)))
        listed.append({'archive': name, 'version': version, 'architecture': architecture, 'platforms': platforms,
                       'execution': {'kind': kind}})
    (directory / 'review.json').write_text(json.dumps({'id': 'sample', 'packages': listed}))
    return directory


class Impact(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.root = Path(self.folder.name)

    def tearDown(self):
        self.folder.cleanup()

    def test_a_package_is_measured_by_what_it_is_for_whatever_its_file_is_called(self):
        before = packages(self.root / 'before', '1.0.0', {
            'old-name.tar.gz': ('wasm', 'any', ['linux'], {'manifest.json': 300, 'guest.wasm': 70_000}),
        })
        after = packages(self.root / 'after', '1.1.0', {
            'sample-linux.tar.gz': ('wasm', 'any', ['linux'], {'manifest.json': 300, 'guest.wasm': 150_000, 'icon.png': 900}),
            'sample-linux-process.tar.gz': ('process', 'x86_64', ['linux'], {'manifest.json': 300, 'sample': 500_000}),
        })
        earlier, later = impact.measured(before), impact.measured(after)
        self.assertEqual((earlier['version'], later['version']), ('1.0.0', '1.1.0'))
        # The same target under another file name is the same row of a comparison.
        self.assertEqual(sorted(earlier['package']), ['wasm-any-linux'])
        self.assertEqual(sorted(later['package']), ['process-x86_64-linux', 'wasm-any-linux'])
        guest = later['package']['wasm-any-linux']
        self.assertEqual((guest['unpacked_bytes'], guest['largest_file_bytes'], guest['files']), (151_200, 150_000, 3))
        self.assertEqual(guest['file_bytes'], (after / 'sample-linux.tar.gz').stat().st_size)
        self.assertEqual(earlier['package']['wasm-any-linux']['largest_file_bytes'], 70_000)

    def test_two_packages_for_the_same_target_and_a_listed_package_that_is_not_there_are_refused(self):
        twice = packages(self.root / 'twice', '1.0.0', {
            'a.tar.gz': ('wasm', 'any', ['linux', 'android'], {'guest.wasm': 10}),
            'b.tar.gz': ('wasm', 'any', ['android', 'linux'], {'guest.wasm': 10}),
        })
        with self.assertRaises(ValueError):
            impact.measured(twice)
        gone = packages(self.root / 'gone', '1.0.0', {'a.tar.gz': ('wasm', 'any', ['linux'], {'guest.wasm': 10})})
        (gone / 'a.tar.gz').unlink()
        with self.assertRaises(ValueError):
            impact.measured(gone)

    def test_an_extension_new_in_the_store_reports_its_sizes_and_no_change(self):
        new = packages(self.root / 'new', '0.1.0', {'sample-linux.tar.gz': ('wasm', 'any', ['linux'], {'guest.wasm': 2_097_152})})
        report = impact.alone(impact.measured(new))
        self.assertTrue(report.startswith('## Resource impact: INCONCLUSIVE'))
        self.assertIn('| Largest file of the package · `wasm-any-linux` | none | 2.00 MiB | n/a |', report)

    def test_runtime_input_contains_only_the_reviewed_linux_wasm_and_its_manifest(self):
        directory = self.root / 'reviewed'
        directory.mkdir()
        manifest = json.dumps({'execution': {'kind': 'wasm', 'abi': 1}, 'program': 'custom.wasm'}).encode()
        wasm = b'\0asm\x01\0\0\0'
        source = directory / 'linux.tar.gz'
        with tarfile.open(source, 'w:gz') as archive:
            for name, body in [('sample/plugin.json', manifest), ('sample/custom.wasm', wasm), ('sample/ignored.png', b'ignored')]:
                member = tarfile.TarInfo(name)
                member.size = len(body)
                archive.addfile(member, io.BytesIO(body))
        linux = {'archive': source.name, 'architecture': 'any', 'platforms': ['linux'],
                 'archive_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
                 'execution': {'kind': 'wasm', 'abi': 1}}
        native = {'archive': 'not-present.tar.gz', 'architecture': 'x86_64', 'platforms': ['linux'],
                  'execution': {'kind': 'process'}}
        review = {'id': 'sample', 'packages': [native, linux]}
        (directory / 'review.json').write_text(json.dumps(review))
        output = self.root / 'input'
        impact.stage_wasm(directory, output)
        self.assertEqual({file.name for file in output.iterdir()}, {'plugin.json', 'guest.wasm'})
        self.assertEqual((output / 'plugin.json').read_bytes(), manifest)
        self.assertEqual((output / 'guest.wasm').read_bytes(), wasm)
        self.assertFalse((output / 'guest.wasm').stat().st_mode & 0o111)

        with source.open('ab') as archive:
            archive.write(b'tampered')
        with self.assertRaisesRegex(ValueError, 'digest'):
            impact.stage_wasm(directory, self.root / 'tampered')
        self.assertFalse((self.root / 'tampered').exists())

    def test_a_native_only_review_cannot_supply_a_runtime_sample(self):
        directory = packages(self.root / 'native-only', '0.1.0', {
            'native.tar.gz': ('process', 'x86_64', ['linux'], {'sample': 100}),
        })
        with self.assertRaisesRegex(ValueError, 'Linux WASM'):
            impact.stage_wasm(directory, self.root / 'input')


if __name__ == '__main__':
    unittest.main(verbosity=2)
