#!/usr/bin/env python3
"""Exercise the review-readiness gate without credentials or external API calls."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('readiness', Path(__file__).with_name('verify-review-readiness.py'))
readiness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(readiness)

HEAD = 'a' * 40
BOTS = ['coderabbitai[bot]', 'devin-ai-integration[bot]', 'greptile-apps[bot]']


class Readiness(unittest.TestCase):
    def fixture(self):
        return {
            'pulls/7': {'head': {'sha': HEAD}, 'labels': []},
            'pulls/7/reviews?per_page=100&page=1': [
                {'user': {'login': login}, 'commit_id': HEAD, 'state': 'COMMENTED'} for login in BOTS],
            'pulls/7/reviews?per_page=100&page=2': [],
            'threads': {'nodes': [{'isResolved': True, 'isOutdated': False}],
                        'pageInfo': {'hasNextPage': False}},
        }

    def check(self, data):
        return readiness.check(lambda path: copy.deepcopy(data[path]),
                               lambda: copy.deepcopy(data['threads']), 7, HEAD, BOTS)

    def test_every_reviewer_on_the_head_and_every_thread_resolved_is_ready(self):
        self.check(self.fixture())

    def test_a_reviewer_missing_or_only_on_an_older_commit_is_not_ready(self):
        for change in ('missing', 'older'):
            with self.subTest(change=change):
                data = self.fixture()
                reviews = data['pulls/7/reviews?per_page=100&page=1']
                if change == 'missing':
                    reviews.pop()
                else:
                    reviews[0]['commit_id'] = 'b' * 40
                with self.assertRaises(ValueError):
                    self.check(data)

    def test_an_open_current_thread_is_not_addressed_but_an_outdated_one_is(self):
        data = self.fixture()
        data['threads']['nodes'].append({'isResolved': False, 'isOutdated': False})
        with self.assertRaises(ValueError):
            self.check(data)
        data['threads']['nodes'][-1]['isOutdated'] = True
        self.check(data)

    def test_unbounded_or_moved_state_is_refused(self):
        for change in ('more-threads', 'head-moved'):
            with self.subTest(change=change):
                data = self.fixture()
                if change == 'more-threads':
                    data['threads']['pageInfo']['hasNextPage'] = True
                else:
                    data['pulls/7']['head']['sha'] = 'c' * 40
                with self.assertRaises(ValueError):
                    self.check(data)

    def test_a_maintainer_waiver_skips_reviewers_but_never_open_threads(self):
        data = self.fixture()
        data['pulls/7/reviews?per_page=100&page=1'] = []
        data['pulls/7']['labels'] = [{'name': readiness.WAIVER}]
        self.check(data)
        data['threads']['nodes'].append({'isResolved': False, 'isOutdated': False})
        with self.assertRaises(ValueError):
            self.check(data)

    def test_a_reviewer_that_reviewed_the_head_once_counts_among_many_reviews(self):
        data = self.fixture()
        reviews = data['pulls/7/reviews?per_page=100&page=1']
        older = [{'user': {'login': BOTS[0]}, 'commit_id': 'd' * 40, 'state': 'COMMENTED'}] * 99
        data['pulls/7/reviews?per_page=100&page=1'] = older + [reviews[1]]
        data['pulls/7/reviews?per_page=100&page=2'] = [reviews[0], reviews[2]]
        data['pulls/7/reviews?per_page=100&page=3'] = []
        self.check(data)


if __name__ == '__main__':
    unittest.main(verbosity=2)
