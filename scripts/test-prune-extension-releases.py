#!/usr/bin/env python3
"""Exercise retention through a stand-in for gh and a local store response, without network access."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('prune', Path(__file__).with_name('prune-extension-releases.py'))
prune = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prune)

REPOSITORY = 'owner/repo'
CURRENT = 'extension/test/v0.1.10'
MANIFEST = 'extension/peersus-test/wasm/plugin.json'
STORE = 'https://store.example.invalid/store'
ARCHIVES = [{'archive': f'test-{platform}.tar.gz', 'platform': platform, 'size': 100, 'sha256': 'a' * 64}
            for platform in ('linux', 'android', 'macos')]

# Keep actual release and tag state separate, so a second invocation and a partial deletion are observable.
FAKE_GH = '''
import json
import os
from pathlib import Path
import sys

state_file = Path(os.environ['FAKE_GH_STATE'])
state = json.loads(state_file.read_text())
api, hostname_option, host, method_option, method, endpoint, *flags = sys.argv[1:]
assert (api, hostname_option, host, method_option) == ('api', '--hostname', 'github.com', '--method')
assert endpoint.startswith('repos/owner/repo/')
path = endpoint.removeprefix('repos/owner/repo/')
state['calls'].append([method, path])
state_file.write_text(json.dumps(state))
if method == 'GET' and path == 'releases?per_page=100':
    assert flags == ['--paginate', '--slurp']
    print(json.dumps([state['releases'][:3], state['releases'][3:]]))
elif method == 'GET' and path == 'contents/extension/peersus-test/wasm/plugin.json?ref=extension':
    assert flags == ['-H', 'Accept: application/vnd.github.raw+json']
    print(json.dumps({'package': {'version': state['source']}, 'execution': {'kind': 'wasm', 'abi': 1}}))
elif method == 'DELETE' and not flags:
    if path in state['fails']:
        print('gh: unable to delete (HTTP 403)', file=sys.stderr)
        sys.exit(1)
    if path.startswith('releases/'):
        release_id = int(path.split('/')[-1])
        release = next(row for row in state['releases'] if row['id'] == release_id)
        assert not release['draft']
        state['releases'].remove(release)
    elif path.startswith('git/refs/tags/'):
        state['refs'].remove(path.removeprefix('git/refs/tags/'))
    else:
        raise AssertionError(path)
    state_file.write_text(json.dumps(state))
else:
    raise AssertionError(sys.argv)
'''


def approval(version='0.1.10'):
    return json.dumps({'schema': 2, 'sequence': 1, 'entries': [
        {'name': 'test', 'version': version, 'artifacts': ARCHIVES}]}).encode()


def asset(name, size, sha256='b' * 64):
    return {'name': name, 'size': size, 'state': 'uploaded', 'digest': 'sha256:' + sha256}


def release(release_id, tag, *, draft=False, prerelease=False):
    return {'id': release_id, 'tag_name': tag, 'draft': draft, 'prerelease': prerelease, 'assets': [
        asset('approval.json', len(approval()), hashlib.sha256(approval()).hexdigest()),
        asset('approval.sig', 64), asset('review.json', 900),
        *(asset(row['archive'], row['size'], row['sha256']) for row in ARCHIVES)]}


class Prune(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        gh = self.root / 'gh'
        gh.write_text(f'#!{sys.executable}\n{FAKE_GH}', encoding='utf-8')
        gh.chmod(0o700)
        self.state_file = self.root / 'state.json'
        self.summary = self.root / 'summary.md'

    def arrange(self, *rows, fails=(), source='0.1.10'):
        self.state_file.write_text(json.dumps({
            'releases': rows, 'refs': [row['tag_name'] for row in rows if not row['draft']],
            'calls': [], 'fails': fails, 'source': source}), encoding='utf-8')

    def state(self):
        return json.loads(self.state_file.read_text(encoding='utf-8'))

    def writes(self):
        return [path for method, path in self.state()['calls'] if method != 'GET']

    def run_prune(self, *, dry_run=False, site_version='0.1.10', tag=CURRENT):
        def read_site(request, *, timeout):
            self.assertEqual(request.full_url, f'{STORE}/test/approval.json')
            return io.BytesIO(approval(site_version))

        argv = ['--repository', REPOSITORY, '--tag', tag]
        argv += ['--dry-run'] if dry_run else ['--source-manifest', MANIFEST, '--store-url', STORE]
        out, err = io.StringIO(), io.StringIO()
        env = {'PATH': str(self.root), 'FAKE_GH_STATE': str(self.state_file), 'GITHUB_STEP_SUMMARY': str(self.summary)}
        with (patch.dict(os.environ, env, clear=True), patch.object(prune, 'open_store', side_effect=read_site) as web,
              contextlib.redirect_stdout(out), contextlib.redirect_stderr(err)):
            result = prune.main(argv)
        return result, out.getvalue(), err.getvalue(), web.call_count

    def test_only_strictly_older_published_versions_of_this_extension_are_removed(self):
        older = ['extension/test/v0.1.9', 'extension/test/v0.0.99']
        kept = [CURRENT, 'extension/test/v0.1.11', 'extension/test-more/v0.0.1', 'v0.1.0-dev.1',
                'extension/test/0.1.0', 'extension/test/v0.01.0', 'extension/test/v0.1.0-rc.1']
        draft = 'extension/test/v0.0.1'
        rows = [release(1, older[0]), release(2, draft, draft=True), release(3, kept[0]),
                release(4, older[1], prerelease=True), *(release(i, tag) for i, tag in enumerate(kept[1:], 5))]
        self.arrange(*rows)
        code, out, err, reads = self.run_prune()
        self.assertEqual(code, 0, err)
        self.assertEqual(reads, 1)
        self.assertEqual([row['tag_name'] for row in self.state()['releases']], [draft, *kept])
        self.assertEqual(self.state()['refs'], kept)
        self.assertEqual(self.writes(), ['releases/4', f'git/refs/tags/{older[1]}',
                                         'releases/1', f'git/refs/tags/{older[0]}'])
        for tag in older:
            self.assertIn(f'Removed release and tag `{tag}`', self.summary.read_text())
        for tag in kept[3:]:
            self.assertIn(tag, err)

    def test_draft_sharing_an_old_tag_blocks_only_that_deletion(self):
        shared, other = 'extension/test/v0.1.8', 'extension/test/v0.1.9'
        self.arrange(release(1, shared), release(2, shared, draft=True), release(3, other), release(4, CURRENT))
        code, _, err, _ = self.run_prune()
        self.assertEqual(code, 1)
        self.assertIn(f'Could not remove release `{shared}`: a draft also uses this tag', err)
        self.assertEqual(self.writes(), ['releases/3', f'git/refs/tags/{other}'])
        self.assertEqual(self.state()['refs'], [shared, CURRENT])

    def test_dry_run_lists_candidates_even_before_cutover_and_writes_nothing(self):
        old = 'extension/test/v0.1.9'
        self.arrange(release(1, old))
        code, out, err, reads = self.run_prune(dry_run=True)
        self.assertEqual(code, 0, err)
        self.assertIn(f'Would remove release and tag `{old}`', out)
        self.assertEqual(reads, 0)
        self.assertEqual(self.state()['calls'], [['GET', 'releases?per_page=100']])

    def test_missing_unpublished_or_incomplete_replacement_refuses_all_deletions(self):
        incomplete = release(2, CURRENT)
        incomplete['assets'].pop(1)
        for label, rows in [('missing', []), ('draft', [release(2, CURRENT, draft=True)]),
                            ('prerelease', [release(2, CURRENT, prerelease=True)]), ('incomplete', [incomplete])]:
            with self.subTest(label=label):
                self.arrange(release(1, 'extension/test/v0.1.9'), *rows)
                code, _, err, reads = self.run_prune()
                self.assertEqual(code, 1)
                self.assertIn(CURRENT, err)
                self.assertEqual(reads, 0)
                self.assertEqual(self.writes(), [])

    def test_stale_branch_manifest_and_stale_deployed_approval_refuse_all_deletions(self):
        for source, site, expected, reads in [('0.1.9', '0.1.10', 'extension branch manifest', 0),
                                                ('0.1.10', '0.1.9', 'published store approval', 1)]:
            with self.subTest(reader=expected):
                self.arrange(release(1, 'extension/test/v0.1.9'), release(2, CURRENT), source=source)
                code, _, err, site_reads = self.run_prune(site_version=site)
                self.assertEqual(code, 1)
                self.assertIn(expected, err)
                self.assertIn('0.1.9', err)
                self.assertIn(CURRENT, err)
                self.assertEqual(self.writes(), [])
                self.assertEqual(site_reads, reads)

    def test_missing_signed_archive_refuses_all_deletions(self):
        incomplete = release(2, CURRENT)
        incomplete['assets'].pop()
        self.arrange(release(1, 'extension/test/v0.1.9'), incomplete)
        code, _, err, reads = self.run_prune()
        self.assertEqual(code, 1)
        self.assertIn(ARCHIVES[-1]['archive'], err)
        self.assertEqual(self.writes(), [])
        self.assertEqual(reads, 1)

    def test_failures_name_the_unremoved_release_or_tag_and_continue(self):
        first, second, third = (f'extension/test/v0.1.{i}' for i in range(1, 4))
        self.arrange(release(1, first), release(2, second), release(3, third), release(4, CURRENT),
                     fails=['releases/1', f'git/refs/tags/{second}'])
        code, _, err, _ = self.run_prune()
        self.assertEqual(code, 1)
        self.assertIn(f'Could not remove release `{first}`', err)
        self.assertIn(f'Could not remove tag `{second}`', err)
        self.assertIn('HTTP 403', err)
        self.assertEqual([row['tag_name'] for row in self.state()['releases']], [first, CURRENT])
        self.assertEqual(self.state()['refs'], [first, second, CURRENT])
        summary = self.summary.read_text()
        self.assertIn(f'Removed release `{second}`; tag removal failed', summary)
        self.assertIn(f'Removed release and tag `{third}`', summary)

    def test_a_second_run_removes_nothing_and_succeeds(self):
        self.arrange(release(1, 'extension/test/v0.1.9'), release(2, CURRENT))
        self.assertEqual(self.run_prune()[0], 0)
        before = self.writes()
        code, out, err, reads = self.run_prune()
        self.assertEqual(code, 0, err)
        self.assertIn('No older published releases', out)
        self.assertEqual(self.writes(), before)
        self.assertEqual(reads, 0)


if __name__ == '__main__':
    unittest.main(verbosity=2)
