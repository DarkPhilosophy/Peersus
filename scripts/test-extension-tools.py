#!/usr/bin/env python3
"""Tests for the extension tools that run without Rust: the CI validator, the embed tool and the site's catalog staging.

Run by `cargo xtask metadata`. Uses the signed fixture the Rust tests also use, and builds a real compiled extension with the
system C compiler, so what is checked is the real input and not a description of it. Nothing leaves a temporary directory.
"""
import io
import importlib.util
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
    def build(self, directory: Path, edit=lambda manifest: None, *, process=False) -> Path:
        package = directory / "echo"
        package.mkdir()
        manifest = {
            "manifest_version": 1, "capabilities": {}, "imports": [], "exports": [], "program": "echo.so",
            "platforms": ["linux"],
            "execution": {"kind": "native_library", "abi": 1},
            "package": {"name": "Echo", "version": "1.0.0", "author": "Someone", "link": "https://example.org/echo", "description": "Echoes."},
            "agreements": {"pages": [
                {"kind": "licence", "title": "MIT", "explains": "You may use and share it.", "changes": "Nothing changes.",
                 "spdx": "MIT", "links": [{"label": "Licence", "url": "https://opensource.org/license/mit"}]},
                {"kind": "notice", "title": "What it does", "explains": "It echoes.", "changes": "Nothing changes."}]},
        }
        if process:
            manifest.update(program="echo", execution={"kind": "process"})
        (package / "plugin.json").write_text(json.dumps(manifest))
        header = run("python3", EMBED, "header", package / "plugin.json")
        self.assertEqual(header.returncode, 0, header.stderr)
        entry = "int main(void){return 73;}" if process else "int32_t peersus_plugin_main(const PeersusHostV1*h){(void)h;return 0;}"
        (directory / "e.c").write_text(
            '#include "peersus_plugin.h"\n' + header.stdout
            + "\nPEERSUS_METADATA_RECORD(PEERSUS_METADATA_BYTES);\n" + entry + "\n"
        )
        flags = ("-fPIE", "-pie") if process else ("-shared", "-fPIC")
        compiled = run("cc", *flags, "-O2", "-Wall", "-Wextra", "-Werror", "-I", SDK, directory / "e.c", "-o", package / manifest["program"])
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

    def test_a_real_process_is_admitted_without_executing_it_and_cannot_claim_wasm(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.build(Path(tmp), process=True)
            checked = run("python3", VALIDATE, archive)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            self.assertEqual(json.loads(checked.stdout)["execution"], {"kind": "process"})
            source = Path(tmp) / "echo"
            path = source / "plugin.json"
            manifest = json.loads(path.read_text())
            manifest["execution"] = {"kind": "wasm", "abi": 1}
            path.write_text(json.dumps(manifest))
            pack(source, archive, "echo")
            self.assertNotEqual(run("python3", VALIDATE, archive).returncode, 0)

    def test_a_process_without_an_explicit_executor_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.build(Path(tmp), lambda manifest: manifest.pop("execution"), process=True)
            result = run("python3", VALIDATE, archive)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("plugin.json must explicitly declare `execution`", result.stderr)

    def test_each_edit_the_manifest_cannot_back_up_with_the_program_is_refused(self):
        cases = {
            "author edited": lambda m: m["package"].update(author="Impostor"),
            "link edited": lambda m: m["package"].update(link="https://example.org/other"),
            "version edited": lambda m: m["package"].update(version="1.0.1"),
            "unknown execution kind": lambda m: m.update(execution={"kind": "process", "abi": 1}),
            "an ABI the host does not run": lambda m: m.update(execution={"kind": "native_library", "abi": 2}),
            "an ABI that is not a number": lambda m: m.update(execution={"kind": "wasm", "abi": "1"}),
            "an extra field in execution": lambda m: m.update(execution={"kind": "wasm", "abi": 1, "extra": True}),
            "insecure link": lambda m: m["package"].update(link="http://example.org/echo"),
            "link with credentials": lambda m: m["package"].update(link="https://u:p@example.org/echo"),
            "a self-assigned id": lambda m: m["package"].update(id="clipboard"),
            "no licence-and-notice agreement": lambda m: m.pop("agreements"),
            "a licence page without a notice page": lambda m: m["agreements"].update(
                pages=[p for p in m["agreements"]["pages"] if p["kind"] == "licence"]),
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

    def test_a_submission_without_its_webassembly_build_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            candidate = root / "candidate"
            candidate.mkdir()
            shutil.copyfile(self.build(root), candidate / "echo-linux-native_library.tar.gz")
            result = run("python3", ROOT / "scripts/validate-extension-build.py", "--directory", candidate,
                         "--id", "echo", "--commit", "a" * 40, "--sdk", "b" * 40, "--out", root / "review")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("WebAssembly build", result.stderr)
            self.assertFalse((root / "review").exists())

    def test_optional_native_artifacts_are_allowed_and_share_one_submission_contract(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            linux = self.build(root)
            candidate = root / "candidate"
            candidate.mkdir()
            library_archive = candidate / "echo-linux-native_library.tar.gz"
            source = root / "echo"
            manifest = json.loads((source / "plugin.json").read_text())
            manifest.update(platforms=["android", "linux", "macos", "windows"], program="echo.wasm", execution={"kind": "wasm", "abi": 1})
            (source / "plugin.json").write_text(json.dumps(manifest))
            module = root / "empty.wasm"
            module.write_bytes(b"\0asm\x01\0\0\0")
            embedded_module = subprocess.run(
                ["python3", str(EMBED), "wasm", str(source / "plugin.json"), str(module)], capture_output=True)
            self.assertEqual(embedded_module.returncode, 0, embedded_module.stderr)
            (source / "echo.so").unlink()
            (source / "echo.wasm").write_bytes(embedded_module.stdout)
            pack(source, candidate / "echo-android.tar.gz", "echo")
            command = ["python3", ROOT / "scripts/validate-extension-build.py",
                       "--directory", candidate, "--commit", "a" * 40, "--sdk", "b" * 40]

            def validate(output, plugin_id="echo"):
                return run(*command, "--id", plugin_id, "--out", root / output)

            def accepts(output, *kinds):
                result = validate(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                reviewed = json.loads((root / output / "review.json").read_text())
                self.assertCountEqual([package["execution"]["kind"] for package in reviewed["packages"]], kinds)
                return reviewed

            accepts("wasm-only", "wasm")
            shutil.copyfile(linux, library_archive)
            accepts("with-library", "wasm", "native_library")
            process_dir = root / "process"
            process_dir.mkdir()
            shutil.copyfile(self.build(process_dir, process=True), candidate / "echo-linux-process.tar.gz")
            library_archive.unlink()
            accepts("with-process", "wasm", "process")
            shutil.copyfile(linux, library_archive)
            # A Linux executable or library cannot stand in for the required Linux WASM build.
            platforms = manifest["platforms"]
            manifest["platforms"] = [platform for platform in platforms if platform != "linux"]
            (source / "plugin.json").write_text(json.dumps(manifest))
            pack(source, candidate / "echo-android.tar.gz", "echo")
            incomplete = validate("missing-linux-wasm")
            self.assertNotEqual(incomplete.returncode, 0)
            self.assertIn("missing: linux", incomplete.stderr)
            self.assertFalse((root / "missing-linux-wasm").exists())
            manifest["platforms"] = platforms
            manifest["capabilities"]["extra.read"] = "Read extra data"
            (source / "plugin.json").write_text(json.dumps(manifest))
            pack(source, candidate / "echo-android.tar.gz", "echo")
            mismatched = validate("review")
            self.assertNotEqual(mismatched.returncode, 0)
            self.assertIn("same extension contract", mismatched.stderr)
            self.assertFalse((root / "review").exists())
            del manifest["capabilities"]["extra.read"]
            (source / "plugin.json").write_text(json.dumps(manifest))
            pack(source, candidate / "echo-android.tar.gz", "echo")
            review = accepts("review", "wasm", "native_library", "process")
            self.assertEqual([p["id"] for p in review["packages"]], ["echo"] * 3)
            self.assertEqual({tuple(p["platforms"]) for p in review["packages"]},
                             {("linux",), ("android", "linux", "macos", "windows")})
            self.assertEqual(review["commit"], "a" * 40)
            # A platform archive filename is NOT an identity. The archive root is.
            self.assertNotEqual(validate("wrong-id", "upload").returncode, 0)
            self.assertFalse((root / "wrong-id").exists())
            shutil.copyfile(library_archive, candidate / "echo-linux-duplicate.tar.gz")
            self.assertNotEqual(validate("ambiguous").returncode, 0)
            self.assertFalse((root / "ambiguous").exists())


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
                "agreements": {"pages": [
                    {"kind": "licence", "title": "MIT", "explains": "Use it.", "changes": "Nothing.",
                     "spdx": "MIT", "links": [{"label": "Licence", "url": "https://opensource.org/license/mit"}]},
                    {"kind": "notice", "title": "What it does", "explains": "It uploads.", "changes": "Nothing."}]},
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
    """What the website stages of a signed catalog with its archives: the development fixture through `--test-catalog`, and
    the same checks (`validate_catalog`, the application's own rules) on copies of it that the application would refuse."""

    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("site_build", SITE)
        cls.build = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.build)

    def stage(self, *flags):
        out = Path(tempfile.mkdtemp()) / "out"
        result = run("python3", SITE, "--output", out, *flags)
        shutil.rmtree(out.parent, ignore_errors=True)
        return result

    def accepted(self, directory):
        try:
            self.build.validate_catalog(directory)
        except (ValueError, KeyError, TypeError, OSError):
            return False
        return True

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

    def test_the_site_stages_without_a_catalog(self):
        result = self.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("0 test store files, 0 approvals", result.stdout)

    def test_the_real_signed_fixture_is_staged_with_its_archive(self):
        result = self.stage("--test-catalog")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("3 test store files", result.stdout)

    def test_platform_catalog_names_every_artifact_once_and_refuses_ambiguity(self):
        directory = self.copy_fixture()
        path = directory / "catalog.json"
        catalog = json.loads(path.read_text())
        catalog["schema"] = 2
        entry = catalog["entries"][0]
        artifact = {key: entry.pop(key) for key in ("archive", "sha256", "size")}
        entry["artifacts"] = [
            {**artifact, "platform": "linux", "architecture": "x86_64"},
            {**artifact, "platform": "android", "architecture": "any"},
        ]
        path.write_text(json.dumps(catalog))
        self.assertEqual(self.build.check_document(catalog, directory), ["echo.tar.gz"])
        entry["artifacts"].append({**artifact, "platform": "android", "architecture": "aarch64"})
        self.assertEqual(self.build.check_document(catalog, directory), ["echo.tar.gz"])
        entry["artifacts"].append({**artifact, "platform": "android", "architecture": "aarch64"})
        with self.assertRaises(ValueError):
            self.build.check_document(catalog, directory)
        entry["artifacts"].pop()
        entry["artifacts"].pop()
        entry["artifacts"][1]["sha256"] = "0" * 64
        with self.assertRaises(ValueError):
            self.build.check_document(catalog, directory)

    def test_every_tampering_the_application_would_refuse_is_refused_before_publishing(self):
        self.assertTrue(self.accepted(self.copy_fixture()), "the untouched fixture is accepted")
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
                self.assertFalse(self.accepted(directory), f"{what} was published")

if __name__ == "__main__":
    unittest.main(verbosity=2)
