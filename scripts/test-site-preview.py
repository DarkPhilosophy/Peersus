#!/usr/bin/env python3
"""Test staged site behavior and the separation of fixture and production catalogs."""
from html.parser import HTMLParser
from pathlib import Path
import json
import os
import posixpath
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "crates/peersus-app/tests/fixtures/catalog"
ARCHITECTURE_FILES = {
    "architecture.html", "architecture.css", "architecture.js",
    "architecture/map.json", "architecture/floorplan-MIT.txt",
}
SITE_NAV = ("index.html", "store.html", "install.html", "privacy.html", "architecture.html", "support.html")
RELAY_SUFFIXES = {".html", ".css", ".js", ".json", ".svg", ".png", ".webp", ".ico", ".txt", ".xml", ".sig", ".ttf"}
POLICY = ("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; "
          "connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'")
PUBLICATION_ORIGIN = "https://raw.githubusercontent.com"
PUBLICATION_URL = PUBLICATION_ORIGIN + "/axxa-dev/Peersus/architecture-map/map.json"


class SiteDocument(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.policy = None
        self.inline = []
        self.scripts = []
        self.styles = []
        self.header = []
        self.footer = []
        self.canonical = None
        self.region = None
        self.map_source = None
        self.map_source_region = None
        self.headings = []
        self.board_title = None
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag in ("header", "footer"):
            self.region = tag
        if tag == "meta" and a.get("http-equiv", "").lower() == "content-security-policy":
            self.policy = a.get("content")
        if tag == "script" and a.get("src"):
            self.scripts.append(a["src"])
        if tag == "link" and a.get("rel") == "stylesheet":
            self.styles.append(a.get("href"))
        if tag == "link" and a.get("rel") == "canonical":
            self.canonical = a.get("href")
        if tag == "style" or tag == "script" and not a.get("src") or "style" in a or any(k.startswith("on") for k in a):
            self.inline.append(tag)
        if a.get("id") == "map-source":
            self.map_source = a
            self.map_source_region = self.region
        if tag == "h1":
            self.headings.append(a.get("class", "").split())
        if a.get("id") == "board-title":
            self.board_title = tag
        if tag == "a" and self.region:
            getattr(self, self.region).append((a.get("href"), a.get("aria-current")))

    def handle_endtag(self, tag):
        if tag == self.region:
            self.region = None


def approval(name, version="0.1.2"):
    """A schema-2 approval of one version, shaped as the store publishes it. The site never checks the signature."""
    return {
        "schema": 2,
        "sequence": 3,
        "entries": [{
            "name": name,
            "version": version,
            "author": "Peersus",
            "link": "https://github.com/axxa-dev/Peersus",
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

    def test_preview_contains_the_architecture_page_and_csp_compatible_assets(self):
        result = self.build("--test-catalog")
        self.assertEqual(result.returncode, 0, result.stderr)
        files = [p for p in self.output.rglob("*") if p.is_file()]
        paths = {p.relative_to(self.output).as_posix() for p in files}
        self.assertEqual(ARCHITECTURE_FILES - paths, set())
        page = SiteDocument((self.output / "architecture.html").read_text())
        self.assertEqual(page.policy, POLICY.replace("connect-src 'self'", f"connect-src 'self' {PUBLICATION_ORIGIN}"))
        self.assertEqual(page.map_source["data-live"], PUBLICATION_URL)
        self.assertEqual(page.map_source["data-snapshot"], "architecture/map.json")
        self.assertEqual(page.map_source_region, "footer")
        self.assertEqual(page.headings, [["visually-hidden"]])
        self.assertEqual(page.board_title, "h2")
        self.assertEqual(int(page.map_source["data-check-interval"]), 24 * 60 * 60 * 1000)
        self.assertEqual(int(page.map_source["data-retry-interval"]), 60 * 60 * 1000)
        for name in SITE_NAV:
            if name != "architecture.html":
                # Navigation changed; every original document still carries its own original policy.
                original = SiteDocument((ROOT / "site" / name).read_text())
                self.assertEqual(SiteDocument((self.output / name).read_text()).policy, original.policy)
        self.assertEqual(page.inline, [])
        self.assertIn("architecture.js", page.scripts)
        self.assertIn("app.js", page.scripts)
        self.assertEqual(page.styles, ["style.css", "architecture.css"])
        self.assertIn(("architecture/floorplan-MIT.txt", None), page.footer)
        self.assertIn(("fonts/OFL.txt", None), page.footer)
        # The test archive and the public installer are staged for the website, not
        # served by the project relay. Every architecture asset must be relay-readable.
        self.assertEqual({p.relative_to(self.output).as_posix() for p in files if p.suffix not in RELAY_SUFFIXES},
                         {"store-test/echo.tar.gz", "install.sh"})
        self.assertEqual((self.output / "install.sh").read_bytes(), (ROOT / "site/install.sh").read_bytes())
        self.assertLessEqual(len(files) + 1, 512)  # / also occupies a relay route
        self.assertLessEqual(sum(p.stat().st_size for p in files), 32 * 1024 * 1024)
        self.assertTrue(all(len(p.relative_to(self.output).parts) - 1 <= 8 for p in files))
        self.assertFalse(any(p.is_symlink() for p in self.output.rglob("*")))
        graph = json.loads((self.output / "architecture/map.json").read_bytes())
        self.assertEqual(graph["schema"], "peersus/architecture-view@1")
        self.assertRegex(graph["source"]["commit"], r"^[a-f0-9]{40}$")
        self.assertRegex(graph["source"]["date"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual([ref for ref, _ in graph["peersus"]["views"]],
                         ["root", "n:code", "n:extensions", "n:send", "n:delivery"])
        self.assertEqual({f["id"] for f in graph["flows"]},
                         {"send", "control-visit", "extension-install", "pairing"})

    def test_architecture_navigation_is_shared_by_site_pages(self):
        result = self.build("--approvals", self.approvals({"test": approval("test")}))
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in (*SITE_NAV, "store/test.html"):
            with self.subTest(page=name):
                page = SiteDocument((self.output / name).read_text())
                architecture_href = posixpath.relpath("architecture.html", posixpath.dirname(name) or ".")
                link = (architecture_href, "page" if name == "architecture.html" else None)
                self.assertEqual([a for a in page.header if a[0] == architecture_href], [link])
                self.assertEqual([a for a in page.footer if a[0] == architecture_href], [link])
        architecture = SiteDocument((self.output / "architecture.html").read_text())
        self.assertEqual(architecture.canonical, "https://peersus.axxa.dev/architecture.html")
        self.assertIn(f"<loc>{architecture.canonical}</loc>", (self.output / "sitemap.xml").read_text())
        self.assertIn(f"]({architecture.canonical})", (self.output / "llms.txt").read_text())

    def test_published_architecture_contains_only_public_structural_data(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        tracked = set(subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0"))
        # The handful of approved extension evidence paths are structural only.
        extension_evidence = {
            f"plugins/peersus-{slug}/wasm/plugin.json"
            for slug in ("clipboard", "control", "location", "monitor", "test", "upload")
        } | {"plugins/peersus-control/wasm/src/remote.rs"}
        personal = {os.environ.get("USER", ""), os.environ.get("LOGNAME", ""), socket.gethostname()}
        personal -= {"", "root", "runner", "test"}
        local_text = re.compile(
            r"\b(?:file|ssh|sftp|ws|wss)://|(?:(?<![\w\\:])[a-z]:[\\/](?=[a-z0-9\\])|/(?:Users|home|root|var|tmp|etc|run|mnt|Volumes)/)"
            r"|\b(?:\d{1,3}\.){3}\d{1,3}\b|\[(?:[0-9a-f]{0,4}:){2,}[0-9a-f:]*\]"
            r"|\b[\w.+-]+@[\w.-]+\.[a-z]{2,}\b|\b(?:localhost|[\w-]+\.(?:local|lan|internal))(?::\d+)?\b"
            r"|\badb-[a-z0-9-]+\b", re.I)
        for name in sorted(ARCHITECTURE_FILES):
            with self.subTest(asset=name):
                text = (self.output / name).read_text()
                if name == "architecture.html":
                    # The normal chrome and the single configured publisher have fixed public addresses.
                    text = text.replace("https://peersus.axxa.dev/architecture.html", "")
                    text = text.replace("https://github.com/axxa-dev/Peersus", "")
                    text = text.replace(PUBLICATION_URL, "").replace(PUBLICATION_ORIGIN, "")
                self.assertFalse(local_text.search(text), "private address, path, email or serial")
                self.assertFalse(re.search(r"https?://", text, re.I), "unexpected public URL")
                self.assertFalse(any(re.search(r"(?<![\w-])" + re.escape(value) + r"(?![\w-])", text, re.I)
                                     for value in personal), "local user or machine name")
        graph = json.loads((self.output / "architecture/map.json").read_bytes())
        refs = set()
        for n in graph["nodes"]:
            refs.add(n["source"]["path"])
            if n.get("doc"):
                refs.add(n["doc"]["path"])
        refs.update(v["source"]["path"] for v in graph["peersus"]["violations"])
        self.assertEqual(refs - tracked - extension_evidence, set())
        for ref in refs:
            self.assertTrue(all(part not in ("", ".", "..") for part in ref.split("/")))
            self.assertNotIn("\\", ref)

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

    def application(self, directory, name="peersus-0.1.0-dev.14-android-aarch64.apk", extra=""):
        (directory / "_app").mkdir()
        (directory / "_app" / "SHA256SUMS").write_text(f"{'a' * 64}  {name}\n{extra}")
        (directory / "_app" / "SHA256SUMS.sig").write_bytes(bytes(64))

    def test_the_application_release_list_is_staged_under_app_beside_the_store(self):
        directory = self.approvals({"test": approval("test")})
        self.application(directory)
        result = self.build("--approvals", directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("SHA256SUMS", "SHA256SUMS.sig"):
            self.assertEqual((self.output / "app" / name).read_bytes(), (directory / "_app" / name).read_bytes())
        self.assertEqual(json.loads((self.output / "store/index.json").read_text())["plugins"], ["test"])

    def test_an_application_list_the_application_would_refuse_is_rejected(self):
        stem = "peersus-0.1.0-dev.14-linux-x86_64"
        for index, change in enumerate((
                {"name": "peersus-0.1.0-dev.14-android-aarch64.exe"},
                {"name": "../peersus-0.1.0-dev.14-android-aarch64.apk"},
                {"name": "peersus-0.1.0-dev.014-android-aarch64.apk"},
                {"extra": f"{'b' * 64}  peersus-0.1.0-dev.15-linux-x86_64.tar.gz\n"},
                {"extra": f"{'b' * 64}  {stem}/peersus\n"},
        )):
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

    def test_cloudflare_worker_sends_the_relays_policy(self):
        # The Worker answers for the server when it is down; a policy that drifted from the relay's would weaken that copy.
        patch = (ROOT / "packaging/relay/iroh-relay-1.4.1-peersus-site.patch").read_text()
        relay = re.search(r'^\+\s*"(default-src[^"]*)"', patch, re.M)[1]
        worker = (ROOT / "packaging/cloudflare/worker.js").read_text()
        value = re.search(r'"Content-Security-Policy":\s*((?:"[^"]*"\s*\+?\s*)+),', worker)[1]
        self.assertEqual("".join(re.findall(r'"([^"]*)"', value)), relay)


if __name__ == "__main__":
    unittest.main()
