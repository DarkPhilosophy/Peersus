#!/usr/bin/env python3
"""Exercise native packaging with a real executable, without launching a plugin."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class NativePackage(unittest.TestCase):
    def test_real_process_package_keeps_identity_companion_and_executable_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "peersus-upload"
            source.mkdir()
            manifest = {
                "manifest_version": 1, "execution": {"kind": "process"},
                "program": "peersus-upload", "files": ["peersus-upload-credential"],
                "platforms": ["linux", "macos"], "capabilities": {}, "imports": [], "exports": [],
                "package": {"name": "Upload", "version": "0.1.0", "author": "Test contributors",
                            "link": "https://example.org/plugin", "description": "Test process."},
            }
            (source / "plugin.json").write_text(json.dumps(manifest))
            binary = root / "main.c"
            binary.write_text("int main(void){return 73;}\n")
            target = root / "release"
            target.mkdir()
            for name in ("peersus-upload", "peersus-upload-credential"):
                subprocess.run(["cc", "-fPIE", "-pie", str(binary), "-o", str(target / name)], check=True)
            output = root / "out"
            output.mkdir()
            command = ["python3", str(ROOT / "scripts/package-native-extension.py"),
                       "--source", str(source), "--sdk", str(ROOT), "--target-dir", str(target),
                       "--out", str(output), "--platform", "linux", "--licenses", str(root / "licenses")]
            licenses = root / "licenses"
            licenses.mkdir()
            (licenses / "THIRD_PARTY_NOTICES.txt").write_text("Fixture contains only the test C program.\n")
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            archive = output / "upload-linux.tar.gz"
            checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"), str(archive)],
                                     capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            report = json.loads(checked.stdout)
            self.assertEqual((report["id"], report["execution"], report["platforms"]),
                             ("upload", {"kind": "process"}, ["linux"]))
            with tarfile.open(archive) as contents:
                names = contents.getnames()
                self.assertIn("upload/peersus-upload-credential", names)
                self.assertIn("upload/LICENSE", names)
                for name in ("upload/peersus-upload", "upload/peersus-upload-credential"):
                    self.assertTrue(contents.getmember(name).mode & 0o100)
                packed = json.load(contents.extractfile("upload/plugin.json"))
                self.assertEqual(packed["package"], manifest["package"])
                self.assertEqual(packed["platforms"], ["linux"])
            before = archive.read_bytes()
            repeated = subprocess.run(command, capture_output=True)
            self.assertNotEqual(repeated.returncode, 0)
            self.assertEqual(archive.read_bytes(), before)

    def test_unrelated_workspace_features_do_not_enter_the_shipped_license_set(self):
        spec = importlib.util.spec_from_file_location("native_package", ROOT / "scripts/package-native-extension.py")
        native = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(native)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "crates/peersus-upload"
            linked = root / "linked"
            linked.mkdir()
            (linked / "LICENSE").write_text("TEST LICENSE FIXTURE\n")
            metadata = {"packages": [
                {"id": "root", "name": "peersus-upload", "version": "0.1.0",
                 "manifest_path": str(source / "Cargo.toml"), "source": None},
                {"id": "linked", "name": "linked", "version": "1.0.0",
                 "manifest_path": str(linked / "Cargo.toml"), "source": "registry", "license": "MIT"},
                {"id": "unrelated", "name": "unrelated-codec", "version": "2.0.0",
                 "manifest_path": str(root / "unrelated/Cargo.toml"), "source": "registry", "license": None},
            ]}
            responses = ["host: x86_64-unknown-linux-gnu\n", json.dumps(metadata).encode(),
                         "peersus-upload v0.1.0\nlinked v1.0.0\n"]
            with patch.object(native.subprocess, "check_output", side_effect=responses):
                notices = native.dependency_licenses(root, source)
            self.assertEqual(notices["licenses/linked-1.0.0/LICENSE"], b"TEST LICENSE FIXTURE\n")
            self.assertNotIn(b"unrelated-codec", notices["THIRD_PARTY_NOTICES.txt"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
