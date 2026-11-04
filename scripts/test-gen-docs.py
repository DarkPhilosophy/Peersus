#!/usr/bin/env python3
"""Behavioral regression coverage for deterministic, bounded-region generation."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("gen-docs.py")
spec = importlib.util.spec_from_file_location("gen_docs", SCRIPT)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class Markers(unittest.TestCase):
    def test_only_the_marked_region_changes_and_rendering_is_idempotent(self):
        source = "manual before\n<!-- DATA-START -->\nold\n<!-- DATA-END -->\nmanual after\n"
        actual = generator.replace_block(source, "DATA", "new\ncontent")
        self.assertEqual(actual, "manual before\n<!-- DATA-START -->\nnew\ncontent\n<!-- DATA-END -->\nmanual after\n")
        self.assertEqual(generator.replace_block(actual, "DATA", "new\ncontent"), actual)

    def test_missing_duplicate_and_reversed_markers_are_refused(self):
        for source in ("no markers", "<!-- DATA-START -->", "<!-- DATA-END --><!-- DATA-START -->", "<!-- DATA-START --><!-- DATA-START --><!-- DATA-END -->", "<!-- DATA-START --><!-- DATA-END --><!-- DATA-END -->"):
            with self.subTest(source=source), self.assertRaises(ValueError):
                generator.replace_block(source, "DATA", "new")


class Generation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.put("scripts/gen-docs.py", SCRIPT.read_text())
        self.put("Cargo.toml", '[workspace]\nmembers = ["crates/example"]\n[workspace.package]\nversion = "1.2.3"\nrust-version = "1.96"\nlicense = "Apache-2.0"\n')
        self.put("rust-toolchain.toml", '[toolchain]\nchannel = "1.96.1"\n')
        self.put("README.md", "manual intro\n" + "\n".join(f"<!-- {name}-START -->\n<!-- {name}-END -->" for name in ("STATUS", "PLUGINS", "CI", "LATEST")) + "\nmanual footer\n")
        self.put("CHANGELOG.md", "manual history\n<!-- CHANGES-START -->\n<!-- CHANGES-END -->\nmanual footer\n")
        self.notes = {"unreleased": {"Fixed": ["A synthetic bug."]}, "releases": [{"version": "1.0.0", "date": "2026-01-02", "changes": {"Added": ["A synthetic feature."]}}]}
        self.put("docs/changes.json", json.dumps(self.notes))
        self.put("crates/example/plugin.json", json.dumps({"platforms": ["linux"], "exports": [{"service": "test.service", "version": 2}]}))
        self.put(".github/workflows/test.yml", "name: Synthetic checks\n")

    def put(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_generator(self, *arguments):
        environment = dict(os.environ)
        # No VCS checkout or network is needed for generation.
        return subprocess.run([sys.executable, str(self.root / "scripts/gen-docs.py"), *arguments], cwd=self.root, env=environment, capture_output=True, text=True)

    def test_check_is_read_only_and_real_generation_tracks_source_changes(self):
        original = (self.root / "README.md").read_bytes()
        self.assertEqual(self.run_generator("--check").returncode, 1)
        self.assertEqual((self.root / "README.md").read_bytes(), original)
        result = self.run_generator()
        self.assertEqual(result.returncode, 0, result.stderr)
        rendered = (self.root / "README.md").read_text()
        self.assertIn("**1.2.3**", rendered)
        self.assertIn("`test.service` v2", rendered)
        self.assertIn("Synthetic checks", rendered)
        self.assertTrue(rendered.startswith("manual intro\n"))
        self.assertTrue(rendered.endswith("manual footer\n"))
        self.assertIn("## 1.0.0 — 2026-01-02", (self.root / "CHANGELOG.md").read_text())
        self.assertEqual(self.run_generator("--check").returncode, 0)
        self.notes["unreleased"]["Fixed"] = ["The changed behavior."]
        self.put("docs/changes.json", json.dumps(self.notes))
        self.assertEqual(self.run_generator("--check").returncode, 1)
        self.assertEqual(self.run_generator().returncode, 0)
        for name in ("README.md", "CHANGELOG.md"):
            self.assertIn("The changed behavior.", (self.root / name).read_text())
        self.assertEqual(self.run_generator("--check").returncode, 0)
        self.assertEqual(self.run_generator().stdout, "")

    def test_generated_links_resolve_from_the_actual_readme_location(self):
        target = self.root / ".github/README.md"
        (self.root / "README.md").rename(target)
        (self.root / "README.md").symlink_to(".github/README.md")
        result = self.run_generator()
        self.assertEqual(result.returncode, 0, result.stderr)
        links = re.findall(r"\]\(([^)]+)\)", target.read_text())
        resolved = {(target.parent / link).resolve() for link in links}
        self.assertEqual(resolved, {
            (self.root / "crates/example/plugin.json").resolve(),
            (self.root / ".github/workflows/test.yml").resolve(),
        })
        for path in resolved:
            self.assertTrue(path.is_file(), f"Generated link does not resolve: {path.name}")

    def test_a_bad_second_output_does_not_partially_write_the_first(self):
        original = (self.root / "README.md").read_bytes()
        self.put("CHANGELOG.md", "missing markers\n")
        self.assertEqual(self.run_generator().returncode, 1)
        self.assertEqual((self.root / "README.md").read_bytes(), original)

    def test_invalid_duplicate_releases_are_refused(self):
        self.notes["releases"] *= 2
        self.put("docs/changes.json", json.dumps(self.notes))
        self.assertEqual(self.run_generator().returncode, 1)


if __name__ == "__main__":
    unittest.main()
