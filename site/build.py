#!/usr/bin/env python3
"""Dependency-free static staging; never deploys and never overwrites output."""
import argparse
import hashlib
import html as markup
import json
import re
import shutil
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

SOURCE = Path(__file__).resolve().parent
FILES = ("index.html", "install.html", "privacy.html", "store.html", "support.html", "style.css", "app.js", "store.js", "downloads.js", "relay.html", "relay.css", "supporters.json", "brand/peersus.svg", "brand/peersus-mark.svg", "tour.js", "shots/phone-send.webp", "shots/phone-receive.webp", "shots/phone-peers.webp", "shots/phone-transfers.webp", "shots/phone-store.webp", "shots/phone-remote.webp", "shots/phone-monitor.webp", "shots/desktop-send.webp")
# Files of the site kept elsewhere in the repository, staged under these names: the application's own typeface and its licence,
# one copy for the program and the site.
BORROWED = {
    "fonts/Quicksand.ttf": SOURCE.parent / "crates/peersus-ui/assets/fonts/Quicksand[wght].ttf",
    "fonts/OFL.txt": SOURCE.parent / "crates/peersus-ui/assets/fonts/OFL.txt",
}
# The project's public repository: the one place the site links for its source, and whose star count it shows.
REPOSITORY_URL = "https://github.com/DarkPhilosophy/Peersus"
EXTERNAL = {"https://ko-fi.com/DarkPhilosophy", "https://www.iroh.computer/", REPOSITORY_URL}
# Where the site is served (the project relay's own name): what canonical addresses, the sitemap and llms.txt name.
SITE_URL = "https://peersus.axxa.dev"
# Each page's address and what it is about, for search engines, link previews and llms.txt. A page that already says what it is
# about in its own description keeps it; these are for the pages that do not.
PAGES = {
    "index.html": ("/", "Peersus hands a file from one of your devices to another: paired by a number both people read, and nothing lands until the receiving device says yes. Free software."),
    "store.html": ("/store.html", "Extensions the maintainer approved for Peersus, with what each does, its platforms and its downloads. Peersus verifies every signature itself."),
    "install.html": ("/install.html", "Download the Peersus development prerelease, with the size of every build, or build it from source on Linux and Android; and what is not ready yet."),
    "privacy.html": ("/privacy.html", "How Peersus transfers are encrypted, what a relay can see, what an extension may reach, and what Peersus and this website do and do not collect."),
    "support.html": ("/support.html", "Voluntary support for Peersus: what a tip is and is not, how the payment provider handles it, and who chose to be thanked in public."),
    "relay.html": ("/relay.html", "The Peersus relay: what it forwards between devices that cannot reach each other directly, and what it cannot read."),
}

# The site's one header and footer, in that order on every page but the relay's (the relay serves that page alone): what each
# page shows of the others, so that the store, the guide and the notes are one site and not pages that lead out of it.
NAVIGATION = (("index.html", "Home"), ("store.html", "Store"), ("install.html", "Install"),
              ("privacy.html", "Security"), ("support.html", "Support"))
HEADER_SLOT = '<header class="site-header"></header>'
FOOTER_SLOT = "<footer></footer>"


def compact(count):
    """A count as a header says it: 999, 1.2k, 12k, 1.2M."""
    for unit, size in (("M", 1_000_000), ("k", 1_000)):
        if count >= size:
            value = count / size
            return f"{value:.1f}".rstrip("0").rstrip(".") + unit if value < 10 else f"{int(value)}{unit}"
    return str(count)


def size_words(bytes_):
    """A size as the store's pages say it (store.js `size`)."""
    if bytes_ >= 1048576:
        return f"{bytes_ / 1048576:.1f} MiB"
    if bytes_ >= 1024:
        return f"{bytes_ / 1024:.1f} KiB"
    return f"{bytes_} B"


def chrome(current, html, stars, up=""):
    """The page with the site's header and footer in their slots. `current` is marked in both; `stars` is the repository's
    star count read when the site was built (never by the visitor's browser), or None when it was not read; `up` leads from a
    page in a folder back to the site's root."""
    if html.count(HEADER_SLOT) != 1 or html.count(FOOTER_SLOT) != 1:
        raise ValueError(f"A page of the site has one header slot and one footer slot: {current}")
    e = markup.escape
    # The home page by its canonical address, the site's root, never a second address `index.html`.
    def address(page):
        return (up or "./") if page == "index.html" else f"{up}{page}"
    def links():
        return "".join(f'<a href="{address(page)}"' + (' aria-current="page"' if page == current else "") + f">{e(title)}</a>"
                       for page, title in NAVIGATION)
    star = "" if stars is None else f'<span class="stars" title="{stars:,} stars on GitHub"><span aria-hidden="true">★</span> {compact(stars)}<span class="visually-hidden"> stars</span></span>'
    github = f'<a class="github" href="{REPOSITORY_URL}" rel="external noopener noreferrer">GitHub{star}</a>'
    brand = (f'<a class="brand" href="{address("index.html")}" aria-label="Peersus home"><span class="brand-line"><img class="mark" src="{up}brand/peersus-mark.svg" '
             'width="44" height="44" alt=""><span class="word">eersus</span></span><span class="edition">THE TRANSFER PORTAL</span></a>')
    header = (f'<header class="site-header">{brand}<nav aria-label="Main navigation">{links()}{github}'
              '<button id="theme" type="button" hidden aria-label="Switch to dark theme">Dark theme</button></nav></header>')
    footer = (f'<footer><a class="brand" href="{address("index.html")}" aria-label="Peersus home"><span class="brand-line"><img class="mark" src="{up}brand/peersus-mark.svg" '
              'width="32" height="32" alt=""><span class="word">eersus</span></span></a><p>Move it. Don’t store it.</p>'
              f'<div>{links()}<a href="{REPOSITORY_URL}" rel="external noopener noreferrer">Source on GitHub</a></div>'
              f'<small>Pre-release · No analytics · No advertising · Apache-2.0 · Typeface Quicksand, <a href="{up}fonts/OFL.txt">SIL Open Font Licence</a></small></footer>')
    return html.replace(HEADER_SLOT, header).replace(FOOTER_SLOT, footer)


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
        # The pages' CSP is script-src 'self'; style-src 'self': inline code would be blocked in the browser, so it is refused here.
        if "style" in attrs:
            raise ValueError("Inline style attributes are not permitted")
        # Structured data (JSON-LD) is data the browser never runs, so the CSP does not concern it; its content must be JSON.
        self.data = tag == "script" and attrs.get("type") == "application/ld+json" and "src" not in attrs
        if tag == "style" or (tag == "script" and "src" not in attrs and not self.data):
            raise ValueError(f"An inline <{tag}> is not permitted")

    def handle_data(self, data):
        if getattr(self, "data", False):
            json.loads(data)

    def handle_endtag(self, tag):
        if tag == "script":
            self.data = False


def validate():
    documents = {}
    for name in FILES:
        path = SOURCE / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing or symlinked source: {name}")
        if path.suffix == ".html":
            document = Document()
            text = path.read_text(encoding="utf-8")
            try:
                document.feed(chrome(name, text, None) if name in PAGES and name != "relay.html" else text)
            except ValueError as error:
                raise ValueError(f"{name}: {error}") from None
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
            if url.path.endswith("/") or url.path in (".", "./", ".."):
                target = target / "index.html"
            if not target.is_relative_to(SOURCE) or target.relative_to(SOURCE).as_posix() not in (*FILES, *BORROWED):
                raise ValueError(f"Unstaged local resource in {name}: {raw}")
            if url.fragment:
                dest = documents.get(target.relative_to(SOURCE).as_posix())
                if not dest or unquote(url.fragment) not in dest.ids:
                    raise ValueError(f"Missing fragment in {name}: {raw}")
    for name, path in BORROWED.items():
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing or symlinked source: {name}")
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


def check_document(catalog, archives: Path | None):
    """Check one signed catalog document (a schema-1 catalog or a schema-2 approval) by the application's own rules, so the site
    never shows what the application would refuse. With `archives`, every archive it names must be there at its signed size and
    digest. It CANNOT check the signature: that needs the public key and a crypto library, and this script has neither on
    purpose. The application is the verifier; this is display-only. Returns the names of the archives it lists."""
    if not isinstance(catalog, dict) or set(catalog) != {"schema", "sequence", "entries"} or type(catalog["schema"]) is not int or catalog["schema"] not in (1, 2):
        raise ValueError("Unexpected catalog shape")
    if not isinstance(catalog["sequence"], int) or isinstance(catalog["sequence"], bool) or catalog["sequence"] < 0:
        raise ValueError("Invalid catalog sequence")
    entries = catalog["entries"]
    if not isinstance(entries, list) or len(entries) > 512:
        raise ValueError("A catalog lists at most 512 extensions")
    common_keys = {"name", "version", "author", "link", "description"}
    archive_keys = {"sha256", "size", "archive"}
    seen = set()
    named = []
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("A catalog entry must be an object")
        grouped = "artifacts" in entry
        expected = common_keys | ({"artifacts"} if grouped else archive_keys)
        if set(entry) != expected or (grouped and catalog["schema"] != 2):
            raise ValueError("A catalog entry has unexpected fields")
        for field in common_keys:
            if not isinstance(entry[field], str):
                raise ValueError(f"Catalog field {field} must be text")
        if not NAME.match(entry["name"]) or entry["name"] in seen:
            raise ValueError("Invalid or duplicate extension name")
        seen.add(entry["name"])
        if not VERSION.match(entry["version"]):
            raise ValueError("Invalid version")
        for field, limit in (("author", 80), ("description", 1000)):
            text = entry[field]
            if (field == "author" and not text.strip()) or len(text) > limit or CONTROL.search(text):
                raise ValueError(f"Invalid {field}")
        url = urlsplit(entry["link"])
        if url.scheme != "https" or not url.hostname or url.username or url.password or url.port or url.query or url.fragment or any(c.isspace() for c in entry["link"]):
            raise ValueError("An extension link must be a plain https address")
        artifacts = entry["artifacts"] if grouped else [{key: entry[key] for key in archive_keys}]
        if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 32:
            raise ValueError("Expected between one and 32 platform artifacts")
        targets = set()
        for artifact in artifacts:
            if not isinstance(artifact, dict) or set(artifact) != archive_keys | ({"platform", "architecture"} if grouped else set()):
                raise ValueError("A platform artifact has unexpected fields")
            if grouped:
                platform, architecture = artifact["platform"], artifact["architecture"]
                if platform not in ("android", "linux", "macos", "windows") or architecture not in ("any", "aarch64", "x86_64", "arm", "x86"):
                    raise ValueError("Unsupported artifact platform or architecture")
                if (platform, architecture) in targets:
                    raise ValueError("Duplicate platform artifacts")
                targets.add((platform, architecture))
            if not isinstance(artifact["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]):
                raise ValueError("Invalid archive digest")
            if type(artifact["size"]) is not int or not 0 < artifact["size"] <= MAX_ARCHIVE:
                raise ValueError("Invalid archive size")
            if not isinstance(artifact["archive"], str) or not ARCHIVE.match(artifact["archive"]) or artifact["archive"].startswith("."):
                raise ValueError("Invalid archive file name")
            if archives is not None:
                archive = archives / artifact["archive"]
                if not archive.is_file() or archive.is_symlink() or archive.stat().st_size != artifact["size"]:
                    raise ValueError(f"Archive for {entry['name']} is missing or is not the signed size")
                if hashlib.sha256(archive.read_bytes()).hexdigest() != artifact["sha256"]:
                    raise ValueError(f"Archive for {entry['name']} is not the signed digest")
            if artifact["archive"] not in named:
                named.append(artifact["archive"])
    return named


def signed_file(directory: Path, document: str, signature: str, limit: int):
    """The JSON of `document` beside its raw 64-byte Ed25519 `signature`, both regular files."""
    for name in (document, signature):
        path = directory / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing or symlinked file: {directory.name}/{name}")
    if (directory / signature).stat().st_size != 64:
        raise ValueError(f"{signature} must be a raw 64-byte Ed25519 signature")
    raw = (directory / document).read_bytes()
    if len(raw) > limit:
        raise ValueError(f"{document} is too large")
    return json.loads(raw)


def validate_catalog(directory: Path):
    """A signed catalog directory with its archives (the development fixture). Returns the files to stage."""
    catalog = signed_file(directory, "catalog.json", "catalog.sig", 1024 * 1024)
    return ["catalog.json", "catalog.sig", *check_document(catalog, directory)]


def validate_approvals(directory: Path):
    """The store's signed approvals, one folder per extension (`<id>/approval.json`, `approval.sig`, as
    scripts/fetch-store-approvals.py gathers them). Each approves exactly one version of the extension its folder names; the
    archives stay where they are published, so they are not checked here. Returns the extension ids, in order."""
    if not directory.is_dir() or directory.is_symlink():
        raise ValueError("The approvals must be a directory")
    plugins = []
    for folder in sorted(directory.iterdir()):
        if folder.name in (APPLICATION, REPOSITORY):
            continue
        if folder.is_symlink() or not folder.is_dir() or not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", folder.name):
            raise ValueError(f"Unexpected entry among the approvals: {folder.name}")
        if {path.name for path in folder.iterdir()} - {"media"} != {"approval.json", "approval.sig"}:
            raise ValueError(f"An approval folder holds approval.json, approval.sig and its pictures only: {folder.name}")
        pictures(folder)
        approval = signed_file(folder, "approval.json", "approval.sig", 64 * 1024)
        check_document(approval, None)
        if approval["schema"] != 2 or [entry["name"] for entry in approval["entries"]] != [folder.name]:
            raise ValueError(f"An approval approves one version of the extension its folder names: {folder.name}")
        plugins.append(folder.name)
    if len(plugins) > 512:
        raise ValueError("At most 512 approvals")
    return plugins


# The pictures an extension gives the store (`store/` of its source, gathered into `media/`): an icon and up to eight screenshots,
# PNG or WebP, each at most 1 MiB, recognised by their first bytes and not by their names alone.
PICTURE = re.compile(r"^(icon|screenshot-[1-8])\.(png|webp)$")
MOST_PICTURE = 1024 * 1024


def pictures(folder: Path):
    """The pictures of one approval folder, checked, in the order a page shows them: the icon, then the screenshots."""
    media = folder / "media"
    if not media.exists():
        return []
    if media.is_symlink() or not media.is_dir():
        raise ValueError(f"Unexpected pictures of {folder.name}")
    found = []
    for path in sorted(media.iterdir()):
        if path.is_symlink() or not path.is_file() or not PICTURE.match(path.name):
            raise ValueError(f"Unexpected picture of {folder.name}: {path.name}")
        body = path.read_bytes()
        png = body.startswith(b"\x89PNG\r\n\x1a\n")
        webp = len(body) > 12 and body[:4] == b"RIFF" and body[8:12] == b"WEBP"
        if not 0 < len(body) <= MOST_PICTURE or not (png if path.suffix == ".png" else webp):
            raise ValueError(f"Not a picture of the kind its name says, or too large: {folder.name}/{path.name}")
        found.append(path.name)
    if len({Path(name).stem for name in found}) != len(found):
        raise ValueError(f"Two pictures of {folder.name} have the same name")
    return sorted(found, key=lambda name: (not name.startswith("icon"), name))


# The folder of the gathered approvals that holds the application's prerelease manifest: no extension id starts with `_`.
APPLICATION = "_app"
# The file of the gathered approvals with the repository's star count, read by scripts/fetch-store-approvals.py when it gathers
# them: the site shows the number without the visitor's browser asking GitHub anything. Absent, the header links without it.
REPOSITORY = "_repository.json"


def validate_repository(directory: Path):
    """The star count gathered beside the approvals, or None when there is none."""
    path = directory / REPOSITORY
    if not path.exists():
        return None
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 1024:
        raise ValueError("The repository file is a small regular file")
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict) or set(document) != {"stars"} or type(document["stars"]) is not int or not 0 <= document["stars"] <= 10**9:
        raise ValueError("The repository file holds its star count only")
    return document["stars"]
WORD = re.compile(r"^[a-z0-9_]{1,32}$")
CHANNEL = re.compile(r"^[a-z0-9-]{1,32}$")


def validate_application(directory: Path):
    """The application's signed development prerelease manifest (`_app/update.json`, `update.sig`), checked by the shape the
    application itself accepts (`app_update.rs`): the site lists its downloads and verifies nothing. False when absent."""
    folder = directory / APPLICATION
    if not folder.exists():
        return False
    if folder.is_symlink() or not folder.is_dir() or {path.name for path in folder.iterdir()} != {"update.json", "update.sig"}:
        raise ValueError("The application folder holds update.json and update.sig only")
    document = signed_file(folder, "update.json", "update.sig", 64 * 1024)
    keys = {"schema", "channel", "version", "build", "artifacts"}
    if not isinstance(document, dict) or not keys <= set(document) <= keys | {"notes"}:
        raise ValueError("Unexpected application manifest shape")
    if type(document["schema"]) is not int or not isinstance(document["channel"], str) or not CHANNEL.match(document["channel"]):
        raise ValueError("Invalid application manifest channel")
    if not isinstance(document["version"], str) or not VERSION.match(document["version"]) or type(document["build"]) is not int or document["build"] < 0:
        raise ValueError("Invalid application version or build")
    if not isinstance(document.get("notes", ""), str) or len(document.get("notes", "")) > 2000:
        raise ValueError("Invalid application notes")
    artifacts = document["artifacts"]
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 64:
        raise ValueError("Expected between one and 64 application artifacts")
    for artifact in artifacts:
        if not isinstance(artifact, dict) or set(artifact) != {"platform", "architecture", "kind", "file", "sha256", "size"}:
            raise ValueError("An application artifact has unexpected fields")
        if not all(isinstance(artifact[key], str) and WORD.match(artifact[key]) for key in ("platform", "architecture")):
            raise ValueError("Invalid application artifact platform or architecture")
        if artifact["kind"] not in ("archive", "image", "package"):
            raise ValueError("Invalid application artifact kind")
        if not isinstance(artifact["file"], str) or not ARCHIVE.match(artifact["file"]) or artifact["file"].startswith("."):
            raise ValueError("Invalid application artifact file name")
        if not isinstance(artifact["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"]):
            raise ValueError("Invalid application artifact digest")
        if type(artifact["size"]) is not int or not 0 < artifact["size"] <= 1024 * 1024 * 1024:
            raise ValueError("Invalid application artifact size")
    return True


def head_metadata(name, html):
    """The page with what describes it to search engines and link previews, added after its viewport line: its canonical address,
    a description when it has none, and the Open Graph and card tags. The page's own words are kept as they are."""
    path, about = PAGES[name]
    title = re.search(r"<title>(.*?)</title>", html, re.S).group(1)
    described = re.search(r'<meta name="description" content="([^"]*)"', html)
    description = markup.unescape(described.group(1)) if described else about
    tags = [] if described else [f'<meta name="description" content="{markup.escape(description)}">']
    tags += [
        f'<link rel="canonical" href="{SITE_URL}{path}">',
        '<meta property="og:site_name" content="Peersus">',
        '<meta property="og:type" content="website">',
        f'<meta property="og:title" content="{markup.escape(markup.unescape(title))}">',
        f'<meta property="og:description" content="{markup.escape(description)}">',
        f'<meta property="og:url" content="{SITE_URL}{path}">',
        '<meta name="twitter:card" content="summary">',
    ]
    if name == "index.html":
        tags.append('<script type="application/ld+json">' + json.dumps({
            "@context": "https://schema.org", "@type": "SoftwareApplication", "name": "Peersus",
            "applicationCategory": "UtilitiesApplication", "operatingSystem": "Linux, Android",
            "description": description, "url": f"{SITE_URL}/", "license": "https://www.apache.org/licenses/LICENSE-2.0",
            "offers": {"@type": "Offer", "price": "0", "priceCurrency": "EUR"},
        }, ensure_ascii=False).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026") + "</script>")
    anchor = re.search(r'<meta name="viewport"[^>]*>', html)
    if not anchor:
        raise ValueError(f"Missing viewport line to add metadata after: {name}")
    return html[:anchor.end()] + "".join(tags) + html[anchor.end():]


PLATFORM_NAMES = {"android": "Android", "linux": "Linux", "macos": "macOS", "windows": "Windows"}


def extension_page(entry, page_style, media, stars):
    """One extension's own page (`store/<id>.html`): what it is, by whom, for which platforms, Install and its downloads, and a
    way to the store. Built from the approval, every value escaped; read by search engines and anyone given the address."""
    name, version = entry["name"], entry["version"]
    e = markup.escape
    platforms = sorted({PLATFORM_NAMES[artifact["platform"]] for artifact in entry["artifacts"]})
    release = f"https://github.com/DarkPhilosophy/Peersus/releases/download/extension%2F{name}%2Fv{version}/"
    downloads = "".join(
        f'<li><a href="{e(release + artifact["archive"])}">{e(PLATFORM_NAMES[artifact["platform"]])}</a> '
        f'<code>{e(artifact["archive"])}</code> · <span title="{artifact["size"]:,} bytes">{size_words(artifact["size"])}</span> · SHA-256 <code>{e(artifact["sha256"])}</code></li>'
        for artifact in entry["artifacts"])
    description = entry["description"] or f"{name}, an extension for Peersus."
    data = json.dumps({
        "@context": "https://schema.org", "@type": "SoftwareApplication", "name": name, "softwareVersion": version,
        "applicationCategory": "UtilitiesApplication", "operatingSystem": ", ".join(platforms), "description": description,
        "author": {"@type": "Organization", "name": entry["author"]}, "url": f"{SITE_URL}/store/{name}.html",
        "isPartOf": {"@type": "WebSite", "name": "Peersus", "url": f"{SITE_URL}/"},
    }, ensure_ascii=False).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    title = f"{name} — Peersus extension store"
    sizes = sorted(artifact["size"] for artifact in entry["artifacts"])
    lightest, heaviest = size_words(sizes[0]), size_words(sizes[-1])
    weight = lightest if lightest == heaviest else f"{lightest}–{heaviest}"
    # The pictures its source gave the store, as staged beside its approval: what it looks like before anyone installs it.
    shots = [picture for picture in media if picture.startswith("screenshot-")]
    gallery = ("" if not shots else '<div class="store-shots">' + "".join(
        f'<img src="{e(name)}/media/{e(picture)}" alt="{e(name)}, screenshot {index}" loading="lazy">'
        for index, picture in enumerate(shots, 1)) + "</div>")
    page = (
        '<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
        f'<meta name="description" content="{e(description[:300])}">'
        '<meta name="color-scheme" content="light dark"><meta name="referrer" content="no-referrer">'
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'\">"
        f'<title>{e(title)}</title><link rel="icon" href="../brand/peersus.svg" type="image/svg+xml"><link rel="stylesheet" href="../{page_style}">'
        # Every page of the site carries the same scripts: a page shown in place of this one starts with what it needs.
        '<script src="../tour.js" defer></script><script src="../store.js" defer></script><script src="../downloads.js" defer></script><script src="../app.js" defer></script>'
        f'<link rel="canonical" href="{SITE_URL}/store/{e(name)}.html"><meta property="og:site_name" content="Peersus"><meta property="og:type" content="website">'
        f'<meta property="og:title" content="{e(title)}"><meta property="og:description" content="{e(description[:300])}"><meta property="og:url" content="{SITE_URL}/store/{e(name)}.html">'
        f'<meta name="twitter:card" content="summary"><script type="application/ld+json">{data}</script></head>'
        f'<body><a class="skip" href="#main">Skip to content</a>{HEADER_SLOT}'
        f'<main class="document store" id="main"><p class="eyebrow">EXTENSION · v{e(version)}</p><h1>{e(name)}</h1>'
        f'<p class="intro">{e(description)}</p><p>By {e(entry["author"])} · for {e(", ".join(platforms))} · {weight} to download · approved and signed by the maintainer.</p>'
        f'<p class="store-sheet-actions"><a class="store-button store-button-large store-install" href="peersus://store/{e(name)}">Install {e(name)}</a>'
        f'<a class="store-button store-button-large store-button-quiet" href="../store.html#{e(name)}">Open in the store</a></p>'
        '<p class="store-hint">Install opens Peersus at this extension; Peersus checks the signature and the archive, then asks you to accept its licence and notice.</p>'
        f'{gallery}<h2>Downloads</h2><ul>{downloads}</ul>'
        f'<p class="fine">The author’s page, as text: <code>{e(entry["link"])}</code></p></main>'
        f'{FOOTER_SLOT}</body></html>\n'
    )
    # Under store/: every address of the site's header and footer leads one folder up, and the store is the page it belongs to.
    return chrome("store.html", page, stars, up="../")


def indexed(name):
    """Whether a page lets search engines index it: one that says `noindex` in its own robots tag stays out of the sitemap."""
    robots = re.search(r'<meta name="robots" content="([^"]*)"', (SOURCE / name).read_text(encoding="utf-8"))
    return not (robots and "noindex" in robots.group(1))


def sitemap(extensions):
    """Every page of the site that may be indexed and every extension's page, by its canonical address."""
    addresses = [SITE_URL + path for name, (path, _) in PAGES.items() if indexed(name)] + [f"{SITE_URL}/store/{name}.html" for name in extensions]
    return ('<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
            + "".join(f"  <url><loc>{markup.escape(address)}</loc></url>\n" for address in addresses) + "</urlset>\n")


def llms(entries):
    """The site in the llms.txt form (https://llmstxt.org): what Peersus is, its pages, and each approved extension."""
    lines = ["# Peersus", "",
             "> Peersus moves files between your own devices, end to end encrypted, with no account and no server of the project: "
             "the receiving device decides what comes in. Free software (Apache-2.0), in development prerelease; Linux and Android "
             "have been exercised on real devices. Extensions add features and start with every permission refused.", "",
             "## Pages", ""]
    titles = {"index.html": "Home", "store.html": "Extension store", "install.html": "Download and install",
              "privacy.html": "Security and privacy", "support.html": "Support", "relay.html": "The relay"}
    lines += [f"- [{titles[name]}]({SITE_URL}{path}): {about}" for name, (path, about) in PAGES.items()]
    if entries:
        lines += ["", "## Extensions", ""]
        lines += [f"- [{entry['name']}]({SITE_URL}/store/{entry['name']}.html): {' '.join(entry['description'].split()) or 'An extension for Peersus.'} "
                  f"(v{entry['version']}, by {entry['author']})" for entry in entries]
    lines += ["", "## Source", "", f"- [Peersus on GitHub]({REPOSITORY_URL}): the source code (Apache-2.0), issues, releases and the extension branch."]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New, nonexistent output directory")
    stores = parser.add_mutually_exclusive_group()
    stores.add_argument("--approvals", type=Path, help="The store's signed approvals to stage under store/ (scripts/fetch-store-approvals.py)")
    stores.add_argument("--test-catalog", action="store_true", help="Clearly marked development preview with public fixtures under store-test/, never store/")
    args = parser.parse_args()
    output = args.output.absolute()
    if output.exists() or output.is_symlink() or output.resolve().is_relative_to(SOURCE):
        parser.error("Output must be a nonexistent directory outside site/")
    validate()
    fixture = SOURCE.parent / "crates/peersus-app/tests/fixtures/catalog"
    store = validate_catalog(fixture) if args.test_catalog else []
    approvals = validate_approvals(args.approvals.absolute()) if args.approvals else []
    application = validate_application(args.approvals.absolute()) if args.approvals else False
    stars = validate_repository(args.approvals.absolute()) if args.approvals else None
    output.mkdir(parents=True, exist_ok=False)
    for name in FILES:
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        if name.endswith(".html"):
            html = head_metadata(name, (SOURCE / name).read_text(encoding="utf-8"))
            if name != "relay.html":
                html = chrome(name, html, stars)
            if not args.test_catalog:
                destination.write_text(html, encoding="utf-8")
                continue
            marker = '<body data-test-catalog="true"><aside class="preview-banner" role="note">Development testing preview · Unpublished prerelease · Public test fixtures only, not production approvals.</aside>'
            if "<body>" not in html:
                raise ValueError(f"Missing preview annotation point: {name}")
            destination.write_text(html.replace("<body>", marker, 1), encoding="utf-8")
        else:
            shutil.copyfile(SOURCE / name, destination)
    for name, path in BORROWED.items():
        (output / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, output / name)
    entries = [json.loads((args.approvals.absolute() / plugin / "approval.json").read_text(encoding="utf-8"))["entries"][0] for plugin in approvals]
    for entry in entries:
        page = extension_page(entry, "style.css", pictures(args.approvals.absolute() / entry["name"]), stars)
        document = Document()
        document.feed(page)
        (output / "store").mkdir(parents=True, exist_ok=True)
        (output / "store" / f"{entry['name']}.html").write_text(page, encoding="utf-8")
    (output / "sitemap.xml").write_text(sitemap([entry["name"] for entry in entries]), encoding="utf-8")
    (output / "llms.txt").write_text(llms(entries), encoding="utf-8")
    (output / "robots.txt").write_text(f"User-agent: *\nAllow: /\nSitemap: {SITE_URL}/sitemap.xml\n", encoding="utf-8")
    for name in store:
        destination = output / "store-test" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(fixture / name, destination)
    media = {}
    for plugin in approvals:
        (output / "store" / plugin).mkdir(parents=True)
        for name in ("approval.json", "approval.sig"):
            shutil.copyfile(args.approvals.absolute() / plugin / name, output / "store" / plugin / name)
        media[plugin] = pictures(args.approvals.absolute() / plugin)
        if media[plugin]:
            (output / "store" / plugin / "media").mkdir()
            for name in media[plugin]:
                shutil.copyfile(args.approvals.absolute() / plugin / "media" / name, output / "store" / plugin / "media" / name)
    if approvals:
        (output / "store" / "index.json").write_text(json.dumps({"schema": 2, "plugins": approvals, "media": media}) + "\n", encoding="utf-8")
    if application:
        (output / "app").mkdir()
        for name in ("update.json", "update.sig"):
            shutil.copyfile(args.approvals.absolute() / APPLICATION / name, output / "app" / name)
    print(f"Staged {len(FILES) + len(BORROWED)} static files, {len(store)} test store files, {len(approvals)} approvals and {'the' if application else 'no'} application prerelease. No deployment performed.")

if __name__ == "__main__":
    main()
