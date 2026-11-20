#!/usr/bin/env python3
"""Source build contract regressions; all commands and files are synthetic and local."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BUILD = Path(__file__).with_name('check-extension-source.py')
GLUE_CHECK = Path(__file__).with_name('check-guest-glue.py')
GLUE = Path(__file__).resolve().parent.parent / 'templates' / 'guest'


class SourceBuild(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.sdk = self.root / 'sdk'
        self.sdk.mkdir()
        (self.sdk / 'sentinel').write_text('original')
        self.source = self.root / 'fly-me'
        self.source.mkdir()
        self.out = self.root / 'output'
        subprocess.run(['git', 'init', '-q', str(self.sdk)], check=True)
        subprocess.run(['git', '-C', str(self.sdk), 'add', 'sentinel'], check=True)
        # A workstation commit policy may refuse a fixture identity. It is the owner's control:
        # only that explicit refusal skips, and hosted CI still runs these tests.
        committed = subprocess.run(
            ['git', '-C', str(self.sdk), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
             '-c', 'commit.gpgsign=false', 'commit', '-qm', 'SDK fixture'], capture_output=True, text=True)
        if committed.returncode and 'BLOCKED' in committed.stderr:
            self.skipTest('workstation git policy refuses fixture commits; runs on CI')
        self.assertEqual(committed.returncode, 0, committed.stderr)
        (self.source / 'step.py').write_text('''import pathlib, sys
phase, sdk, output = sys.argv[1:]
sdk, output = pathlib.Path(sdk), pathlib.Path(output)
trace = sdk / 'trace'
if phase == 'build':
    (sdk / 'sentinel').write_text('changed inside disposable SDK')
    trace.write_text('built')
elif phase == 'test':
    assert trace.read_text() == 'built'
    trace.write_text('tested')
elif phase == 'package':
    assert trace.read_text() == 'tested'
    (output / 'fly-me.tar.gz').write_bytes(b'candidate, not yet validated')
else:
    raise SystemExit(7)
''')
        self.gate()
        self.recipe()

    def gate(self, refuse=False, glue_refuse=False):
        code = ("import sys\nsys.stderr.write('the manifest carries no licence-and-notice agreement\\n')\nraise SystemExit(2)\n"
                if refuse else 'raise SystemExit(0)\n')
        glue = ("import sys\nsys.stderr.write('wasm/src/host.rs differs from templates/guest/host.rs\\n')\nraise SystemExit(1)\n"
                if glue_refuse else 'raise SystemExit(0)\n')
        (self.sdk / 'scripts').mkdir(exist_ok=True)
        (self.sdk / 'scripts/check-extension-manifest.py').write_text(code)
        (self.sdk / 'scripts/check-guest-glue.py').write_text(glue)

    def recipe(self, fail=False):
        commands = []
        for phase in ('build', 'test', 'package'):
            step = 'fail' if fail and phase == 'test' else phase
            command = [sys.executable, '{source}/step.py', step, '{sdk}', '{out}']
            commands.append(f'{phase} = [{json.dumps(command)}]')
        (self.source / 'submission.toml').write_text(
            'schema = 1\nid = "fly-me"\nartifacts = ["fly-me.tar.gz"]\n' + '\n'.join(commands))

    def run_build(self):
        return subprocess.run([sys.executable, str(BUILD), '--source', str(self.source),
                               '--sdk', str(self.sdk), '--out', str(self.out)],
                              capture_output=True, text=True)

    def test_a_manifest_without_a_valid_agreement_is_refused_before_any_command(self):
        self.gate(refuse=True)
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('licence-and-notice agreement', result.stderr)
        self.assertFalse(self.out.exists())
        self.assertFalse((self.sdk / 'trace').exists(), 'the gate ran after untrusted commands')

    def test_guest_glue_that_is_not_the_sdks_own_is_refused_before_any_command(self):
        self.gate(glue_refuse=True)
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('guest glue', result.stderr)
        self.assertFalse(self.out.exists())
        self.assertFalse((self.sdk / 'trace').exists(), 'the glue was compared after untrusted commands')

    def test_an_sdk_without_the_glue_check_refuses_everything(self):
        (self.sdk / 'scripts/check-guest-glue.py').unlink()
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.sdk / 'trace').exists())

    def test_build_tests_and_packaging_execute_in_order_without_mutating_sdk(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.sdk / 'sentinel').read_text(), 'original')
        self.assertEqual((self.out / 'fly-me.tar.gz').read_bytes(), b'candidate, not yet validated')
        report = json.loads((self.out / 'build.json').read_text())
        self.assertEqual(report['status'], 'built-not-validated')
        self.assertFalse((self.out / 'review.json').exists())

    def test_failed_tests_do_not_produce_a_publishable_output(self):
        self.recipe(fail=True)
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.out.exists())

    def test_an_existing_output_is_preserved(self):
        self.out.mkdir()
        (self.out / 'keep').write_text('preserved')
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertEqual((self.out / 'keep').read_text(), 'preserved')

    def test_source_symlinks_are_refused_before_commands_execute(self):
        (self.source / 'escape').symlink_to(self.sdk)
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertFalse(self.out.exists())

    def test_store_pictures_are_checked_before_any_command(self):
        store = self.source / 'store'
        store.mkdir()
        (store / 'icon.png').write_bytes(b'\x89PNG\r\n\x1a\n' + bytes(16))
        (store / 'screenshot-1.webp').write_bytes(b'RIFF' + bytes(4) + b'WEBP' + bytes(8))
        for name, body in (('screenshot-2.png', b'GIF89a' + bytes(16)), ('banner.png', b'\x89PNG\r\n\x1a\n'), ('screenshot-9.webp', b'RIFF' + bytes(4) + b'WEBP')):
            with self.subTest(name=name):
                (store / name).write_bytes(body)
                self.assertNotEqual(self.run_build().returncode, 0)
                self.assertFalse(self.out.exists())
                (store / name).unlink()

    def test_archive_path_traversal_is_refused(self):
        path = self.source / 'submission.toml'
        path.write_text(path.read_text().replace('artifacts = ["fly-me.tar.gz"]', 'artifacts = ["../fly-me.tar.gz"]'))
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertFalse(self.out.exists())


class GuestGlue(unittest.TestCase):
    """The real check against the real one copy: only the extension is synthetic."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.source = Path(self.tmp.name) / 'thing'
        (self.source / 'wasm/src').mkdir(parents=True)

    def compare(self):
        return subprocess.run([sys.executable, str(GLUE_CHECK), str(self.source)], capture_output=True, text=True)

    def test_a_guest_that_carries_no_glue_file_passes(self):
        (self.source / 'wasm/src/lib.rs').write_text('// its own few lines\n')
        self.assertEqual(self.compare().returncode, 0)

    def test_exact_copies_pass_and_one_changed_byte_is_refused_by_name(self):
        for name in ('host.rs', 'native.rs'):
            (self.source / 'wasm/src' / name).write_bytes((GLUE / name).read_bytes())
        self.assertEqual(self.compare().returncode, 0)
        native = self.source / 'wasm/src/native.rs'
        native.write_bytes(native.read_bytes() + b'\n')
        result = self.compare()
        self.assertEqual(result.returncode, 1)
        self.assertIn('native.rs differs', result.stderr)
        self.assertNotIn('host.rs', result.stderr)

    def test_a_link_in_place_of_a_glue_file_is_refused(self):
        (self.source / 'wasm/src/host.rs').symlink_to(GLUE / 'host.rs')
        self.assertEqual(self.compare().returncode, 1)

    def test_no_extension_named_is_a_usage_error_not_a_pass(self):
        self.assertEqual(subprocess.run([sys.executable, str(GLUE_CHECK)], capture_output=True).returncode, 2)


if __name__ == '__main__':
    unittest.main()
