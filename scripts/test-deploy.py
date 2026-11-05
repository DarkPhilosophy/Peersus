#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('deploy', Path(__file__).with_name('deploy.py'))
DEPLOY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DEPLOY)
OLD = 'a' * 40
NEW = 'b' * 40
TREE = 'c' * 40


class Publication(unittest.TestCase):
    def prepared(self, mode='dev', fresh=True):
        return {'source': OLD, 'tree': TREE, 'branch': 'main', 'old': OLD,
                'fresh': fresh, 'backup': 'refs/heads/backup/main-' + OLD if fresh else None,
                'version': 'v1.2.3' if mode == 'release' else None, 'mode': mode}

    def test_development_moves_off_main_after_first_release(self):
        self.assertEqual(DEPLOY.destination('dev', False), 'main')
        self.assertEqual(DEPLOY.destination('dev', True), 'dev')
        with self.assertRaises(DEPLOY.Failure):
            DEPLOY.destination('dev', True, 'main')
        with self.assertRaises(DEPLOY.Failure):
            DEPLOY.destination('release', True, 'other')

    def test_destination_movement_causes_no_commit_or_push(self):
        with patch.object(DEPLOY, 'remote_refs', return_value={'refs/heads/main': NEW}), \
                patch.object(DEPLOY, 'git') as git:
            with self.assertRaisesRegex(DEPLOY.Failure, 'moved'):
                DEPLOY.publish(Path('.'), 'origin', self.prepared())
            git.assert_not_called()

    def run_publish(self, mode='dev', fresh=True, fail_push=False):
        calls = []
        def git(repo, *args):
            calls.append(args)
            if args[:2] == ('rev-parse', 'FETCH_HEAD'):
                return OLD
            if args[0] == 'commit-tree':
                return NEW
            if args[0] == 'push' and fail_push:
                raise DEPLOY.Failure('simulated atomic rejection')
            return ''
        after = {'refs/heads/main': NEW}
        if mode == 'release':
            after['refs/tags/v1.2.3^{}'] = NEW
        with patch.object(DEPLOY, 'remote_refs', side_effect=[{'refs/heads/main': OLD}, after]), \
                patch.object(DEPLOY, 'git', side_effect=git):
            if fail_push:
                with self.assertRaisesRegex(DEPLOY.Failure, 'atomic rejection'):
                    DEPLOY.publish(Path('.'), 'origin', self.prepared(mode, fresh))
            else:
                self.assertEqual(DEPLOY.publish(Path('.'), 'origin', self.prepared(mode, fresh)), NEW)
        return calls

    def test_fresh_main_is_signed_root_and_backup_is_in_same_atomic_push(self):
        calls = self.run_publish()
        commit = next(args for args in calls if args[0] == 'commit-tree')
        self.assertIn('-S', commit)
        self.assertNotIn('-p', commit)
        self.assertIn(('verify-commit', NEW), calls)
        push = next(args for args in calls if args[0] == 'push')
        self.assertIn('--atomic', push)
        self.assertIn('--force-with-lease=refs/heads/main:' + OLD, push)
        self.assertIn(OLD + ':refs/heads/backup/main-' + OLD, push)
        self.assertIn(('fetch', '--no-tags', 'origin', 'refs/heads/main'), calls)

    def test_later_release_preserves_parent_and_signs_tag(self):
        calls = self.run_publish('release', False)
        commit = next(args for args in calls if args[0] == 'commit-tree')
        self.assertEqual(commit[commit.index('-p') + 1], OLD)
        self.assertTrue(any(args[:2] == ('tag', '-s') for args in calls))
        self.assertTrue(any(args[0] == 'verify-tag' for args in calls))
        self.assertTrue(any(args[:2] == ('tag', '-d') for args in calls))
        push = next(args for args in calls if args[0] == 'push')
        self.assertIn('--force-with-lease=refs/tags/v1.2.3:', push)

    def test_rejected_release_cleans_only_its_temporary_local_tag(self):
        calls = self.run_publish('release', False, True)
        staged = next(args[2] for args in calls if args[:2] == ('tag', '-s'))
        self.assertIn(('tag', '-d', staged), calls)
        self.assertNotIn(('tag', '-d', 'v1.2.3'), calls)

    def test_signing_failure_never_falls_back_or_pushes(self):
        calls = []
        def git(repo, *args):
            calls.append(args)
            if args[0] == 'commit-tree':
                raise DEPLOY.Failure('signing unavailable')
            if args[:2] == ('rev-parse', 'FETCH_HEAD'):
                return OLD
            return ''
        with patch.object(DEPLOY, 'remote_refs', return_value={'refs/heads/main': OLD}), \
                patch.object(DEPLOY, 'git', side_effect=git):
            with self.assertRaisesRegex(DEPLOY.Failure, 'signing unavailable'):
                DEPLOY.publish(Path('.'), 'origin', self.prepared())
        self.assertEqual(sum(args[0] == 'commit-tree' for args in calls), 1)
        self.assertFalse(any(args[0] == 'push' for args in calls))

    def test_real_committed_source_plan_is_read_only_and_checks_release_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = root / 'source'
            remote = root / 'remote.git'
            repo.mkdir()
            def run(*args):
                return subprocess.run(['git', '-C', str(repo), *args], check=True,
                                      capture_output=True, text=True).stdout.strip()
            run('init', '--initial-branch=main')
            subprocess.run(['git', 'init', '--bare', str(remote)], check=True, capture_output=True)
            (repo / 'Cargo.toml').write_text('[workspace.package]\nversion = "1.2.3"\n')
            run('add', '--', 'Cargo.toml')
            run('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                '-c', 'commit.gpgsign=false', 'commit', '-m', 'fixture')
            run('remote', 'add', 'origin', str(remote))
            run('push', 'origin', 'main')
            before = run('rev-parse', 'HEAD')
            prepared = DEPLOY.plan(repo, 'dev', 'origin')
            self.assertTrue(prepared['fresh'])
            self.assertEqual(prepared['source'], before)
            self.assertEqual(run('rev-parse', 'HEAD'), before)
            self.assertEqual(run('status', '--porcelain'), '')
            refs_before = run('show-ref')
            import sys
            subprocess.run([sys.executable, str(Path(DEPLOY.__file__)), 'dev', '--repo', str(repo)],
                           check=True, capture_output=True, text=True)
            self.assertEqual(run('show-ref'), refs_before)
            self.assertEqual(run('status', '--porcelain'), '')
            self.assertEqual(DEPLOY.plan(repo, 'release', 'origin', version='v1.2.3')['version'], 'v1.2.3')
            with self.assertRaisesRegex(DEPLOY.Failure, 'workspace version'):
                DEPLOY.plan(repo, 'release', 'origin', version='v9.9.9')
            (repo / 'Cargo.toml').write_text('[workspace.package]\nversion = "2.0.0"\n')
            with self.assertRaisesRegex(DEPLOY.Failure, 'Commit tracked changes'):
                DEPLOY.plan(repo, 'dev', 'origin')


if __name__ == '__main__':
    unittest.main(verbosity=2)
