#!/usr/bin/env python3
"""Check the first-party policy against temporary declarations; no build tools or local plugins are needed."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


CHECK = Path(__file__).with_name('check-first-party-extensions.py')
IDS = ('clipboard', 'control', 'location', 'monitor', 'test', 'upload')
PLATFORMS = ['linux', 'android', 'macos', 'windows']
GUEST_BUILD = ('cargo build --manifest-path {source}/wasm/Cargo.toml --locked --release '
               '--target wasm32-unknown-unknown --target-dir {sdk}/target')
NATIVE_BUILD = ('cargo build --manifest-path {source}/wasm/Cargo.toml --locked --release '
                '--features native --target-dir {sdk}/target')
LINK_PROCESS = ('cc -std=c11 -O2 -Wall -Wextra -Werror -fstack-protector-strong -fPIE -pie -DPEERSUS_LINKED_MODULE '
                '{sdk}/crates/peersus-plugin-runtime/native/host.c {sdk}/target/release/libpeersus_ID_wasm.a '
                '-Wl,-z,relro,-z,now -lpthread -ldl -lm -o {sdk}/target/release/peersus-ID-process')
GUEST_TEST = 'cargo test --manifest-path {source}/wasm/Cargo.toml --locked --target-dir {sdk}/target'
TRANSLATIONS = ('cargo run --manifest-path {sdk}/Cargo.toml --locked -p xtask --target-dir {sdk}/target '
                '-- check-manifest --complete-translations {source}/wasm')
PACKAGE = ('python3 {sdk}/scripts/package-native-extension.py --source {source}/wasm --sdk {sdk} '
           '--target-dir {sdk}/targetDIR --out {out} --platform OS --id ID')
CANVAS_COMPARE = 'cmp {sdk}/templates/guest/canvas.rs {source}/wasm/src/canvas_sdk.rs'


def recipe(plugin, *, keep_canvas_compare=True):
    wasm = [f'{plugin}-{platform}.tar.gz' for platform in PLATFORMS]
    build, test = [GUEST_BUILD], [GUEST_TEST]
    package = [PACKAGE.replace('DIR', '').replace('OS', platform) for platform in PLATFORMS]
    if plugin == 'location':
        if keep_canvas_compare:
            build.insert(0, CANVAS_COMPARE)
    else:
        build.extend([NATIVE_BUILD, LINK_PROCESS])
        native_package = PACKAGE.replace('DIR', '/release').replace('OS', 'linux')
        package.extend([native_package + ' --native-library libpeersus_ID_wasm.so',
                        native_package + ' --process peersus-ID-process'])
        wasm.extend([f'{plugin}-linux-native_library.tar.gz', f'{plugin}-linux-process.tar.gz'])
    if plugin == 'clipboard':
        test.append(TRANSLATIONS)
    return {'schema': 1, 'id': plugin, 'artifacts': wasm,
            **{phase: [cmd.replace('ID', plugin).split() for cmd in commands]
               for phase, commands in [('build', build), ('test', test), ('package', package)]}}


class FirstParty(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)

    def source(self, plugin, data=None, **manifest):
        folder = self.root / f'peersus-{plugin}'
        (folder / 'wasm').mkdir(parents=True, exist_ok=True)
        (folder / 'submission.toml').write_text(''.join(
            f'{key} = {json.dumps(value)}\n' for key, value in (data if data is not None else recipe(plugin)).items()),
            encoding='utf-8')
        document = {'manifest_version': 1, 'execution': {'kind': 'wasm', 'abi': 1},
                    'program': f'peersus_{plugin}_wasm.wasm', 'platforms': PLATFORMS, **manifest}
        (folder / 'wasm/plugin.json').write_text(json.dumps(document), encoding='utf-8')
        return folder

    def check(self, source=None):
        argv = ['--source', str(source)] if source is not None else ['--root', str(self.root)]
        return subprocess.run([sys.executable, str(CHECK), *argv], capture_output=True, text=True, timeout=10)

    def test_complete_roster_passes_and_location_is_the_only_form_exception(self):
        for plugin in IDS:
            self.source(plugin)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Location', result.stdout)
        self.assertIn('canvas_present', result.stdout)
        self.assertIn('34', result.stdout)

    def test_missing_source_is_named_and_cannot_silently_shrink_the_roster(self):
        for plugin in IDS[:-1]:
            self.source(plugin)
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('peersus-upload', result.stderr)

    def test_a_platform_named_in_the_manifest_needs_its_archive(self):
        data = recipe('monitor')
        data['artifacts'].remove('monitor-windows.tar.gz')
        result = self.check(self.source('monitor', data))
        self.assertEqual(result.returncode, 1)
        self.assertIn('monitor-windows.tar.gz', result.stderr)

    def test_a_native_form_cannot_be_omitted_by_another_extension(self):
        for form in ('native_library', 'process'):
            with self.subTest(form=form):
                data = recipe('test')
                name = f'test-linux-{form}.tar.gz'
                data['artifacts'].remove(name)
                result = self.check(self.source('test', data))
                self.assertEqual(result.returncode, 1)
                self.assertIn(name, result.stderr)

    def test_the_manifest_must_allow_all_four_platforms_and_name_the_built_program(self):
        for overrides, reason in [({'platforms': PLATFORMS[:-1]}, 'windows'),
                                  ({'program': 'not-the-built-program.wasm'}, 'program'),
                                  ({'execution': {'kind': 'process'}}, 'execution')]:
            with self.subTest(overrides=overrides):
                result = self.check(self.source('upload', **overrides))
                self.assertEqual(result.returncode, 1)
                self.assertIn(reason, result.stderr)

    def test_format_and_name_can_stay_the_same_while_a_recipe_vector_is_wrong(self):
        for phase, vector, token, replacement in [
            ('build', 2, 'libpeersus_control_wasm.a', 'libpeersus_control_wasm.so'),
            ('test', 0, '--locked', '--offline'),
            ('package', 0, '{sdk}/target', '{sdk}/target/release'),
        ]:
            with self.subTest(phase=phase):
                data = recipe('control')
                command = data[phase][vector]
                data[phase][vector] = [arg.replace(token, replacement) for arg in command]
                result = self.check(self.source('control', data))
                self.assertEqual(result.returncode, 1)
                self.assertIn(f'{phase}[{vector + 1}]', result.stderr)

    def test_clipboard_translation_check_must_use_the_locked_shared_target(self):
        data = recipe('clipboard')
        command = data['test'][1]
        offset = command.index('--target-dir')
        del command[offset:offset + 2]
        result = self.check(self.source('clipboard', data))
        self.assertEqual(result.returncode, 1)
        self.assertIn('test[2]', result.stderr)
        self.assertIn('--target-dir', result.stderr)

    def test_location_may_drop_only_the_precise_legacy_canvas_cmp(self):
        data = recipe('location', keep_canvas_compare=False)
        source = self.source('location', data)
        result = self.check(source)
        self.assertEqual(result.returncode, 0, result.stderr)
        data['build'].insert(0, ['cmp', '{sdk}/arbitrary', '{source}/arbitrary'])
        result = self.check(self.source('location', data))
        self.assertEqual(result.returncode, 1)
        self.assertIn('build[1]', result.stderr)

    def test_one_source_mode_does_not_accept_a_different_claimed_identity(self):
        result = self.check(self.source('test', recipe('monitor')))
        self.assertEqual(result.returncode, 1)
        self.assertIn('id', result.stderr)


if __name__ == '__main__':
    unittest.main(verbosity=2)
