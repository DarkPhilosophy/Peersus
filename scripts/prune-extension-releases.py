#!/usr/bin/env python3
"""Remove older releases of one extension after its verified replacement is live in the app and on the site."""
import argparse
from contextlib import nullcontext
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener

# Match the stable IDs and bounded major.minor.patch versions admitted by verify-extension-admission.py.
PLUGIN = r'[a-z][a-z0-9_-]{0,63}'
NUMBER = r'(?:0|[1-9][0-9]{0,9})'
VERSION = re.compile(rf'{NUMBER}(?:\.{NUMBER}){{2}}')
TAG = re.compile(rf'extension/({PLUGIN})/v({VERSION.pattern})')
MANIFEST = re.compile(r'extension/([a-z][a-z0-9_-]{0,79})/(?:wasm/)?plugin\.json')
ARCHIVE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.tar\.gz')


class NoStoreRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


open_store = build_opener(NoStoreRedirect).open


def github(repository, path, *options, method='GET'):
    try:
        return subprocess.run(['gh', 'api', '--hostname', 'github.com', '--method', method,
                               f'repos/{repository}/{path}', *options],
                              check=True, capture_output=True, text=True, timeout=30).stdout
    except subprocess.CalledProcessError as error:
        # Do not echo API response bodies or headers into the runner log.
        status = re.search(r'\bHTTP ([0-9]{3})\b', error.stderr)
        reason = f'HTTP {status[1]}' if status else f'gh exit {error.returncode}'
        raise ValueError(f'GitHub {method} failed ({reason})') from None
    except subprocess.TimeoutExpired:
        raise ValueError(f'GitHub {method} timed out; its outcome was not confirmed') from None


def plan(repository, tag):
    plugin, version = TAG.fullmatch(tag).groups()
    offered = tuple(map(int, version.split('.')))
    # Finish the paginated read before deleting anything, so page offsets cannot shift under our own writes.
    pages = json.loads(github(repository, 'releases?per_page=100', '--paginate', '--slurp'))
    if not isinstance(pages, list) or not pages or any(not isinstance(page, list) for page in pages):
        raise ValueError('Invalid GitHub release inventory')
    current, older, drafts, ids = None, [], set(), set()
    for page in pages:
        for release in page:
            if (not isinstance(release, dict) or not isinstance(release.get('tag_name'), str)
                    or type(release.get('draft')) is not bool or type(release.get('id')) is not int
                    or release['id'] <= 0 or release['id'] in ids):
                raise ValueError('Invalid or duplicate record in the GitHub release inventory')
            ids.add(release['id'])
            name = release['tag_name']
            match = TAG.fullmatch(name)
            if not match:
                print(f'Keeping tag outside the admitted extension format: {ascii(name)}', file=sys.stderr)
            elif release['draft']:
                drafts.add(name)
            elif name == tag:
                if current is not None:
                    raise ValueError(f'Multiple published releases claim `{tag}`')
                current = release
            elif match[1] == plugin:
                numeric = tuple(map(int, match[2].split('.')))
                if numeric < offered:
                    older.append((numeric, name, release['id']))
    return current, sorted(older), drafts


def uploaded_assets(release, tag):
    if release is None or release.get('prerelease') is not False:
        raise ValueError(f'Publish and verify the stable replacement `{tag}` before cleanup')
    assets = {}
    inventory = release.get('assets')
    if not isinstance(inventory, list):
        raise ValueError(f'Replacement `{tag}` has no valid asset inventory')
    for asset in inventory:
        if (not isinstance(asset, dict) or not isinstance(asset.get('name'), str)
                or asset['name'] in assets or asset.get('state') != 'uploaded'
                or type(asset.get('size')) is not int or asset['size'] <= 0):
            raise ValueError(f'Replacement `{tag}` has an incomplete or ambiguous asset inventory')
        assets[asset['name']] = asset
    if (not {'approval.json', 'approval.sig', 'review.json'} <= assets.keys()
            or assets['approval.sig']['size'] != 64 or not any(ARCHIVE.fullmatch(name) for name in assets)):
        raise ValueError(f'Replacement `{tag}` is incomplete; publish its approval, signature, review and archives first')
    return assets


def check_source(repository, manifest_path, tag, version):
    try:
        manifest = json.loads(github(repository, f'contents/{manifest_path}?ref=extension',
                                    '-H', 'Accept: application/vnd.github.raw+json'))
        source = manifest['package']['version']
        if not isinstance(source, str) or not VERSION.fullmatch(source):
            raise ValueError('The source manifest has no admitted version')
        if manifest['execution']['kind'] != 'wasm':
            raise ValueError('The selected source manifest is not the store’s WebAssembly manifest')
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ValueError(f'Cannot check the extension branch manifest for `{tag}`: {error}') from None
    if source != version:
        raise ValueError(f'The extension branch manifest still names {ascii(source)}, not `{tag}`. '
                         'Merge the matching source before cleanup')


def check_store(store_url, plugin, version, tag, assets):
    url = f'{store_url.rstrip("/")}/{plugin}/approval.json'
    try:
        with open_store(Request(url, headers={'Cache-Control': 'no-cache'}), timeout=30) as response:
            raw = response.read(65537)
        if len(raw) > 65536:
            raise ValueError('The published approval exceeds 64 KiB')
        approval = json.loads(raw)
        entry, = approval['entries']
        if approval['schema'] != 2 or entry['name'] != plugin:
            raise ValueError('The published approval does not identify this extension')
        site_version = entry['version']
        if not isinstance(site_version, str) or not VERSION.fullmatch(site_version):
            raise ValueError('The published approval has no admitted version')
    except HTTPError as error:
        error.close()
        raise ValueError(f'Cannot read the published store approval for `{tag}` (HTTP {error.code}). '
                         'Restage and deploy the store before cleanup') from None
    except URLError:
        raise ValueError(f'Cannot reach the published store to check `{tag}`') from None
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ValueError(f'Cannot check the published store approval for `{tag}`: {error}. '
                         'Restage and deploy the store before cleanup') from None
    if site_version != version:
        raise ValueError(f'The published store approval still names {ascii(site_version)}, not `{tag}`. '
                         'Restage and deploy the store before cleanup')
    if assets['approval.json'].get('digest') != f'sha256:{hashlib.sha256(raw).hexdigest()}':
        raise ValueError(f'The deployed approval does not match the approval asset of `{tag}`. '
                         'Restage and deploy the store from the published release')
    platforms = set()
    artifacts = entry.get('artifacts')
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 32:
        raise ValueError(f'The deployed approval for `{tag}` has no valid archive inventory')
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            raise ValueError(f'The deployed approval for `{tag}` has an invalid archive entry')
        name, size, digest, platform = (artifact.get(key) for key in ('archive', 'size', 'sha256', 'platform'))
        if (not isinstance(name, str) or not ARCHIVE.fullmatch(name)
                or type(size) is not int or not isinstance(digest, str)
                or not isinstance(platform, str)):
            raise ValueError(f'The deployed approval for `{tag}` has invalid archive metadata')
        asset = assets.get(name)
        if not asset or asset['size'] != size or asset.get('digest') != f'sha256:{digest}':
            raise ValueError(f'Replacement `{tag}` is missing the approved archive bytes for `{name}`')
        platforms.add(platform)
    if not {'linux', 'android', 'macos'} <= platforms:
        raise ValueError(f'The deployed approval for `{tag}` lacks a required WebAssembly platform')


def prune(args, summary):
    def report(message, *, failed=False):
        print(message, file=sys.stderr if failed else sys.stdout, flush=True)
        if summary:
            print(f'- {message}', file=summary, flush=True)

    tag = args.tag
    plugin, version = TAG.fullmatch(tag).groups()
    if summary:
        print(f'### Extension release retention for `{tag}`\n', file=summary, flush=True)
    current, older, drafts = plan(args.repository, tag)
    if not older:
        report('No older published releases to remove.')
        return 0
    if args.dry_run:
        for _, name, _ in older:
            if name in drafts:
                report(f'Keeping `{name}` because a draft also uses that tag.')
            else:
                report(f'Would remove release and tag `{name}` after publication and store cutover.')
        return 0

    assets = uploaded_assets(current, tag)
    check_source(args.repository, args.source_manifest, tag, version)
    check_store(args.store_url, plugin, version, tag, assets)
    failed = False
    for _, name, release_id in older:
        try:
            if name in drafts:
                raise ValueError('a draft also uses this tag')
            # Delete the selected published release by ID, never by a tag lookup that could select a draft.
            github(args.repository, f'releases/{release_id}', method='DELETE')
        except (OSError, ValueError) as error:
            report(f'Could not remove release `{name}`: {error}', failed=True)
            failed = True
            continue
        try:
            github(args.repository, f'git/refs/tags/{name}', method='DELETE')
        except (OSError, ValueError) as error:
            report(f'Could not remove tag `{name}`: {error}', failed=True)
            report(f'Removed release `{name}`; tag removal failed.')
            failed = True
        else:
            report(f'Removed release and tag `{name}`.')
    return int(failed)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True, help='GitHub repository (owner/name)')
    parser.add_argument('--tag', required=True, help='Verified replacement tag: extension/<id>/v<version>')
    parser.add_argument('--source-manifest', help='Exact WebAssembly manifest path used by the store on the extension branch')
    parser.add_argument('--store-url', help='Published store directory URL, containing <id>/approval.json')
    parser.add_argument('--dry-run', action='store_true', help='List candidates without any write or cutover check')
    args = parser.parse_args(argv)
    if not re.fullmatch(r'[A-Za-z0-9-]+/[A-Za-z0-9_.-]+', args.repository) or args.repository.split('/')[1] in ('.', '..'):
        parser.error('Use a GitHub repository in owner/name form')
    identity = TAG.fullmatch(args.tag)
    if not identity:
        parser.error('Use an exact admitted extension/<id>/v<version> tag')
    if not args.dry_run:
        manifest = MANIFEST.fullmatch(args.source_manifest or '')
        if not manifest or manifest[1].removeprefix('peersus-') != identity[1]:
            parser.error('Specify the exact store source manifest of this extension with --source-manifest')
        try:
            url = urlsplit(args.store_url or '')
        except ValueError:
            parser.error('Specify the deployed HTTPS store directory with --store-url')
        if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment:
            parser.error('Specify the deployed HTTPS store directory with --store-url')
    try:
        output = os.environ.get('GITHUB_STEP_SUMMARY')
        with (Path(output).open('a', encoding='utf-8') if output else nullcontext()) as summary:
            return prune(args, summary)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Extension release cleanup refused: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
