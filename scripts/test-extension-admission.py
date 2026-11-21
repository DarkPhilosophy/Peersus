#!/usr/bin/env python3
"""Exercise trusted admission policy without credentials or external API writes."""
import base64
import json
import copy
import re
import importlib.util
from pathlib import Path
import tempfile
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

    def own(self):
        """An admin's own first submission with no other reviewer, and this admission run approved in its environment."""
        data = self.fixture()
        data['pulls/45']['user'] = {'login': 'maintainer', 'id': 20, 'type': 'User'}
        data['pulls/45/reviews?per_page=100&page=1'] = []
        data['collaborators/maintainer/permission'] = {'permission': 'admin'}
        data['users/maintainer'] = {'login': 'maintainer', 'id': 20, 'type': 'User'}
        owner = {'schema': 1, 'owner': {'login': 'maintainer', 'id': 20}, 'contributors': []}
        for ref in ('a' * 40, 'b' * 40):
            data['contents/extension/peersus-upload/ownership.json?ref=' + ref] = {
                'type': 'file', 'encoding': 'base64', 'content': base64.b64encode(json.dumps(owner).encode()).decode()}
        data['actions/runs/900/approvals'] = [
            {'state': 'approved', 'environments': [{'name': 'extension-approval'}],
             'user': {'login': 'maintainer', 'type': 'User'}}]
        return data

    def check_own(self, data, admission_run=900):
        return admission.check(lambda path: copy.deepcopy(data[path]), 123, 45, 'a' * 40, admission_run)

    def test_an_admins_own_submission_is_admitted_by_an_admins_approval_of_the_admission_run(self):
        self.assertEqual(self.check_own(self.own()), {'directory': 'extension/peersus-upload', 'id': 'upload'})

    def test_without_that_approval_an_admins_own_submission_is_not_admitted(self):
        for change in ('no-run', 'rejected', 'other-environment', 'approver-not-admin', 'approver-bot', 'author-not-admin'):
            with self.subTest(change=change):
                data = self.own()
                approval = data['actions/runs/900/approvals'][0]
                run = 900
                if change == 'no-run':
                    run = None
                elif change == 'rejected':
                    approval['state'] = 'rejected'
                elif change == 'other-environment':
                    approval['environments'] = [{'name': 'github-pages'}]
                elif change == 'approver-not-admin':
                    approval['user'] = {'login': 'helper', 'type': 'User'}
                    data['collaborators/helper/permission'] = {'permission': 'write'}
                elif change == 'approver-bot':
                    approval['user']['type'] = 'Bot'
                else:
                    data['collaborators/maintainer/permission'] = {'permission': 'write'}
                with self.assertRaises(ValueError):
                    self.check_own(data, run)

    def test_a_contributors_submission_still_needs_an_independent_review(self):
        # A contributor's PR with no maintainer review is not admitted by the environment approval alone.
        data = self.fixture()
        data['pulls/45/reviews?per_page=100&page=1'] = []
        data['actions/runs/900/approvals'] = self.own()['actions/runs/900/approvals']
        data['collaborators/contributor/permission'] = {'permission': 'read'}
        with self.assertRaises(ValueError):
            self.check_own(data)

    def test_the_summary_before_approval_needs_no_approval_but_still_refuses_what_could_not_be_admitted(self):
        data = self.own()
        del data['actions/runs/900/approvals']
        seen = admission.inspect(lambda path: copy.deepcopy(data[path]), 123, 45, 'a' * 40)
        self.assertEqual(seen['submission'], {'directory': 'extension/peersus-upload', 'id': 'upload'})
        # The same scope rule as the gate: a change outside the plugin's directory is refused before anyone is asked.
        data['pulls/45/files?per_page=100&page=1'] = [{'filename': '.github/workflows/ci.yml'}]
        with self.assertRaises(ValueError):
            admission.inspect(lambda path: copy.deepcopy(data[path]), 123, 45, 'a' * 40)

    def test_the_summary_says_what_is_admitted_and_shows_contributor_text_only_as_text(self):
        data = self.own()
        seen = admission.inspect(lambda path: copy.deepcopy(data[path]), 123, 45, 'a' * 40)
        seen['pr']['title'] = 'Update [click here](https://evil.example) <img src=x>'
        seen['pr']['number'] = 45
        review = {'id': 'upload', 'commit': 'a' * 40, 'packages': [{
            'archive': 'upload-linux.tar.gz', 'archive_sha256': 'c' * 64, 'archive_size': 2048, 'platforms': ['linux'],
            'architecture': 'any', 'execution': {'kind': 'wasm'}, 'version': '1.2.0', 'author': 'Someone',
            'description': 'Uploads\nfiles **now**', 'capabilities': ['upload.send']}]}
        evidence = {'verdict': 'PASS', 'reason': 'all scheduled metrics are within limits', 'agreement': 'changed'}
        text = admission.summary(seen, review, 'd' * 64, '1.1.9', evidence)
        self.assertIn('`upload@1.2.0`', text)
        self.assertIn('`1.1.9` → `1.2.0`', text)
        self.assertIn('**PASS**', text)
        self.assertIn('| Licence and notice | Changed |', text)
        self.assertIn('`' + 'c' * 64 + '`', text)
        self.assertIn('`upload.send`', text)
        self.assertIn('extension/upload/v1.2.0', text)
        # No link, tag or line break of the contributor's survives as one.
        self.assertNotIn('](https://evil.example)', text)
        self.assertIsNone(re.search(r'(?<!\\)<', text), 'an HTML tag of the contributor survived')
        self.assertIn('Uploads files \\*\\*now\\*\\*', text)

    def test_benchmark_evidence_must_match_the_exact_review_and_current_store_version(self):
        review = {'id': 'upload', 'commit': 'a' * 40, 'packages': [{'version': '1.2.0'}]}
        receipt = {'schema': 1, 'id': 'upload', 'version': '1.2.0', 'commit': 'a' * 40,
                   'previous': '1.1.9', 'review_sha256': 'd' * 64}
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / 'impact.md').write_text('## Resource impact: PASS — all scheduled metrics are within limits\n')
            (root / 'extension-input.json').write_text(json.dumps(receipt))
            sample = {'schema': 3, 'workload': {'profile': 'extension-startup-idle'}, 'agreement_sha256': 'f' * 64}
            for side in ('baseline', 'change'):
                for index in range(1, 4):
                    (root / f'extension-{side}-{index}.json').write_text(json.dumps(sample))
            evidence, fingerprint = admission.benchmark_evidence(root, review, 'd' * 64, '1.1.9')
            self.assertEqual(evidence['verdict'], 'PASS')
            self.assertEqual(evidence['agreement'], 'unchanged')
            self.assertRegex(fingerprint, r'^[0-9a-f]{64}$')
            with self.assertRaises(ValueError):
                admission.benchmark_evidence(root, review, 'd' * 64, '1.1.8')
            with self.assertRaises(ValueError):
                admission.benchmark_evidence(root, review, 'e' * 64, '1.1.9')
            for index in range(1, 4):
                (root / f'extension-baseline-{index}.json').write_text(json.dumps({**sample, 'agreement_sha256': 'a' * 64}))
            updated, updated_fingerprint = admission.benchmark_evidence(root, review, 'd' * 64, '1.1.9')
            self.assertEqual(updated['agreement'], 'changed')
            self.assertNotEqual(fingerprint, updated_fingerprint)
            (root / 'extension-change-2.json').unlink()
            with self.assertRaises(FileNotFoundError):
                admission.benchmark_evidence(root, review, 'd' * 64, '1.1.9')
            (root / 'impact.md').write_text('## Resource impact: INCONCLUSIVE — missing sample\n')
            with self.assertRaises(ValueError):
                admission.benchmark_evidence(root, review, 'd' * 64, '1.1.9')

    def test_the_preview_hashes_every_actual_archive(self):
        import hashlib
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            archive = root / 'linux.tar.gz'
            archive.write_bytes(b'reviewed bytes')
            review = {'packages': [{'archive': archive.name, 'archive_size': archive.stat().st_size,
                                    'archive_sha256': hashlib.sha256(archive.read_bytes()).hexdigest()}]}
            admission.verify_review_archives(root, review)
            archive.write_bytes(b'altered bytes!')
            with self.assertRaisesRegex(ValueError, 'wrong SHA-256'):
                admission.verify_review_archives(root, review)

    def test_approval_job_name_is_from_one_verified_version_not_a_pr_title(self):
        seen = {'submission': {'id': 'test'}}
        package = {'id': 'test', 'version': '0.1.3'}
        review = {'id': 'test', 'packages': [package, package.copy()]}
        self.assertEqual(admission.approval_subject(seen, review), 'test@0.1.3')
        with self.assertRaisesRegex(ValueError, 'requested extension and version'):
            admission.approval_subject(seen, review, 'test@0.1.2')
        for other in ({'id': 'other'}, {'version': '0.1.2'}, {'version': '0.1.3\njob=other'}):
            with self.subTest(other=other), self.assertRaises(ValueError):
                admission.approval_subject(seen, {**review, 'packages': [package, {**package, **other}]})

    def test_the_version_the_store_has_is_the_newest_release_of_that_plugin(self):
        releases = [{'tag_name': 'extension/test/v0.1.2'}, {'tag_name': 'extension/test/v0.1.10'},
                    {'tag_name': 'extension/other/v9.0.0'}, {'tag_name': 'app/test'}]
        self.assertEqual(admission.previous_version(lambda path: releases, 'test'), '0.1.10')
        self.assertIsNone(admission.previous_version(lambda path: releases, 'new'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
