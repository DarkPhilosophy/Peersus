#!/usr/bin/env python3
"""Tests for the extension tools that run without Rust: the CI validator, the embed tool and the site's catalog staging.

Run by `cargo xtask metadata`. Uses the signed fixture the Rust tests also use, and builds a real compiled extension with the
system C compiler, so what is checked is the real input and not a description of it. Nothing leaves a temporary directory.
"""
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "crates/peersus-app/tests/fixtures/catalog"
VALIDATE = ROOT / "scripts/validate-extension.py"
EMBED = ROOT / "scripts/embed-metadata.py"
SITE = ROOT / "site/build.py"
SDK = ROOT / "crates/peersus-plugin-runtime/sdk"


def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], capture_output=True, text=True, **kwargs)


def pack(source: Path, destination: Path, name: str):
    with tarfile.open(destination, "w:gz") as archive:
        archive.add(source, arcname=name)


class ValidatorAndEmbed(unittest.TestCase):
    def build(self, directory: Path, edit=lambda manifest: None) -> Path:
        package = directory / "echo"
        package.mkdir()
        manifest = {
            "manifest_version": 1, "capabilities": {}, "imports": [], "exports": [], "program": "echo.so",
            "execution": {"kind": "native_library", "abi": 1},
            "package": {"name": "Echo", "version": "1.0.0", "author": "Someone", "link": "https://example.org/echo", "description": "Echoes."},
        }
        (package / "plugin.json").write_text(json.dumps(manifest))
        header = run("python3", EMBED, "header", package / "plugin.json")
        self.assertEqual(header.returncode, 0, header.stderr)
        (directory / "e.c").write_text(
            '#include "peersus_plugin.h"\n' + header.stdout
            + "\nPEERSUS_METADATA_RECORD(PEERSUS_METADATA_BYTES);\nint32_t peersus_plugin_main(const PeersusHostV1*h){(void)h;return 0;}\n"
        )
        compiled = run("cc", "-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror", "-I", SDK, directory / "e.c", "-o", package / "echo.so")
        self.assertEqual(compiled.returncode, 0, compiled.stderr)
        # The manifest is edited AFTER the program was built from it: the compiled-in record stays as it was.
        edited = json.loads((package / "plugin.json").read_text())
        edit(edited)
        (package / "plugin.json").write_text(json.dumps(edited))
        archive = directory / "echo.tar.gz"
        pack(package, archive, "echo")
        return archive

    def test_a_real_compiled_extension_passes_and_reports_the_digest_the_maintainer_approves(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.build(Path(tmp))
            result = run("python3", VALIDATE, archive)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual((report["id"], report["version"], report["author"]), ("echo", "1.0.0", "Someone"))
            self.assertEqual(len(report["archive_sha256"]), 64)

    def test_each_edit_the_manifest_cannot_back_up_with_the_program_is_refused(self):
        cases = {
            "author edited": lambda m: m["package"].update(author="Impostor"),
            "link edited": lambda m: m["package"].update(link="https://example.org/other"),
            "version edited": lambda m: m["package"].update(version="1.0.1"),
            "unknown execution kind": lambda m: m.update(execution={"kind": "process", "abi": 1}),
            "an ABI the host does not run": lambda m: m.update(execution={"kind": "native_library", "abi": 2}),
            "an ABI that is not a number": lambda m: m.update(execution={"kind": "wasm", "abi": "1"}),
            "an extra field in execution": lambda m: m.update(execution={"kind": "wasm", "abi": 1, "extra": True}),
            "legacy process package": lambda m: m.pop("execution"),
            "insecure link": lambda m: m["package"].update(link="http://example.org/echo"),
            "link with credentials": lambda m: m["package"].update(link="https://u:p@example.org/echo"),
            "a self-assigned id": lambda m: m["package"].update(id="clipboard"),
        }
        for what, edit in cases.items():
            with self.subTest(what), tempfile.TemporaryDirectory() as tmp:
                archive = self.build(Path(tmp), edit)
                result = run("python3", VALIDATE, archive)
                self.assertNotEqual(result.returncode, 0, f"{what} was accepted")
                self.assertTrue(result.stderr.startswith("refused:"), result.stderr)

    def test_an_archive_that_escapes_its_folder_or_holds_a_link_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            good = self.build(tmp)
            for what in ("parent path", "symbolic link"):
                with self.subTest(what):
                    bad = tmp / f"{what.replace(' ', '-')}.tar.gz"
                    with tarfile.open(good) as source, tarfile.open(bad, "w:gz") as out:
                        for member in source:
                            out.addfile(member, source.extractfile(member) if member.isfile() else None)
                        if what == "parent path":
                            escaped = tarfile.TarInfo("echo/../evil")
                            escaped.size = 1
                            out.addfile(escaped, io.BytesIO(b"x"))
                        else:
                            link = tarfile.TarInfo("echo/link")
                            link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
                            out.addfile(link)
                    self.assertNotEqual(run("python3", VALIDATE, bad).returncode, 0, f"{what} was accepted")


class WasmMetadata(unittest.TestCase):
    def test_module_metadata_is_accepted_and_manifest_tampering_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            package = directory / "upload"
            package.mkdir()
            manifest = {
                "manifest_version": 1, "program": "upload.wasm",
                "execution": {"kind": "wasm", "abi": 1},
                "package": {"name": "Upload", "version": "1.0.0",
                            "author": "Contributors", "link": "https://example.org/upload"},
            }
            manifest_path = package / "plugin.json"
            manifest_path.write_text(json.dumps(manifest))
            # A valid empty module is enough: the validator must never execute it.
            module = directory / "module.wasm"
            module.write_bytes(b"\0asm\x01\0\0\0")
            result = subprocess.run(
                ["python3", str(EMBED), "wasm", str(manifest_path), str(module)],
                capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            (package / "upload.wasm").write_bytes(result.stdout)
            self.assertEqual(run("python3", VALIDATE, package).returncode, 0)
            repeated = run("python3", EMBED, "wasm", manifest_path, package / "upload.wasm")
            self.assertNotEqual(repeated.returncode, 0, "duplicate metadata must be refused")
            manifest["package"]["version"] = "1.0.1"
            manifest_path.write_text(json.dumps(manifest))
            self.assertNotEqual(run("python3", VALIDATE, package).returncode, 0)

    def test_invalid_modules_are_refused_before_emitting_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            manifest = directory / "plugin.json"
            manifest.write_text(json.dumps({
                "manifest_version": 1, "program": "bad.wasm",
                "execution": {"kind": "wasm", "abi": 1},
                "package": {"name": "Upload", "version": "1.0.0",
                            "author": "Contributors", "link": "https://example.org/upload"},
            }))
            module = directory / "bad.wasm"
            for content in (b"not wasm", b"\0asm\x01\0\0\0\0\x80", b"\0asm\x01\0\0\0\0\x05x"):
                with self.subTest(content=content):
                    module.write_bytes(content)
                    result = run("python3", EMBED, "wasm", manifest, module)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    checked = run("python3", VALIDATE, directory)
                    self.assertNotEqual(checked.returncode, 0)
                    self.assertTrue(checked.stderr.startswith("refused:"), checked.stderr)

class SiteCatalog(unittest.TestCase):
    def stage(self, catalog=None):
        out = Path(tempfile.mkdtemp()) / "out"
        args = ["python3", SITE, "--output", out] + (["--catalog", catalog] if catalog else [])
        result = run(*args)
        shutil.rmtree(out.parent, ignore_errors=True)
        return result

    def copy_fixture(self):
        directory = Path(tempfile.mkdtemp()) / "catalog"
        shutil.copytree(FIXTURE, directory)
        (directory / "README.txt").unlink()
        self.addCleanup(shutil.rmtree, directory.parent, ignore_errors=True)
        return directory

    def edit(self, directory, change):
        path = directory / "catalog.json"
        catalog = json.loads(path.read_text())
        change(catalog["entries"][0])
        path.write_text(json.dumps(catalog, separators=(",", ":"), sort_keys=True))

    def test_the_site_stages_without_a_catalog_exactly_as_before(self):
        result = self.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("and 0 store files", result.stdout)

    def test_the_real_signed_fixture_is_staged_with_its_archive(self):
        result = self.stage(self.copy_fixture())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("and 3 store files", result.stdout)

    def test_every_tampering_the_application_would_refuse_is_refused_before_publishing(self):
        def flip(directory):
            data = (directory / "echo.tar.gz").read_bytes()
            (directory / "echo.tar.gz").write_bytes(bytes([data[0] ^ 1]) + data[1:])
        cases = {
            "archive changed": flip,
            "archive missing": lambda d: (d / "echo.tar.gz").unlink(),
            "insecure link": lambda d: self.edit(d, lambda e: e.update(link="http://example.org/x")),
            "link with credentials": lambda d: self.edit(d, lambda e: e.update(link="https://u:p@example.org/x")),
            "path in the archive name": lambda d: self.edit(d, lambda e: e.update(archive="../x.tar.gz")),
            "control character in the author": lambda d: self.edit(d, lambda e: e.update(author="a\u202eb")),
            "unknown field": lambda d: self.edit(d, lambda e: e.update(extra=1)),
            "short signature": lambda d: (d / "catalog.sig").write_bytes(b"x" * 10),
            "bad version": lambda d: self.edit(d, lambda e: e.update(version="1.0")),
            "uppercase digest": lambda d: self.edit(d, lambda e: e.update(sha256=e["sha256"].upper())),
        }
        for what, change in cases.items():
            with self.subTest(what):
                directory = self.copy_fixture()
                change(directory)
                result = self.stage(directory)
                self.assertNotEqual(result.returncode, 0, f"{what} was published")


if __name__ == "__main__":
    unittest.main(verbosity=2)
