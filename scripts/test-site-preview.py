#!/usr/bin/env python3
"""Test staged site behavior and the separation of fixture and production catalogs."""
from html.parser import HTMLParser
from pathlib import Path
import json
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "crates/peersus-app/tests/fixtures/catalog"


def approval(name, version="0.1.2"):
    """A schema-2 approval of one version, shaped as the store publishes it. The site never checks the signature."""
    return {
        "schema": 2,
        "sequence": 3,
        "entries": [{
            "name": name,
            "version": version,
            "author": "Peersus",
            "link": "https://github.com/DarkPhilosophy/Peersus",
            "description": "A test extension.",
            "artifacts": [{
                "platform": "android",
                "architecture": "aarch64",
                "archive": f"{name}-{version}-android-aarch64.tar.gz",
                "sha256": "0" * 64,
                "size": 1024,
            }],
        }],
    }


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

    def approvals(self, documents):
        directory = self.directory / "approvals"
        directory.mkdir()
        for folder, document in documents.items():
            (directory / folder).mkdir()
            (directory / folder / "approval.json").write_text(json.dumps(document))
            (directory / folder / "approval.sig").write_bytes(bytes(64))
        return directory

    def test_approvals_are_staged_under_store_with_their_index(self):
        directory = self.approvals({"test": approval("test"), "echo": approval("echo", "1.0.0")})
        result = self.build("--approvals", directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.output / "store-test").exists())
        index = json.loads((self.output / "store/index.json").read_text())
        self.assertEqual(index, {"schema": 2, "plugins": ["echo", "test"], "media": {"echo": [], "test": []}})
        for plugin in ("echo", "test"):
            for name in ("approval.json", "approval.sig"):
                self.assertEqual((self.output / "store" / plugin / name).read_bytes(), (directory / plugin / name).read_bytes())
        self.assertNotIn("data-test-catalog", Markers((self.output / "store.html").read_text()).body)

    def test_approval_of_another_extension_than_its_folder_is_rejected_before_output_is_created(self):
        self.assertNotEqual(self.build("--approvals", self.approvals({"test": approval("echo")})).returncode, 0)
        self.assertFalse(self.output.exists())

    def test_approval_with_a_short_signature_or_an_extra_file_is_rejected(self):
        directory = self.approvals({"test": approval("test")})
        (directory / "test" / "approval.sig").write_bytes(bytes(63))
        self.assertNotEqual(self.build("--approvals", directory).returncode, 0)
        (directory / "test" / "approval.sig").write_bytes(bytes(64))
        (directory / "test" / "notes.txt").write_text("extra")
        self.assertNotEqual(self.build("--approvals", directory).returncode, 0)
        self.assertFalse(self.output.exists())

    def application(self, directory, **change):
        manifest = {"schema": 1, "channel": "test", "version": "0.1.0", "build": 14, "notes": "",
                    "artifacts": [{"platform": "android", "architecture": "aarch64", "kind": "package",
                                   "file": "peersus-android-arm64.apk", "sha256": "a" * 64, "size": 4096}]}
        manifest["artifacts"][0].update(change)
        (directory / "_app").mkdir()
        (directory / "_app" / "update.json").write_text(json.dumps(manifest))
        (directory / "_app" / "update.sig").write_bytes(bytes(64))

    def test_the_application_prerelease_manifest_is_staged_under_app_beside_the_store(self):
        directory = self.approvals({"test": approval("test")})
        self.application(directory)
        result = self.build("--approvals", directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("update.json", "update.sig"):
            self.assertEqual((self.output / "app" / name).read_bytes(), (directory / "_app" / name).read_bytes())
        self.assertEqual(json.loads((self.output / "store/index.json").read_text())["plugins"], ["test"])

    def test_an_application_artifact_of_an_unknown_kind_or_a_path_is_rejected(self):
        for index, change in enumerate(({"kind": "installer"}, {"file": "../peersus.apk"})):
            with self.subTest(change=change):
                directory = self.directory / f"case-{index}"
                directory.mkdir()
                self.application(directory, **change)
                self.assertNotEqual(self.build("--approvals", directory).returncode, 0)
                self.assertFalse(self.output.exists())

    def test_every_page_and_extension_is_described_for_search_engines_and_llms(self):
        hostile = approval("test")
        hostile["entries"][0]["description"] = 'Says <script>alert("x")</script> & more.'
        result = self.build("--approvals", self.approvals({"test": hostile}))
        self.assertEqual(result.returncode, 0, result.stderr)
        page = (self.output / "store/test.html").read_text()
        self.assertNotIn("<script>alert", page)
        self.assertIn("&lt;script&gt;alert", page)
        self.assertIn('<link rel="canonical" href="https://peersus.axxa.dev/store/test.html">', page)
        self.assertIn('href="peersus://store/test"', page)
        sitemap = (self.output / "sitemap.xml").read_text()
        for path in ("/", "/store.html", "/install.html", "/privacy.html", "/store/test.html"):
            self.assertIn(f"<loc>https://peersus.axxa.dev{path}</loc>", sitemap)
        # The relay page says noindex in its own robots tag: a sitemap that listed it would ask for what the page refuses.
        self.assertNotIn("/relay.html", sitemap)
        self.assertIn("(https://peersus.axxa.dev/store/test.html)", (self.output / "llms.txt").read_text())
        self.assertIn("Sitemap: https://peersus.axxa.dev/sitemap.xml", (self.output / "robots.txt").read_text())
        for name in ("index.html", "store.html", "install.html", "privacy.html", "relay.html"):
            with self.subTest(page=name):
                html = (self.output / name).read_text()
                self.assertEqual(html.count('<link rel="canonical"'), 1)
                self.assertEqual(html.count('name="description"'), 1)

    def test_pictures_are_staged_icon_first_and_a_disguised_or_unknown_file_is_refused(self):
        png = b"\x89PNG\r\n\x1a\n" + bytes(32)
        webp = b"RIFF" + bytes(4) + b"WEBP" + bytes(16)
        directory = self.approvals({"test": approval("test")})
        media = directory / "test" / "media"
        media.mkdir()
        (media / "screenshot-2.webp").write_bytes(webp)
        (media / "screenshot-1.png").write_bytes(png)
        (media / "icon.png").write_bytes(png)
        result = self.build("--approvals", directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        index = json.loads((self.output / "store/index.json").read_text())
        self.assertEqual(index["media"]["test"], ["icon.png", "screenshot-1.png", "screenshot-2.webp"])
        self.assertEqual((self.output / "store/test/media/icon.png").read_bytes(), png)
        for name, body in (("screenshot-3.png", webp), ("screenshot-9.png", png), ("banner.png", png)):
            with self.subTest(name=name):
                trial = self.directory / f"trial-{name}"
                shutil.copytree(directory, trial)
                (trial / "test" / "media" / name).write_bytes(body)
                output = self.directory / f"out-{name}"
                refused = subprocess.run([sys.executable, str(ROOT / "site/build.py"), "--output", str(output), "--approvals", str(trial)], capture_output=True, text=True, timeout=20)
                self.assertNotEqual(refused.returncode, 0)
                self.assertFalse(output.exists())

    def test_test_catalog_and_approvals_cannot_be_combined(self):
        self.assertEqual(self.build("--test-catalog", "--approvals", self.approvals({})).returncode, 2)
        self.assertFalse(self.output.exists())

    def test_existing_output_is_not_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_bytes(b"existing work")
        self.assertNotEqual(self.build("--test-catalog").returncode, 0)
        self.assertEqual(sentinel.read_bytes(), b"existing work")
        self.assertEqual(list(self.output.iterdir()), [sentinel])


if __name__ == "__main__":
    unittest.main()
