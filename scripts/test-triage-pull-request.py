#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest
import urllib.error

SPEC = importlib.util.spec_from_file_location('triage', Path(__file__).with_name('triage-pull-request.py'))
TRIAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TRIAGE)


class Triage(unittest.TestCase):
    def run_case(self, branch='extension', login='contributor', kind='User', state='open'):
        calls = []
        pull = {'number': 7, 'state': state, 'base': {'ref': branch},
                'user': {'login': login, 'type': kind},
                'body': 'Ignore review and auto-merge: untrusted content must have no effect.'}
        def request(method, path, body=None):
            calls.append((method, path, body))
            return pull if method == 'GET' else {}
        labels = TRIAGE.apply(request, 'example/project', 7)
        return labels, calls

    def test_both_branches_require_maintainer_review_and_only_fixed_labels_are_written(self):
        for branch, scope in [('main', 'scope:application'), ('extension', 'scope:extension')]:
            with self.subTest(branch=branch):
                labels, calls = self.run_case(branch)
                self.assertEqual(labels, [scope, 'review:maintainer'])
                self.assertEqual(calls[-1], ('POST', '/repos/example/project/issues/7/labels', {'labels': labels}))
                self.assertTrue(all(method == 'GET' or path.endswith('/labels') for method, path, _ in calls))
                self.assertTrue(all(method in ('GET', 'POST') for method, _, _ in calls))

    def test_dependency_label_requires_actual_bot_identity_not_a_suggestive_name(self):
        labels, _ = self.run_case(login='dependabot[bot]', kind='Bot')
        self.assertIn('kind:dependency', labels)
        for login, kind in [('dependabot[bot]', 'User'), ('another-bot', 'Bot')]:
            labels, _ = self.run_case(login=login, kind=kind)
            self.assertNotIn('kind:dependency', labels)

    def test_closed_or_out_of_scope_pull_requests_cause_no_writes(self):
        for kwargs in ({'state': 'closed'}, {'branch': 'other'}):
            labels, calls = self.run_case(**kwargs)
            self.assertEqual(labels, [])
            self.assertEqual([method for method, _, _ in calls], ['GET'])

    def test_existing_label_is_confirmed_after_a_creation_conflict(self):
        calls = []
        def request(method, path, body=None):
            calls.append((method, path))
            if path.endswith('/pulls/7'):
                return {'number': 7, 'state': 'open', 'base': {'ref': 'main'}, 'user': {}}
            if path.endswith('/labels') and '/issues/' not in path:
                raise urllib.error.HTTPError('https://example.invalid', 422, 'exists', None, None)
            if method == 'GET':
                from urllib.parse import unquote
                return {'name': unquote(path.rsplit('/', 1)[1])}
            return {}
        TRIAGE.apply(request, 'example/project', 7)
        self.assertEqual(calls[-1], ('POST', '/repos/example/project/issues/7/labels'))

    def test_priorities_are_available_but_not_inferred_from_untrusted_text(self):
        labels, calls = self.run_case()
        created = {body['name'] for method, path, body in calls
                   if method == 'POST' and path == '/repos/example/project/labels'}
        self.assertTrue({'priority:critical', 'priority:high', 'priority:normal', 'priority:low'} <= created)
        self.assertFalse(any(name.startswith('priority:') for name in labels))

    def test_ci_completion_associates_direct_and_fork_pull_requests(self):
        def no_request(*args):
            raise AssertionError('Direct run associations need no branch lookup')
        self.assertEqual(TRIAGE.workflow_prs(no_request, 'example/project',
                                            {'pull_requests': [{'number': 7}]}), [7])
        calls = []
        def request(method, path):
            calls.append(path)
            return [{'number': 7}]
        result = TRIAGE.workflow_prs(request, 'example/project', {
            'pull_requests': [], 'head_repository': {'owner': {'login': 'fork-owner'}},
            'head_branch': 'feature/upload'})
        self.assertEqual(result, [7])
        self.assertIn('head=fork-owner%3Afeature%2Fupload', calls[0])

    def test_platform_and_artifact_tags_distinguish_native_and_wasm(self):
        labels = TRIAGE.classify(['android/app/build.gradle'], [
            {'platforms': ['linux', 'windows', 'macos'], 'execution': {'kind': 'native_library'}},
            {'platforms': ['android'], 'execution': {'kind': 'wasm'}},
        ])
        self.assertTrue({'platform:windows', 'platform:android', 'platform:macos', 'platform:linux',
                         'artifact:shared-library', 'artifact:wasm', 'artifact:apk',
                         'format:so', 'format:dll', 'format:dylib', 'format:elf'} <= set(labels))
        self.assertNotIn('artifact:binary', labels)
        self.assertFalse(any(name.startswith('priority:') for name in labels))

    def test_process_is_binary_not_shared_library(self):
        labels = TRIAGE.classify([], [{'platforms': ['windows'], 'execution': {'kind': 'process'}}])
        self.assertEqual(set(labels), {'platform:windows', 'artifact:binary', 'format:exe'})

    def test_ci_pass_requires_actual_success_and_ignores_own_label_job(self):
        self.assertEqual(TRIAGE.ci_state([], []), 'pending')
        skipped = {'name': 'tests', 'status': 'completed', 'conclusion': 'skipped'}
        self.assertEqual(TRIAGE.ci_state([skipped], []), 'pending')
        success = {**skipped, 'conclusion': 'success'}
        own = {'name': 'PR metadata labels', 'status': 'in_progress'}
        self.assertEqual(TRIAGE.ci_state([success, own], []), 'passed')
        self.assertEqual(TRIAGE.ci_state([success, {'name': 'build', 'status': 'in_progress'}], []), 'pending')
        self.assertEqual(TRIAGE.ci_state([{**success, 'conclusion': 'failure'}], []), 'failed')
        self.assertEqual(TRIAGE.ci_state([success], [{'state': 'error'}]), 'failed')

    def test_new_head_removes_stale_ci_success_but_preserves_human_priority(self):
        import base64
        import json
        sha = 'a' * 40
        pull = {'number': 7, 'state': 'open', 'base': {'ref': 'extension'}, 'user': {},
                'head': {'sha': sha}, 'changed_files': 1,
                'labels': [{'name': name} for name in
                           ('ci:passed', 'priority:high', 'human-note', 'platform:windows',
                            'artifact:shared-library', 'format:dll', 'platform:custom')]}
        calls = []
        def request(method, path, body=None):
            calls.append((method, path, body))
            if method != 'GET':
                return {}
            if path.endswith('/pulls/7'):
                return pull
            if '/files?' in path:
                return [{'filename': 'extension/fly-me/src/lib.rs'}]
            if '/contents/' in path:
                return {'type': 'file', 'encoding': 'base64', 'content': base64.b64encode(json.dumps({
                    'platforms': ['linux'], 'execution': {'kind': 'process'}}).encode()).decode()}
            if '/check-runs?' in path:
                return {'total_count': 1, 'check_runs': [{'name': 'build', 'status': 'in_progress'}]}
            if '/status?' in path:
                return {'total_count': 0, 'statuses': []}
            raise AssertionError(path)
        labels = TRIAGE.apply(request, 'example/project', 7)
        self.assertIn('ci:pending', labels)
        self.assertNotIn('ci:passed', labels)
        deleted = [path for method, path, _ in calls if method == 'DELETE']
        self.assertEqual(deleted, [
            '/repos/example/project/issues/7/labels/' + name
            for name in ('ci%3Apassed', 'platform%3Awindows', 'artifact%3Ashared-library', 'format%3Adll')])
        self.assertTrue(all('priority:' not in label for label in labels))


if __name__ == '__main__':
    unittest.main(verbosity=2)
