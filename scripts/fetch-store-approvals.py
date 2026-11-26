#!/usr/bin/env python3
"""Gather what the website lists: for each extension source on the `extension` branch, the signed approval of the version
that source names (`approval.json`, `approval.sig` of release `extension/<id>/v<version>`); and the signed list of the
application's newest development release (`SHA256SUMS`, `SHA256SUMS.sig` of the newest tag `v<version>-dev.<build>`), which
names its downloads.

Read-only, anonymous GitHub API requests, every answer bounded. It verifies nothing: the application checks the signatures
with the maintainer's key, and site/build.py checks the documents' shape before it stages them. A source whose version has
no release, or whose release has no approval, is left out and said so. Writes `<out>/<id>/approval.json` and `approval.sig`,
and `<out>/_app/SHA256SUMS` and `SHA256SUMS.sig` when there is such a release (`_app` is no extension id); and the
repository's star count as `<out>/_repository.json`, which the site shows so that no visitor's browser asks GitHub for it.
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.github.com/repos/"
ID = re.compile(r"^[a-z][a-z0-9_-]{0,63}$")
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
MOST = {"tree": 4 * 1024 * 1024, "json": 256 * 1024, "approval": 64 * 1024, "picture": 1024 * 1024}
# The pictures an extension's source may give the store, in its `store/` folder: an icon and up to eight screenshots.
PICTURE = re.compile(r"^(icon|screenshot-[1-8])\.(png|webp)$")


def get(url, limit, accept="application/vnd.github+json"):
    request = urllib.request.Request(url, headers={"Accept": accept, "User-Agent": "peersus-site"})
    # GitHub allows an anonymous address 60 API requests an hour; a token (CI's GITHUB_TOKEN, or `gh auth token`) lifts that.
    # Only for the API, and as a header urllib does not carry over a redirect to a download host.
    token = os.environ.get("GITHUB_TOKEN")
    if token and url.startswith(API):
        request.add_unredirected_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read(limit + 1)
    if len(body) > limit:
        raise ValueError(f"answer too large: {url}")
    return body


def sources(repo):
    """Plugin id -> (manifest blob sha, {picture name: blob sha}), the manifest by the application's own rule: `wasm/plugin.json`,
    else the root manifest; the pictures from the same source's `store/` folder."""
    tree = json.loads(get(f"{API}{repo}/git/trees/extension?recursive=1", MOST["tree"]))
    if tree.get("truncated"):
        raise ValueError("the branch listing is truncated")
    found = {}
    pictures = {}
    for blob in tree["tree"]:
        parts = blob["path"].split("/")
        if blob.get("type") != "blob" or parts[0] != "extension":
            continue
        if len(parts) == 4 and parts[2] == "store" and PICTURE.match(parts[3]):
            pictures.setdefault(parts[1].removeprefix("peersus-"), {})[parts[3]] = blob["sha"]
            continue
        if len(parts) == 3 and parts[2] == "plugin.json":
            wasm = False
        elif len(parts) == 4 and parts[2:] == ["wasm", "plugin.json"]:
            wasm = True
        else:
            continue
        plugin = parts[1].removeprefix("peersus-")
        if not ID.match(plugin):
            continue
        if plugin not in found or wasm:
            found[plugin] = blob["sha"]
    return {plugin: (sha, pictures.get(plugin, {})) for plugin, sha in found.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="New, nonexistent output directory")
    parser.add_argument("--repo", default="axxa-dev/Peersus")
    args = parser.parse_args()
    if args.out.exists():
        parser.exit(1, "The output directory must not exist\n")
    staged = {}
    for plugin, (sha, pictures) in sorted(sources(args.repo).items()):
        blob = json.loads(get(f"{API}{args.repo}/git/blobs/{sha}", MOST["json"], "application/vnd.github.raw+json"))
        version = (blob.get("package") or {}).get("version", "")
        if not VERSION.match(version):
            print(f"{plugin}: its source names no valid version; left out", file=sys.stderr)
            continue
        try:
            release = json.loads(get(f"{API}{args.repo}/releases/tags/extension/{plugin}/v{version}", MOST["json"]))
        except urllib.error.HTTPError as error:
            if error.code == 404:
                print(f"{plugin} {version}: no release, so not approved; left out", file=sys.stderr)
                continue
            raise
        assets = {asset["name"]: asset["url"] for asset in release.get("assets", [])}
        if not {"approval.json", "approval.sig"} <= assets.keys():
            print(f"{plugin} {version}: the release has no approval; left out", file=sys.stderr)
            continue
        staged[plugin] = {
            name: get(assets[name], MOST["approval"], "application/octet-stream") for name in ("approval.json", "approval.sig")
        }
        # Pictures of the same source as the approved version: what the store shows before anyone installs it.
        staged[plugin].update({
            f"media/{name}": get(f"{API}{args.repo}/git/blobs/{picture}", MOST["picture"], "application/vnd.github.raw+json")
            for name, picture in sorted(pictures.items())
        })
    application = app_release(args.repo)
    stars = repository_stars(args.repo)
    args.out.mkdir(parents=True)
    (args.out / "_repository.json").write_text(json.dumps({"stars": stars}) + "\n", encoding="utf-8")
    for plugin, files in staged.items():
        (args.out / plugin).mkdir()
        for name, body in files.items():
            (args.out / plugin / name).parent.mkdir(exist_ok=True)
            (args.out / plugin / name).write_bytes(body)
    if application:
        (args.out / "_app").mkdir()
        for name, body in application.items():
            (args.out / "_app" / name).write_bytes(body)
    print(f"Gathered {len(staged)} approvals and {'the' if application else 'no'} application prerelease; nothing was verified or published.")


def repository_stars(repo):
    """How many people starred the repository on GitHub, as its public metadata says."""
    stars = json.loads(get(f"{API}{repo}", MOST["json"])).get("stargazers_count")
    if type(stars) is not int or not 0 <= stars <= 10**9:
        raise ValueError("the repository's star count is not a count")
    return stars


# The application's release tags, as the application reads them (`app_update.rs`): `v<version>-<channel>.<build>`.
APP_TAG = re.compile(r"^refs/tags/(v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-dev\.([1-9][0-9]{0,19}))$")


def app_release(repo):
    """The signed list of the application's newest development release, or nothing when there is none."""
    references = json.loads(get(f"{API}{repo}/git/matching-refs/tags/v", MOST["tree"]))
    tags = [match for match in (APP_TAG.match(reference.get("ref", "")) for reference in references) if match]
    if not tags:
        print("no development release is tagged; the site lists no application download", file=sys.stderr)
        return None
    newest = max(tags, key=lambda match: tuple(int(part) for part in match.group(2, 3, 4, 5))).group(1)
    try:
        release = json.loads(get(f"{API}{repo}/releases/tags/{newest}", MOST["json"]))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            print(f"{newest}: tagged but not released; the site lists no application download", file=sys.stderr)
            return None
        raise
    assets = {asset["name"]: asset["url"] for asset in release.get("assets", [])}
    if release.get("draft") or not {"SHA256SUMS", "SHA256SUMS.sig"} <= assets.keys():
        print(f"{newest}: the release has no signed list; left out", file=sys.stderr)
        return None
    return {name: get(assets[name], MOST["approval"], "application/octet-stream") for name in ("SHA256SUMS", "SHA256SUMS.sig")}


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, urllib.error.URLError) as error:
        sys.exit(f"Approvals not gathered: {error}")
