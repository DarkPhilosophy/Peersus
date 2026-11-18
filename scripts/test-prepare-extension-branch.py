#!/usr/bin/env python3
"""Check the actual lean-tree exporter, including fail-closed filesystem boundaries."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("prepare_extension", Path(__file__).with_name("prepare-extension-branch.py"))
EXPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORT)


class ExtensionTree(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "app"
        self.repo.mkdir()
        subprocess.run(["git", "init", "--quiet", str(self.repo)], check=True)
        self.put("scripts/extension-branch/README.md", "Build a plugin; submit its source.\n")
        self.put(".github/PULL_REQUEST_TEMPLATE/extension.md", "Describe the extension's purpose and data access.\n")
        self.put(".github/workflows/extension-validate.yml", "name: Source submission\n")
        self.put(".github/workflows/triage.yml", "name: Metadata triage\n")
        self.put("LICENSE", "Fixture license\n")
        self.put("NOTICE", "Fixture notice\n")
        for plugin in EXPORT.PLUGINS:
            self.put(f"plugins/{plugin}/Cargo.toml", f'[package]\nname = "{plugin}"\n')
            self.put(f"plugins/{plugin}/plugin.json", '{"id":"fixture"}\n')
            self.put(f"plugins/{plugin}/submission.toml", 'schema = 1\n')
            self.put(f"plugins/{plugin}/ownership.json", '{"schema":1,"owner":{"login":"fixture","id":1},"contributors":[]}\n')
            self.put(f"plugins/{plugin}/src/lib.rs", "pub fn original_behavior() {}\n")
        self.put("plugins/peersus-upload/wasm/src/lib.rs", "pub fn mobile_behavior() {}\n")
        self.put("Cargo.toml", "This application workspace must not be exported.\n")
        self.put("site/index.html", "Not part of the extension branch.\n")
        self.out = self.root / "extension"

    def put(self, name, value):
        file = self.repo / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(value)
        return file

    def test_exports_real_sources_one_upload_directory_and_dedicated_ci(self):
        EXPORT.prepare(self.repo, self.out)
        self.assertEqual({p.name for p in self.out.iterdir()}, {"README.md", ".github", "extension", "LICENSE", "NOTICE"})
        self.assertEqual({p.name for p in (self.out / "extension").iterdir()}, set(EXPORT.PLUGINS))
        for plugin in EXPORT.PLUGINS:
            self.assertEqual((self.out / f"extension/{plugin}/src/lib.rs").read_bytes(),
                             (self.repo / f"plugins/{plugin}/src/lib.rs").read_bytes())
        self.assertEqual((self.out / "extension/peersus-upload/wasm/src/lib.rs").read_text(),
                         "pub fn mobile_behavior() {}\n")
        self.assertEqual((self.out / "README.md").read_bytes(),
                         (self.repo / "scripts/extension-branch/README.md").read_bytes())
        self.assertEqual((self.out / ".github/workflows/extension-validate.yml").read_bytes(),
                         (self.repo / ".github/workflows/extension-validate.yml").read_bytes())
        self.assertEqual((self.out / ".github/PULL_REQUEST_TEMPLATE.md").read_bytes(),
                         (self.repo / ".github/PULL_REQUEST_TEMPLATE/extension.md").read_bytes())
        self.assertFalse((self.out / ".git").exists())
        self.assertFalse((self.out / "site").exists())

    def test_existing_output_and_in_checkout_output_are_never_overwritten(self):
        self.out.mkdir()
        marker = self.out / "user-work"
        marker.write_text("keep")
        with self.assertRaises(ValueError):
            EXPORT.prepare(self.repo, self.out)
        self.assertEqual(marker.read_text(), "keep")
        with self.assertRaises(ValueError):
            EXPORT.prepare(self.repo, self.repo / "new-tree")
        self.assertFalse((self.repo / "new-tree").exists())

    def test_ignored_build_outputs_are_not_copied_but_untracked_source_is(self):
        self.put(".gitignore", "/plugins/\n")
        self.put("plugins/peersus-upload/target/private-build-output", "not source")
        EXPORT.prepare(self.repo, self.out)
        self.assertFalse((self.out / "extension/peersus-upload/target").exists())
        self.assertTrue((self.out / "extension/peersus-upload/src/lib.rs").is_file())

    def test_symlink_input_is_rejected_before_output_is_created(self):
        outside = self.root / "outside.rs"
        outside.write_text("outside input")
        (self.repo / "plugins/peersus-upload/src/link.rs").symlink_to(outside)
        with self.assertRaises(ValueError):
            EXPORT.prepare(self.repo, self.out)
        self.assertFalse(self.out.exists())

    def test_credential_like_filename_is_rejected_without_opening_it(self):
        self.put("plugins/peersus-upload/.env.local", "TEST_FIXTURE_ONLY")
        with self.assertRaises(ValueError):
            EXPORT.prepare(self.repo, self.out)
        self.assertFalse(self.out.exists())

    def test_incomplete_plugin_cannot_produce_a_source_branch(self):
        (self.repo / "plugins/peersus-upload/plugin.json").unlink()
        with self.assertRaises(ValueError):
            EXPORT.prepare(self.repo, self.out)
        self.assertFalse(self.out.exists())

    def test_standalone_control_guest_is_exported_without_becoming_a_native_workspace_member(self):
        self.put("plugins/peersus-control/wasm/Cargo.toml", '[package]\nname = "peersus-control-wasm"\n[workspace]\n')
        self.put("plugins/peersus-control/wasm/plugin.json", '{"execution":{"kind":"wasm","abi":1}}\n')
        self.put("plugins/peersus-control/wasm/src/lib.rs", "pub fn control_behavior() {}\n")
        self.put("plugins/peersus-control/submission.toml", 'schema = 1\n')
        self.put("plugins/peersus-control/ownership.json", '{"schema":1,"owner":{"login":"fixture","id":1},"contributors":[]}\n')
        # A guest-only plugin has no native process crate at its root.
        root = self.repo / "plugins/peersus-control"
        for name in ("Cargo.toml", "plugin.json", "src/lib.rs"):
            file = root / name
            if file.exists():
                file.unlink()
        EXPORT.prepare(self.repo, self.out)
        exported = self.out / "extension/peersus-control"
        self.assertEqual((exported / "wasm/src/lib.rs").read_bytes(), (root / "wasm/src/lib.rs").read_bytes())
        self.assertFalse((exported / "Cargo.toml").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
