#!/usr/bin/env python3
"""Exercise trusted admission policy without credentials or external API writes."""
import base64
import json
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('admission', Path(__file__).with_name('verify-extension-admission.py'))
admission = importlib.util.module_from_spec(spec)
spec.loader.exec_module(admission)


class Admission(unittest.TestCase):
    def fixture(self):
        sha = 'a' * 40
        return {
            'actions/runs/123': {'event': 'pull_request', 'conclusion': 'success', 'status': 'completed',
                                'head_sha': sha, 'path': '.github/workflows/extension-validate.yml',
                                'pull_requests': [{'number': 45}]},
            'pulls/45': {'base': {'ref': 'extension'}, 'head': {'sha': sha}, 'state': 'open',
                         'user': {'login': 'contributor', 'id': 10, 'type': 'User'}, 'changed_files': 1},
            'pulls/45/files?per_page=100&page=1': [{'filename': 'extension/peersus-upload/src/lib.rs'}],
            'pulls/45/reviews?per_page=100&page=1': [
                {'state': 'APPROVED', 'commit_id': sha, 'user': {'login': 'maintainer', 'type': 'User'}}],
            'collaborators/maintainer/permission': {'permission': 'write'},
            'git/ref/heads/extension': {'object': {'sha': 'b' * 40}},
            'contents/extension?ref=' + 'b' * 40: [{'name': 'peersus-upload', 'type': 'dir'}],
            'contents/extension/peersus-upload/ownership.json?ref=' + 'a' * 40: self.ownership(),
            'contents/extension/peersus-upload/ownership.json?ref=' + 'b' * 40: self.ownership(),
            'users/contributor': {'login': 'contributor', 'id': 10, 'type': 'User'},
        }

    def ownership(self):
        data = {'schema': 1, 'owner': {'login': 'contributor', 'id': 10}, 'contributors': []}
        return {'type': 'file', 'encoding': 'base64',
                'content': base64.b64encode(json.dumps(data).encode()).decode()}

    def test_maintainer_approval_cannot_bypass_plugin_ownership_on_updates(self):
        data = self.fixture()
        data['pulls/45']['user'] = {'login': 'outsider', 'id': 99, 'type': 'User'}
        with self.assertRaises(ValueError):
            self.check(data)

    def check(self, data):
        return admission.check(lambda path: copy.deepcopy(data[path]), 123, 45, 'a' * 40)

    def test_exact_successful_source_run_and_independent_maintainer_can_admit(self):
        self.assertEqual(self.check(self.fixture()), {'directory': 'extension/peersus-upload', 'id': 'upload'})

    def test_green_run_cannot_hide_modified_workflow_or_cross_plugin_changes(self):
        for filename, previous in [('.github/workflows/extension-validate.yml', None),
                                   ('extension/other/src/lib.rs', 'extension/peersus-upload/src/lib.rs'),
                                   ('extension/peersus-upload/script.py', '.github/workflows/ci.yml')]:
            with self.subTest(filename=filename, previous=previous):
                data = self.fixture()
                file = {'filename': filename}
                if previous:
                    file['previous_filename'] = previous
                data['pulls/45/files?per_page=100&page=1'] = [file]
                with self.assertRaises(ValueError):
                    self.check(data)

    def test_stale_approval_bots_self_approval_and_dismissed_approval_cannot_admit(self):
        for change in ('stale', 'bot', 'self', 'dismissed', 'read-only', 'head-moved', 'failed', 'truncated-files'):
            with self.subTest(change=change):
                data = self.fixture()
                review = data['pulls/45/reviews?per_page=100&page=1'][0]
                if change == 'stale':
                    review['commit_id'] = 'b' * 40
                elif change == 'bot':
                    review['user']['type'] = 'Bot'
                elif change == 'self':
                    data['pulls/45']['user']['login'] = 'maintainer'
                elif change == 'dismissed':
                    data['pulls/45/reviews?per_page=100&page=1'].append({**review, 'state': 'DISMISSED'})
                elif change == 'read-only':
                    data['collaborators/maintainer/permission']['permission'] = 'read'
                elif change == 'head-moved':
                    data['pulls/45']['head']['sha'] = 'b' * 40
                elif change == 'failed':
                    data['actions/runs/123']['conclusion'] = 'failure'
                else:
                    data['pulls/45']['changed_files'] = 2
                with self.assertRaises(ValueError):
                    self.check(data)


if __name__ == '__main__':
    unittest.main(verbosity=2)
