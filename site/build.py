#!/usr/bin/env python3
"""Dependency-free static staging; never deploys and never overwrites output."""
import argparse
import hashlib
import json
import re
import shutil
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

SOURCE = Path(__file__).resolve().parent
FILES = ("index.html", "install.html", "privacy.html", "style.css", "app.js", "supporters.json", "brand/peersus.svg")
EXTERNAL = {"https://ko-fi.com/DarkPhilosophy"}

class Document(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.links = []
        self.title = False
        self.lang = False
        self.csp = False
    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            if attrs["id"] in self.ids:
                raise ValueError("Duplicate element id")
            self.ids.add(attrs["id"])
        self.title |= tag == "title"
        self.lang |= tag == "html" and bool(attrs.get("lang"))
        self.csp |= tag == "meta" and attrs.get("http-equiv", "").lower() == "content-security-policy"
        for key in ("href", "src"):
            if key in attrs:
                self.links.append((tag, key, attrs[key]))
        if any(key.startswith("on") for key in attrs):
            raise ValueError("Inline handlers are not permitted")


def validate():
    documents = {}
    for name in FILES:
        path = SOURCE / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing or symlinked source: {name}")
        if path.suffix == ".html":
            document = Document()
            document.feed(path.read_text(encoding="utf-8"))
            if not (document.title and document.lang and document.csp):
                raise ValueError(f"Missing title, language, or CSP: {name}")
            documents[name] = document
    for name, document in documents.items():
        for tag, key, raw in document.links:
            url = urlsplit(raw)
            if url.scheme or url.netloc:
                if tag != "a" or key != "href" or raw not in EXTERNAL:
                    raise ValueError(f"Unexpected external resource in {name}")
                continue
            if not raw or url.query:
                raise ValueError(f"Empty or unexpected query link in {name}")
            target = (SOURCE / name).parent / unquote(url.path) if url.path else SOURCE / name
            target = target.resolve()
            if not target.is_relative_to(SOURCE) or target.relative_to(SOURCE).as_posix() not in FILES:
                raise ValueError(f"Unstaged local resource in {name}: {raw}")
            if url.fragment:
                dest = documents.get(target.relative_to(SOURCE).as_posix())
                if not dest or unquote(url.fragment) not in dest.ids:
                    raise ValueError(f"Missing fragment in {name}: {raw}")
    supporters = json.loads((SOURCE / "supporters.json").read_text(encoding="utf-8"))
    if not isinstance(supporters, list) or len(supporters) > 200:
        raise ValueError("Expected an acknowledgement list of at most 200 entries")
    names = set()
    for entry in supporters:
        if not isinstance(entry, dict) or set(entry) != {"displayName", "consent"}:
            raise ValueError("Acknowledgements allow only displayName and consent")
        name = entry["displayName"]
        if entry["consent"] is not True or not isinstance(name, str) or not name.strip() or len(name) > 80:
            raise ValueError("An approved, bounded display name is required")
        if name != name.strip() or re.search(r"[\x00-\x1f\x7f-\x9f\u202a-\u202e\u2066-\u2069]", name) or name in names:
            raise ValueError("Invalid or duplicate acknowledgement")
        names.add(name)


NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
ARCHIVE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f\u202a-\u202e\u2066-\u2069]")
MAX_ARCHIVE = 128 * 1024 * 1024


def validate_catalog(directory: Path):
    """Check a signed catalog directory (the output of `cargo xtask catalog-sign`) by the application's own rules, so the
    site never publishes what the application would refuse. It CANNOT check the signature: that needs the public key and a
    crypto library, and this script has neither on purpose. The application is the verifier; this is display-only.

    Returns the files to stage, as names relative to `directory`."""
    for name in ("catalog.json", "catalog.sig"):
        path = directory / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing or symlinked catalog file: {name}")
    if (directory / "catalog.sig").stat().st_size != 64:
        raise ValueError("catalog.sig must be a raw 64-byte Ed25519 signature")
    raw = (directory / "catalog.json").read_bytes()
    if len(raw) > 1024 * 1024:
        raise ValueError("catalog.json is too large")
    catalog = json.loads(raw)
    if not isinstance(catalog, dict) or set(catalog) != {"schema", "sequence", "entries"} or catalog["schema"] != 1:
        raise ValueError("Unexpected catalog shape")
    if not isinstance(catalog["sequence"], int) or isinstance(catalog["sequence"], bool) or catalog["sequence"] < 0:
        raise ValueError("Invalid catalog sequence")
    entries = catalog["entries"]
    if not isinstance(entries, list) or len(entries) > 512:
        raise ValueError("A catalog lists at most 512 extensions")
    keys = {"name", "version", "sha256", "size", "author", "link", "description", "archive"}
    seen = set()
    staged = ["catalog.json", "catalog.sig"]
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != keys:
            raise ValueError("A catalog entry has unexpected fields")
        for field in ("name", "version", "sha256", "author", "link", "description", "archive"):
            if not isinstance(entry[field], str):
                raise ValueError(f"Catalog field {field} must be text")
        if not NAME.match(entry["name"]) or entry["name"] in seen:
            raise ValueError("Invalid or duplicate extension name")
        seen.add(entry["name"])
        if not VERSION.match(entry["version"]) or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
            raise ValueError("Invalid version or digest")
        if not isinstance(entry["size"], int) or isinstance(entry["size"], bool) or not 0 < entry["size"] <= MAX_ARCHIVE:
            raise ValueError("Invalid archive size")
        for field, limit in (("author", 80), ("description", 1000)):
            text = entry[field]
            if (field == "author" and not text.strip()) or len(text) > limit or CONTROL.search(text):
                raise ValueError(f"Invalid {field}")
        url = urlsplit(entry["link"])
        if url.scheme != "https" or not url.hostname or url.username or url.password or url.port or url.query or url.fragment or any(c.isspace() for c in entry["link"]):
            raise ValueError("An extension link must be a plain https address")
        if not ARCHIVE.match(entry["archive"]) or entry["archive"].startswith("."):
            raise ValueError("Invalid archive file name")
        archive = directory / entry["archive"]
        if not archive.is_file() or archive.is_symlink() or archive.stat().st_size != entry["size"]:
            raise ValueError(f"Archive for {entry['name']} is missing or is not the signed size")
        if hashlib.sha256(archive.read_bytes()).hexdigest() != entry["sha256"]:
            raise ValueError(f"Archive for {entry['name']} is not the signed digest")
        staged.append(entry["archive"])
    return staged


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New, nonexistent output directory")
    parser.add_argument("--catalog", type=Path, help="Signed catalog directory to publish under store/ (optional)")
    args = parser.parse_args()
    output = args.output.absolute()
    if output.exists() or output.is_symlink() or output.resolve().is_relative_to(SOURCE):
        parser.error("Output must be a nonexistent directory outside site/")
    validate()
    store = validate_catalog(args.catalog.absolute()) if args.catalog else []
    output.mkdir(parents=True, exist_ok=False)
    for name in FILES:
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(SOURCE / name, destination)
    for name in store:
        destination = output / "store" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(args.catalog.absolute() / name, destination)
    print(f"Staged {len(FILES)} static files and {len(store)} store files. No deployment performed.")

if __name__ == "__main__":
    main()
