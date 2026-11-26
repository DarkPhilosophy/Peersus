#!/usr/bin/env python3
"""Check the synchronizer against real repositories: what it plans, what it sends, what it refuses."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('sync_github', Path(__file__).with_name('sync-github.py'))
SYNC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SYNC)
BRANCH = 'submission/demo-20260101'
IDENTITY = {'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
            'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}


class Synchronizer(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        # A runner without a configured identity gets the fixture's; a person's own is left as it is.
        if subprocess.run(['git', 'config', 'user.name'], capture_output=True).returncode:
            environment = patch.dict(os.environ, IDENTITY)
            environment.start()
            self.addCleanup(environment.stop)
        # The fixture has no signing key, and a push of unsigned commits is not something every
        # machine permits: the one `git push` is delivered by hand under the same lease instead.
        for name, value in (('SIGN', ()), ('git', self.deliver)):
            stub = patch.object(SYNC, name, value)
            stub.start()
            self.addCleanup(stub.stop)
        self.root = Path(temp.name)
        # What GitHub holds: main, and the submission branch with the extension beside another directory.
        self.hub = self.root / 'hub'
        self.git(self.root, 'init', '--quiet', '--initial-branch=main', str(self.hub))
        self.put('.gitignore', '/plugins/\n', self.hub)
        self.put('README.md', 'application\n', self.hub)
        self.commit(self.hub, 'application')
        self.git(self.hub, 'checkout', '--quiet', '--orphan', BRANCH)
        self.git(self.hub, 'rm', '--quiet', '-rf', '.')
        self.put('extension/peersus-demo/plugin.json', '{"version":1}\n', self.hub)
        self.put('extension/peersus-demo/src/lib.rs', 'pub fn old() {}\n', self.hub)
        self.put('extension/peersus-demo/src/gone.rs', 'pub fn gone() {}\n', self.hub)
        self.put('extension/peersus-else/src/lib.rs', 'pub fn untouched() {}\n', self.hub)
        self.put('README.md', 'branch readme\n', self.hub)
        self.commit(self.hub, 'submission')
        self.git(self.hub, 'checkout', '--quiet', '--detach')
        # The application checkout with the working copy: git-ignored, with build output that never belongs on the branch.
        self.repo = self.root / 'app'
        self.git(self.root, 'clone', '--quiet', '--branch', 'main', str(self.hub), str(self.repo))
        self.put('plugins/peersus-demo/plugin.json', '{"version":1}\n')
        self.put('plugins/peersus-demo/src/lib.rs', 'pub fn new() {}\n')
        self.put('plugins/peersus-demo/src/added.rs', 'pub fn added() {}\n')
        self.put('plugins/peersus-demo/target/debug/out.bin', 'built\n')

    def git(self, where, *args):
        # The fixture's own commits are unsigned whatever the person's configuration asks for.
        return subprocess.run(['git', '-c', 'commit.gpgsign=false', '-C', str(where), *args],
                              check=True, capture_output=True, text=True).stdout.strip()

    def deliver(self, repo, *args, **options):
        if args[0] != 'push':
            return GIT(repo, *args, **options)
        lease, remote, refspec = args[1:]
        self.assertEqual(remote, 'origin')
        self.assertTrue(lease.startswith('--force-with-lease=refs/heads/'))
        ref, old = lease.removeprefix('--force-with-lease=').rsplit(':', 1)
        new, destination = refspec.split(':', 1)
        self.assertEqual(destination, ref)
        self.git(repo, 'update-ref', 'refs/delivery/tip', new)
        self.git(self.hub, 'fetch', '--quiet', str(repo), 'refs/delivery/tip')
        self.git(repo, 'update-ref', '-d', 'refs/delivery/tip')
        try:
            self.git(self.hub, 'update-ref', ref, new, old or '0' * 40)
        except subprocess.CalledProcessError as error:
            raise SYNC.Failure('Git operation failed: push') from error
        return ''

    def put(self, name, value, where=None):
        file = (where or self.repo) / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(value)
        return file

    def commit(self, where, message):
        self.git(where, 'add', '-A')
        self.git(where, 'commit', '--quiet', '-m', message)

    def hub_commit(self, branch, name, value, message):
        self.git(self.hub, 'checkout', '--quiet', branch)
        self.put(name, value, self.hub)
        self.commit(self.hub, message)
        self.git(self.hub, 'checkout', '--quiet', '--detach')

    def head(self, branch):
        return self.git(self.hub, 'rev-parse', 'refs/heads/' + branch)

    def files(self, branch):
        return dict(line.split('\t')[::-1] for line in
                    self.git(self.hub, 'ls-tree', '-r', '--format=%(objectname)\t%(path)', branch).splitlines())

    def heads(self):
        return dict(line.split() for line in
                    self.git(self.hub, 'for-each-ref', '--format=%(refname) %(objectname)', 'refs/heads').splitlines())

    def run_sync(self, publish=False, targets=('extensions',), messages=None):
        said = []
        done = SYNC.synchronize(self.repo, 'origin', targets, publish, messages or {}, say=said.append)
        return done, said

    def test_a_plan_writes_nothing_and_names_what_differs(self):
        before = self.heads()
        objects = self.git(self.repo, 'count-objects', '-v')
        done, said = self.run_sync()
        self.assertFalse(done)
        self.assertEqual(said[0], BRANCH + ': 3 files of peersus-demo differ from plugins/peersus-demo')
        self.assertEqual(self.heads(), before)
        self.assertEqual(self.git(self.repo, 'count-objects', '-v'), objects)

    def test_publishing_replaces_only_the_extensions_directory_with_one_commit_on_the_tip(self):
        before = self.head(BRANCH)
        untouched = {path: blob for path, blob in self.files(BRANCH).items()
                     if not path.startswith('extension/peersus-demo/')}
        self.run_sync(publish=True, messages={'demo': 'Demo: what changed'})
        after = self.head(BRANCH)
        self.assertEqual(self.git(self.hub, 'rev-list', '--parents', '-1', after), after + ' ' + before)
        self.assertEqual(self.git(self.hub, 'log', '-1', '--format=%s', after), 'Demo: what changed')
        files = self.files(BRANCH)
        self.assertEqual({path for path in files if path.startswith('extension/peersus-demo/')},
                         {'extension/peersus-demo/plugin.json', 'extension/peersus-demo/src/lib.rs',
                          'extension/peersus-demo/src/added.rs'})
        self.assertEqual(self.git(self.hub, 'show', BRANCH + ':extension/peersus-demo/src/lib.rs'), 'pub fn new() {}')
        self.assertEqual({path: blob for path, blob in files.items()
                          if not path.startswith('extension/peersus-demo/')}, untouched)
        # The checkout that ran it is as it was, and a second run has nothing left to do.
        self.assertEqual(self.git(self.repo, 'status', '--porcelain'), '')
        self.assertEqual(self.git(self.repo, 'rev-parse', '--abbrev-ref', 'HEAD'), 'main')
        done, said = self.run_sync()
        self.assertTrue(done)
        self.assertEqual(said[0], BRANCH + ': in sync')

    def test_the_default_message_names_the_extension(self):
        self.run_sync(publish=True)
        self.assertEqual(self.git(self.hub, 'log', '-1', '--format=%s', BRANCH),
                         'Demo: the source as it is in the development copy')

    def test_an_executable_source_file_stays_executable(self):
        self.put('plugins/peersus-demo/build.sh', '#!/bin/sh\n').chmod(0o755)
        self.run_sync(publish=True)
        self.assertTrue(self.git(self.hub, 'ls-tree', BRANCH, 'extension/peersus-demo/build.sh').startswith('100755 '))

    def test_a_credential_like_file_stops_everything_before_any_write(self):
        self.put('plugins/peersus-demo/api_key.txt', 'not for publication\n')
        before = self.heads()
        with self.assertRaisesRegex(SYNC.Failure, 'credential-like'):
            self.run_sync(publish=True)
        self.assertEqual(self.heads(), before)

    def test_a_branch_that_moved_after_planning_is_not_overwritten(self):
        refs = SYNC.DEPLOY.remote_refs(self.repo, 'origin')
        prepared, _ = SYNC.plan_submission(self.repo, 'origin', refs, 'refs/heads/' + BRANCH, 'peersus-demo', write=True)
        self.hub_commit(BRANCH, 'extension/peersus-demo/src/lib.rs', 'pub fn someone_else() {}\n', 'a reviewer pushed meanwhile')
        moved = self.head(BRANCH)
        with self.assertRaisesRegex(SYNC.Failure, 'moved after planning'):
            SYNC.publish_submission(self.repo, 'origin', prepared, 'Demo: late')
        self.assertEqual(self.head(BRANCH), moved)

    def test_the_lease_itself_refuses_a_branch_that_moved_between_the_check_and_the_push(self):
        old = self.head(BRANCH)
        self.git(self.repo, 'fetch', '--quiet', 'origin', BRANCH)
        new = self.git(self.repo, 'commit-tree', old + '^{tree}', '-p', old, '-m', 'Demo: ours')
        self.hub_commit(BRANCH, 'README.md', 'moved\n', 'theirs')
        moved = self.head(BRANCH)
        with self.assertRaisesRegex(SYNC.Failure, 'push'):
            SYNC.push(self.repo, 'origin', 'refs/heads/' + BRANCH, old, new)
        self.assertEqual(self.head(BRANCH), moved)

    def test_local_commits_of_the_branch_are_kept_under_the_new_one(self):
        self.git(self.repo, 'fetch', '--quiet', 'origin', BRANCH + ':' + BRANCH)
        tree = self.git(self.repo, 'rev-parse', BRANCH + '^{tree}')
        local = self.git(self.repo, 'commit-tree', tree, '-p', BRANCH, '-m', 'Demo: an earlier local step')
        self.git(self.repo, 'update-ref', 'refs/heads/' + BRANCH, local)
        self.run_sync(publish=True)
        self.assertEqual(self.git(self.hub, 'rev-parse', BRANCH + '^'), local)

    def test_a_local_branch_that_rewrote_the_remote_one_is_refused(self):
        self.git(self.repo, 'fetch', '--quiet', 'origin', BRANCH + ':' + BRANCH)
        tree = self.git(self.repo, 'rev-parse', BRANCH + '^{tree}')
        self.git(self.repo, 'update-ref', 'refs/heads/' + BRANCH, self.git(self.repo, 'commit-tree', tree, '-m', 'unrelated'))
        before = self.heads()
        with self.assertRaisesRegex(SYNC.Failure, 'differs locally and on the remote'):
            self.run_sync(publish=True)
        self.assertEqual(self.heads(), before)

    def test_a_branch_without_local_source_or_of_another_kind_is_never_touched(self):
        self.git(self.hub, 'branch', 'submission/absent-20260101', BRANCH)
        self.git(self.hub, 'branch', 'feature/peersus-demo', BRANCH)
        before = self.heads()
        _, said = self.run_sync(publish=True, targets=SYNC.TARGETS)
        self.assertFalse(any('absent' in line or 'feature' in line for line in said))
        after = self.heads()
        self.assertNotEqual(after.pop('refs/heads/' + BRANCH), before.pop('refs/heads/' + BRANCH))
        self.assertEqual(after, before)

    def test_two_branches_for_one_extension_are_refused(self):
        self.git(self.hub, 'branch', 'submission/demo-20260202', BRANCH)
        with self.assertRaisesRegex(SYNC.Failure, 'More than one submission branch exists for peersus-demo'):
            self.run_sync(publish=True)

    def test_a_message_for_an_extension_without_a_branch_is_refused(self):
        before = self.heads()
        with self.assertRaisesRegex(SYNC.Failure, 'No submission branch with local source for: typo'):
            self.run_sync(publish=True, messages={'typo': 'Typo: text'})
        self.assertEqual(self.heads(), before)

    def test_main_in_sync_is_said_and_tracked_changes_are_refused(self):
        done, said = self.run_sync(targets=('main',))
        self.assertTrue(done)
        self.assertEqual(said[0], 'main: in sync')
        self.put('README.md', 'application, edited\n')
        with self.assertRaisesRegex(SYNC.Failure, 'Commit tracked changes'):
            self.run_sync(targets=('main',))

    def test_main_ahead_is_planned_as_the_snapshot_of_the_committed_tree(self):
        self.put('README.md', 'application, second\n')
        self.commit(self.repo, 'second')
        done, said = self.run_sync(targets=('main',))
        self.assertFalse(done)
        self.assertRegex(said[0], r'^main: publish the committed tree [0-9a-f]{12} as a fresh signed root; '
                                  r'previous tip kept as backup/main-[0-9a-f]{7}$')

    def test_languages_fast_forward_only_and_never_with_uncommitted_files(self):
        checkout = self.root / 'languages'
        self.git(self.repo, 'worktree', 'add', '--quiet', '--orphan', '-b', 'languages', str(checkout))
        self.put('languages/0.1.0/de.po', 'msgid "a"\nmsgstr "b"\n', checkout)
        self.commit(checkout, 'languages: first')
        done, said = self.run_sync(targets=('languages',))
        self.assertFalse(done)
        self.assertEqual(said[0], 'languages: push 1 signed commit')
        self.run_sync(publish=True, targets=('languages',))
        self.assertEqual(self.head('languages'), self.git(checkout, 'rev-parse', 'HEAD'))
        self.assertEqual(self.run_sync(targets=('languages',))[1][0], 'languages: in sync')
        self.put('languages/0.1.0/de.po', 'msgid "a"\nmsgstr "c"\n', checkout)
        with self.assertRaisesRegex(SYNC.Failure, 'uncommitted changes'):
            self.run_sync(publish=True, targets=('languages',))
        # A local branch that rewrote what the remote holds is refused, not forced.
        self.git(checkout, 'commit', '--quiet', '--amend', '-am', 'languages: rewritten')
        published = self.head('languages')
        with self.assertRaisesRegex(SYNC.Failure, 'does not continue the remote one'):
            self.run_sync(publish=True, targets=('languages',))
        self.assertEqual(self.head('languages'), published)


GIT = SYNC.git

if __name__ == '__main__':
    unittest.main()
