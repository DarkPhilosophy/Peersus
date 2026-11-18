#!/usr/bin/env python3
"""Exercise the real macOS staging entry point against isolated artifacts, with stand-ins for macOS's own tools."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("stage-macos.py")
PROGRAMS = ("peersus-ui", "peersus", "peersus-wasm-host", "peersus-store-fetch", "peersus-native-host")
# Each stand-in does the one thing the script relies on: `sips` and `iconutil` write the file they were asked for, and
# `codesign` leaves a note of what it was asked to seal.
TOOLS = {
    "sips": 'while [ "$1" != "--out" ]; do last="$1"; shift; done\ncp "$last" "$2"\n',
    "iconutil": 'while [ "$1" != "--output" ]; do shift; done\nls "$3" | sort > "$2"\n',
    "codesign": 'for last; do :; done\nprintf "%s\\n" "$*" > "$last/Contents/sealed"\n',
}


class Staging(unittest.TestCase):
    def workspace(self, temporary, programs=PROGRAMS, profile="release"):
        # Resolved, as the script resolves its own place: on macOS the temporary directory is reached through a link.
        root = Path(temporary).resolve() / "workspace"
        (root / "scripts").mkdir(parents=True)
        shutil.copyfile(SCRIPT, root / "scripts/stage-macos.py")
        shutil.copyfile(SCRIPT.with_name("build_layout.py"), root / "scripts/build_layout.py")
        built = root / "target" / profile
        built.mkdir(parents=True)
        for name in programs:
            (built / name).write_text("artifact:" + name)
            (built / name).chmod(0o755)
        for name in ("LICENSE", "NOTICE", "crates/peersus-ui/assets/peersus-icon.png"):
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("metadata:" + name)
        (root / "Cargo.toml").write_text('[workspace]\nmembers = []\n\n[workspace.package]\nversion = "7.8.9"\nedition = "2024"\n\n[workspace.dependencies]\nversion = "1"\n')
        tools = Path(temporary) / "tools"
        tools.mkdir()
        for name, body in TOOLS.items():
            (tools / name).write_text("#!/bin/sh\n" + body)
            (tools / name).chmod(0o755)
        return root, tools

    def stage(self, root, tools, *arguments, without=()):
        environment = dict(os.environ)
        for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "PEERSUS_BUILD_NUMBER"):
            environment.pop(name, None)
        for name in without:
            (tools / name).unlink()
        # Only the stand-ins and a shell: a real `codesign` of the machine that runs the test must not be found.
        environment["PATH"] = f"{tools}:/bin:/usr/bin" if not without else str(tools)
        return subprocess.run(
            [sys.executable, str(root / "scripts/stage-macos.py"), *arguments],
            env=environment, cwd=root.parent, capture_output=True, text=True,
        )

    def test_the_bundle_is_one_application_with_its_programs_side_by_side(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.workspace(temporary)
            result = self.stage(root, tools)
            self.assertEqual(result.returncode, 0, result.stderr)
            bundle = root / "target/distribution/macos/Peersus.app"
            self.assertEqual(result.stdout.strip(), str(bundle))
            for name in PROGRAMS:
                program = bundle / "Contents/MacOS" / name
                self.assertEqual(program.read_text(), "artifact:" + name)
                self.assertTrue(os.access(program, os.X_OK))
            with open(bundle / "Contents/Info.plist", "rb") as file:
                information = plistlib.load(file)
            self.assertEqual(information["CFBundleExecutable"], "peersus-ui")
            self.assertEqual(information["CFBundleIdentifier"], "io.github.DarkPhilosophy.Peersus")
            self.assertEqual(information["CFBundleName"], "Peersus")
            self.assertEqual(information["CFBundleShortVersionString"], "7.8.9")
            self.assertEqual(information["CFBundleVersion"], "1")
            self.assertNotIn("LSEnvironment", information)
            # The icon the plist names is there, made of the nine sizes and nothing enlarged past the source.
            sizes = (bundle / "Contents/Resources" / (information["CFBundleIconFile"] + ".icns")).read_text().split()
            self.assertEqual(len(sizes), 9)
            self.assertNotIn("icon_512x512@2x.png", sizes)
            self.assertEqual((bundle / "Contents/Resources/LICENSE").read_text(), "metadata:LICENSE")
            # Sealed last, ad hoc, as one bundle.
            self.assertEqual((bundle / "Contents/sealed").read_text().strip(), f"--force --deep --sign - {bundle}")
            # A second staging refuses to write over the first.
            self.assertNotEqual(self.stage(root, tools).returncode, 0)
            self.assertEqual((bundle / "Contents/MacOS/peersus").read_text(), "artifact:peersus")

    def test_a_variable_is_carried_only_when_asked_for(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.workspace(temporary, profile="debug")
            result = self.stage(root, tools, "--profile", "debug", "--environment", "FREYA_RENDERER=software")
            self.assertEqual(result.returncode, 0, result.stderr)
            with open(root / "target/distribution/macos/Peersus.app/Contents/Info.plist", "rb") as file:
                self.assertEqual(plistlib.load(file)["LSEnvironment"], {"FREYA_RENDERER": "software"})

    def test_what_is_not_a_variable_is_refused(self):
        for pair in ("FREYA_RENDERER", "=software", "A B=1"):
            with tempfile.TemporaryDirectory() as temporary:
                root, tools = self.workspace(temporary)
                result = self.stage(root, tools, "--environment", pair)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / "target/distribution").exists())

    def test_a_program_that_was_not_built_is_named_and_nothing_is_staged(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.workspace(temporary, programs=("peersus-ui", "peersus", "peersus-wasm-host", "peersus-native-host"))
            result = self.stage(root, tools)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("peersus-store-fetch", result.stderr)
            self.assertFalse((root / "target/distribution").exists())

    def test_without_the_tools_of_macos_nothing_is_staged(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.workspace(temporary)
            result = self.stage(root, tools, without=("iconutil",))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("iconutil", result.stderr)
            self.assertFalse((root / "target/distribution").exists())


if __name__ == "__main__":
    unittest.main()
