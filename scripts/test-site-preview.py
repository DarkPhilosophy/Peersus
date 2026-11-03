#!/usr/bin/env python3
"""Test staged site behavior and the separation of fixture and production catalogs."""
from html.parser import HTMLParser
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "crates/peersus-app/tests/fixtures/catalog"


class Markers(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.body = {}
        self.notes = 0
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "body":
            self.body = attrs
        if tag == "aside" and attrs.get("role") == "note":
            self.notes += 1


class SitePreview(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="site-preview-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.output = self.directory / "site"

    def build(self, *args):
        return subprocess.run([sys.executable, str(ROOT / "site/build.py"), "--output", str(self.output), *map(str, args)], capture_output=True, text=True, timeout=20)

    def test_fixture_catalog_never_populates_production_store_and_annotates_every_page(self):
        result = self.build("--test-catalog")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.output / "store").exists())
        files = {p.name for p in (self.output / "store-test").iterdir()}
        self.assertEqual(files, {"catalog.json", "catalog.sig", "echo.tar.gz"})
        for name in files:
            self.assertEqual((self.output / "store-test" / name).read_bytes(), (FIXTURES / name).read_bytes())
        for page in self.output.glob("*.html"):
            with self.subTest(page=page.name):
                markers = Markers(page.read_text())
                self.assertEqual(markers.body.get("data-test-catalog"), "true")
                self.assertEqual(markers.notes, 1)

    def test_empty_preview_contains_no_test_or_production_catalog(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.output / "store").exists())
        self.assertFalse((self.output / "store-test").exists())
        self.assertNotIn("data-test-catalog", Markers((self.output / "index.html").read_text()).body)

    def test_test_and_approved_catalog_options_cannot_be_combined(self):
        self.assertEqual(self.build("--test-catalog", "--catalog", FIXTURES).returncode, 2)
        self.assertFalse(self.output.exists())

    def test_existing_output_is_not_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_bytes(b"existing work")
        self.assertNotEqual(self.build("--test-catalog").returncode, 0)
        self.assertEqual(sentinel.read_bytes(), b"existing work")
        self.assertEqual(list(self.output.iterdir()), [sentinel])

    def test_changed_archive_is_rejected_before_output_is_created(self):
        catalog = self.directory / "catalog"
        catalog.mkdir()
        for name in ("catalog.json", "catalog.sig", "echo.tar.gz"):
            shutil.copyfile(FIXTURES / name, catalog / name)
        (catalog / "echo.tar.gz").write_bytes(b"not the approved archive")
        self.assertNotEqual(self.build("--catalog", catalog).returncode, 0)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
