#!/usr/bin/env python3
"""Source build contract regressions; all commands and files are synthetic and local."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BUILD = Path(__file__).with_name('check-extension-source.py')


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
        subprocess.run(['git', '-C', str(self.sdk), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        '-c', 'commit.gpgsign=false', 'commit', '-qm', 'SDK fixture'], check=True)
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
        self.recipe()

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

    def test_archive_path_traversal_is_refused(self):
        path = self.source / 'submission.toml'
        path.write_text(path.read_text().replace('artifacts = ["fly-me.tar.gz"]', 'artifacts = ["../fly-me.tar.gz"]'))
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertFalse(self.out.exists())


if __name__ == '__main__':
    unittest.main()
