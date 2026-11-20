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

    def test_both_branches_are_scoped_and_only_fixed_labels_are_written(self):
        for branch, scope in [('main', 'scope:application'), ('extension', 'scope:extension')]:
            with self.subTest(branch=branch):
                labels, calls = self.run_case(branch)
                self.assertEqual(labels, [scope], 'no maintainer is asked before reviewers and CI are done')
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

    def test_reconciliation_covers_all_open_prs_despite_superseded_events(self):
        calls = []
        def request(method, path):
            calls.append(path)
            if path.endswith('&page=1'):
                return [{'number': i, 'base': {'ref': 'extension'}} for i in range(1, 101)]
            return [{'number': 101, 'base': {'ref': 'main'}},
                    {'number': 102, 'base': {'ref': 'unrelated'}}]
        self.assertEqual(TRIAGE.open_prs(request, 'example/project'), list(range(1, 102)))
        self.assertEqual(len(calls), 2)

    def test_reconciliation_refuses_truncated_coverage(self):
        def request(method, path):
            return [{'number': i, 'base': {'ref': 'extension'}} for i in range(1, 101)]
        with self.assertRaisesRegex(ValueError, 'coverage'):
            TRIAGE.open_prs(request, 'example/project')

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
        earlier = {'id': 1, 'name': 'build', 'status': 'completed', 'conclusion': 'failure'}
        later = {**earlier, 'id': 2, 'conclusion': 'success'}
        self.assertEqual(TRIAGE.ci_state([later, earlier], []), 'passed', 'a rerun supersedes its failure')
        self.assertEqual(TRIAGE.ci_state([{**later, 'conclusion': 'failure'}, {**earlier, 'conclusion': 'success'}], []),
                         'failed', 'a newer failure is not hidden by an older success')
        self.assertEqual(TRIAGE.ci_state([success, earlier, {**later, 'conclusion': 'skipped'}], []),
                         'failed', 'an unrelated label event that skips the build cannot clear its failure')

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

    def maintainer_case(self, conclusion, reviewed, unresolved=False):
        import base64
        import json
        sha = 'a' * 40
        pull = {'number': 7, 'state': 'open', 'base': {'ref': 'extension'}, 'user': {},
                'head': {'sha': sha}, 'changed_files': 1, 'labels': [{'name': 'review:maintainer'}]}
        calls = []
        def request(method, path, body=None):
            calls.append((method, path, body))
            if path == '/graphql':
                return {'data': {'repository': {'pullRequest': {'reviewThreads': {
                    'nodes': [{'isResolved': not unresolved, 'isOutdated': False}],
                    'pageInfo': {'hasNextPage': False}}}}}}
            if method != 'GET':
                return {}
            if path.endswith('/pulls/7'):
                return pull
            if '/reviews?' in path:
                return ([{'user': {'login': login}, 'commit_id': sha} for login in reviewed]
                        if path.endswith('page=1') else [])
            if '/files?' in path:
                return [{'filename': 'extension/fly-me/src/lib.rs'}]
            if '/contents/' in path:
                return {'type': 'file', 'encoding': 'base64', 'content': base64.b64encode(json.dumps({
                    'platforms': ['linux'], 'execution': {'kind': 'wasm'}}).encode()).decode()}
            if '/check-runs?' in path:
                return {'total_count': 1, 'check_runs': [
                    {'name': 'build', 'status': 'completed', 'conclusion': conclusion}]}
            if '/status?' in path:
                return {'total_count': 0, 'statuses': []}
            raise AssertionError(path)
        return TRIAGE.apply(request, 'example/project', 7), calls

    def test_maintainer_is_asked_only_after_reviewers_and_ci_pass_and_unasked_otherwise(self):
        everyone = ['devin-ai-integration[bot]']
        labels, _ = self.maintainer_case('success', everyone)
        self.assertIn('review:maintainer', labels)
        others = ['coderabbitai[bot]', 'greptile-apps[bot]']
        for conclusion, reviewed, unresolved in [('failure', everyone, False), ('success', others, False),
                                                 ('success', everyone, True)]:
            with self.subTest(conclusion=conclusion, reviewed=len(reviewed), unresolved=unresolved):
                labels, calls = self.maintainer_case(conclusion, reviewed, unresolved)
                self.assertNotIn('review:maintainer', labels)
                self.assertIn(('DELETE', '/repos/example/project/issues/7/labels/review%3Amaintainer', None), calls)


if __name__ == '__main__':
    unittest.main(verbosity=2)
