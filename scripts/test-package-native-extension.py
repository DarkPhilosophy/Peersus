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

# The licence-and-notice agreement every packaged manifest must carry; the validator's
# pre-screen requires at least one licence page and one notice page.
AGREEMENT = {"pages": [
    {"kind": "licence", "title": "MIT", "explains": "You may use and share it.",
     "changes": "Nothing changes.", "spdx": "MIT",
     "links": [{"label": "Licence", "url": "https://opensource.org/license/mit"}]},
    {"kind": "notice", "title": "What it does", "explains": "A packaging test extension.",
     "changes": "Nothing changes."}]}



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
                "agreements": AGREEMENT,
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
            archive = output / "upload-linux-process.tar.gz"
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
            source = root / "plugin/peersus-upload"
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


class GuestPackage(unittest.TestCase):
    """A WebAssembly guest, packaged by the same script, under an id the recipe states and not one guessed from a path."""

    @staticmethod
    def guest(root, name="notes-guest", platforms=("android",), program_bytes=b"\0asm\x01\0\0\0"):
        source = root / name
        source.mkdir()
        manifest = {
            "manifest_version": 1, "execution": {"kind": "wasm", "abi": 1},
            "program": "guest.wasm", "platforms": list(platforms), "capabilities": {},
            "imports": [], "exports": [],
            "package": {"name": "Notes", "version": "0.1.0", "author": "Test contributors",
                        "link": "https://example.org/plugin", "description": "Test guest."},
            "agreements": AGREEMENT,
        }
        (source / "plugin.json").write_text(json.dumps(manifest))
        built = root / "target/wasm32-unknown-unknown/release"
        built.mkdir(parents=True)
        (built / "guest.wasm").write_bytes(program_bytes)
        licenses = root / "licenses"
        licenses.mkdir()
        (licenses / "THIRD_PARTY_NOTICES.txt").write_text("Fixture contains only a test module.\n")
        return source, root / "target", licenses, manifest

    def run_packager(self, root, source, target, licenses, *extra, platform="android"):
        output = root / "out"
        output.mkdir(exist_ok=True)
        return output, subprocess.run(
            ["python3", str(ROOT / "scripts/package-native-extension.py"), "--source", str(source),
             "--sdk", str(ROOT), "--target-dir", str(target), "--out", str(output), "--platform", platform,
             "--licenses", str(licenses), *extra], capture_output=True, text=True)

    def test_the_same_guest_built_natively_is_packaged_as_a_native_library_beside_it(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, _ = self.guest(root, platforms=("linux", "android"))
            native = root / "native"
            native.mkdir()
            (root / "lib.c").write_text("int peersus_plugin_main(const void *host){return host ? 0 : 1;}\n")
            subprocess.run(["cc", "-shared", "-fPIC", str(root / "lib.c"), "-o", str(native / "libnotes.so")],
                           check=True)
            output, result = self.run_packager(root, source, native, licenses, "--id", "notes",
                                               "--native-library", "libnotes.so", platform="linux")
            self.assertEqual(result.returncode, 0, result.stderr)
            archive = output / "notes-linux-native_library.tar.gz"
            checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"), str(archive)],
                                     capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            report = json.loads(checked.stdout)
            self.assertEqual((report["execution"], report["platforms"]),
                             ({"kind": "native_library", "abi": 1}, ["linux"]))
            # The same guest linked into a program: the third tier, beside the other two.
            (root / "main.c").write_text("int main(void){return 0;}\n")
            subprocess.run(["cc", "-fPIE", "-pie", str(root / "main.c"), "-o", str(native / "notes-process")],
                           check=True)
            _, linked = self.run_packager(root, source, native, licenses, "--id", "notes",
                                          "--process", "notes-process", platform="linux")
            self.assertEqual(linked.returncode, 0, linked.stderr)
            checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"),
                                      str(output / "notes-linux-process.tar.gz")], capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            self.assertEqual(json.loads(checked.stdout)["execution"], {"kind": "process"})
            # Not a shared object: refused, nothing written.
            (native / "fake.so").write_bytes(b"\0asm\x01\0\0\0")
            _, refused = self.run_packager(root, source, native, licenses, "--id", "other",
                                           "--native-library", "fake.so", platform="linux")
            self.assertNotEqual(refused.returncode, 0)
            self.assertFalse((output / "other-linux-native_library.tar.gz").exists())

    def test_a_recipe_cannot_fill_in_a_missing_source_executor(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, manifest = self.guest(root, platforms=("linux",))
            manifest.pop("execution")
            (source / "plugin.json").write_text(json.dumps(manifest))
            for variant in [(), ("--native-library", "libnotes.so"), ("--process", "notes-process")]:
                with self.subTest(variant):
                    output, result = self.run_packager(root, source, target, licenses, *variant, platform="linux")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("plugin.json must explicitly declare `execution`", result.stderr)
                    self.assertEqual(list(output.iterdir()), [])

    def test_a_guest_is_packaged_under_the_stated_id_and_passes_the_independent_validator(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, manifest = self.guest(root)
            output, result = self.run_packager(root, source, target, licenses, "--id", "notes")
            self.assertEqual(result.returncode, 0, result.stderr)
            archive = output / "notes-android.tar.gz"
            checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"), str(archive)],
                                     capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            report = json.loads(checked.stdout)
            # The id is what the recipe said. The directory the source sat in is named differently, on purpose.
            self.assertEqual((report["id"], report["execution"], report["platforms"], report["architecture"]),
                             ("notes", {"kind": "wasm", "abi": 1}, ["android"], "any"))
            with tarfile.open(archive) as contents:
                names = contents.getnames()
                self.assertIn("notes/guest.wasm", names)
                self.assertIn("notes/LICENSE", names)
                self.assertEqual(json.load(contents.extractfile("notes/plugin.json"))["package"], manifest["package"])
                module = contents.extractfile("notes/guest.wasm").read()
            # The record travelled inside the module, as the validator demands, and the module is still one.
            self.assertTrue(module.startswith(b"\0asm\x01\0\0\0"))
            self.assertIn(b"peersus.meta", module)

    def test_platform_scoped_settings_keep_one_contract_across_required_guest_archives(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            platforms = ("linux", "android", "macos")
            source, target, licenses, manifest = self.guest(root, platforms=platforms)
            settings = [
                {"id": "enabled", "label": "Enabled", "kind": {"type": "toggle", "default": False}},
                {"id": "mobile", "label": "Mobile only", "kind": {"type": "toggle", "default": False},
                 "platforms": ["android"]},
            ]
            manifest["contributions"] = {"settings": settings}
            (source / "plugin.json").write_text(json.dumps(manifest))
            digests = set()
            for platform in platforms:
                output, result = self.run_packager(root, source, target, licenses, "--id", "notes", platform=platform)
                self.assertEqual(result.returncode, 0, result.stderr)
                archive = output / f"notes-{platform}.tar.gz"
                checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"),
                                          str(archive), "--contract-digest"], capture_output=True, text=True)
                self.assertEqual(checked.returncode, 0, checked.stderr)
                digests.add(json.loads(checked.stdout)["contract_sha256"])
            self.assertEqual(len(digests), 1, "admission requires the same extension contract across all platforms")
            for platform in platforms:
                with tarfile.open(output / f"notes-{platform}.tar.gz") as contents:
                    packed = json.load(contents.extractfile("notes/plugin.json"))
                    self.assertEqual(packed["contributions"]["settings"], settings)

    def test_the_guests_language_files_travel_in_the_package_and_are_listed_as_its_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, _ = self.guest(root)
            (source / "languages").mkdir()
            ro = 'msgid ""\nmsgstr "Language: ro\\n"\n\nmsgid "Share"\nmsgstr "Partajează"\n'
            (source / "languages/ro.po").write_text(ro)
            output, result = self.run_packager(root, source, target, licenses, "--id", "notes")
            self.assertEqual(result.returncode, 0, result.stderr)
            archive = output / "notes-android.tar.gz"
            checked = subprocess.run(["python3", str(ROOT / "scripts/validate-extension.py"), str(archive)],
                                     capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            with tarfile.open(archive) as contents:
                self.assertEqual(contents.extractfile("notes/languages/ro.po").read().decode(), ro)
                # Installed only because the manifest lists it: an unlisted file never reaches a device.
                self.assertIn("languages/ro.po", json.load(contents.extractfile("notes/plugin.json"))["files"])
            # Anything else in that folder is refused, not silently shipped or dropped.
            (source / "languages/notes.txt").write_text("not a language file")
            _, refused = self.run_packager(root, source, target, licenses, "--id", "other")
            self.assertNotEqual(refused.returncode, 0)
            self.assertFalse((output / "other-android.tar.gz").exists())

    def test_the_same_guest_is_byte_for_byte_the_same_archive_every_time(self):
        digests = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                source, target, licenses, _ = self.guest(root)
                output, result = self.run_packager(root, source, target, licenses, "--id", "notes")
                self.assertEqual(result.returncode, 0, result.stderr)
                digests.append((output / "notes-android.tar.gz").read_bytes())
        self.assertEqual(digests[0], digests[1], "an archive that differs between runs cannot be the one that was approved")

    def test_an_id_outside_the_identifier_grammar_is_refused_not_written(self):
        for bad in ("", ".hidden", "a b", "a/b", "é", "x" * 129):
            with tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                source, target, licenses, _ = self.guest(root)
                output, result = self.run_packager(root, source, target, licenses, "--id", bad)
                self.assertNotEqual(result.returncode, 0, repr(bad))
                self.assertEqual(list(output.iterdir()), [], f"an archive was written for the id {bad!r}")

    def test_a_platform_the_manifest_does_not_declare_is_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, _ = self.guest(root, platforms=("android",))
            output, result = self.run_packager(root, source, target, licenses, "--id", "notes", platform="linux")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(output.iterdir()), [])

    def test_a_program_that_is_not_a_webassembly_module_is_refused_before_anything_is_written(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, _ = self.guest(root, program_bytes=b"\x7fELF not a module")
            output, result = self.run_packager(root, source, target, licenses, "--id", "notes")
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(output.iterdir()), [])

    def test_a_missing_program_is_refused_and_an_existing_archive_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, target, licenses, _ = self.guest(root)
            output, first = self.run_packager(root, source, target, licenses, "--id", "notes")
            self.assertEqual(first.returncode, 0, first.stderr)
            archive = output / "notes-android.tar.gz"
            before = archive.read_bytes()
            _, again = self.run_packager(root, source, target, licenses, "--id", "notes")
            self.assertNotEqual(again.returncode, 0)
            self.assertEqual(archive.read_bytes(), before)
            (target / "wasm32-unknown-unknown/release/guest.wasm").unlink()
            other = root / "second"
            other.mkdir()
            missing = subprocess.run(
                ["python3", str(ROOT / "scripts/package-native-extension.py"), "--source", str(source), "--sdk", str(ROOT),
                 "--target-dir", str(target), "--out", str(other), "--platform", "android", "--id", "notes",
                 "--licenses", str(licenses)], capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertEqual(list(other.iterdir()), [])

    def test_no_plugin_is_named_anywhere_in_the_packager(self):
        # A packager that lists the plugins it will package is a packager nobody else can use. Any spelling, anywhere: a
        # comment or a variable that names one is the first step of a list.
        text = (ROOT / "scripts/package-native-extension.py").read_text().lower()
        for named in ("clipboard", "upload"):
            self.assertNotIn(named, text, f"the packager mentions a first-party plugin: {named}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
