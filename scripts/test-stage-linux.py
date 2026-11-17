#!/usr/bin/env python3
"""Exercise the real staging entry point against isolated release artifacts."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("stage-linux.py")
BINARIES = ("peersus", "peersus-ui", "peersus-wasm-host", "peersus-native-host", "peersus-store-fetch")
METADATA = (
    "packaging/linux/io.github.DarkPhilosophy.Peersus.desktop",
    "packaging/linux/io.github.DarkPhilosophy.Peersus.metainfo.xml",
    "crates/peersus-ui/assets/peersus-icon.svg",
    "LICENSE",
    "NOTICE",
)


class Staging(unittest.TestCase):
    def stage(self, target, triple=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "workspace"
            (root / "scripts").mkdir(parents=True)
            shutil.copyfile(SCRIPT, root / "scripts/stage-linux.py")
            shutil.copyfile(SCRIPT.with_name("build_layout.py"), root / "scripts/build_layout.py")
            build = root / "target" if target is None else Path(target)
            if not build.is_absolute():
                build = root / build
            release = build / triple / "release" if triple else build / "release"
            release.mkdir(parents=True)
            for name in (*BINARIES, "sample-extension"):
                (release / name).write_text("artifact:" + name)
                (release / name).chmod(0o755)
            for name in METADATA:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("metadata:" + name)
            plugin = root / "crates/peersus-sample"
            plugin.mkdir()
            (plugin / "plugin.json").write_text(json.dumps({"program": "sample-extension", "platforms": ["linux"]}))
            environment = dict(os.environ)
            environment.pop("CARGO_TARGET_DIR", None)
            environment.pop("CARGO_BUILD_TARGET", None)
            if triple:
                environment["CARGO_BUILD_TARGET"] = triple
            if target is not None:
                environment["CARGO_TARGET_DIR"] = str(target)
            result = subprocess.run([sys.executable, str(root / "scripts/stage-linux.py")], env=environment, cwd=temporary, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            output = build / "distribution/linux"
            for name in BINARIES:
                self.assertEqual((output / "bin" / name).read_text(), "artifact:" + name)
                self.assertTrue(os.access(output / "bin" / name, os.X_OK))
            self.assertEqual((output / "share/peersus/plugins/sample/sample-extension").read_text(), "artifact:sample-extension")
            self.assertEqual((output / "share/licenses/peersus/LICENSE").read_text(), "metadata:LICENSE")
            # Repeating staging must refuse to overwrite an existing distribution.
            repeated = subprocess.run([sys.executable, str(root / "scripts/stage-linux.py")], env=environment, cwd=temporary, capture_output=True, text=True)
            self.assertNotEqual(repeated.returncode, 0)
            self.assertEqual((output / "bin/peersus").read_text(), "artifact:peersus")

    def test_default_target(self):
        self.stage(None)

    def test_relative_target_is_workspace_relative(self):
        self.stage("custom-build")

    def test_absolute_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.stage(Path(temporary) / "external-build")

    def test_explicit_triple_never_selects_flat_directory_artifacts(self):
        self.stage("target/ui", "x86_64-unknown-linux-gnu")


if __name__ == "__main__":
    unittest.main()
