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
OLDER = 'b' * 40
BOTS = ['coderabbitai[bot]', 'devin-ai-integration[bot]', 'greptile-apps[bot]']
REVIEWS = 'pulls/7/reviews?per_page=100&page=1'
RUNS = f'commits/{HEAD}/check-runs?filter=latest&per_page=100&page=1'
STATUSES = f'commits/{HEAD}/statuses?per_page=100&page=1'


def review(login, commit=HEAD, kind='Bot'):
    return {'user': {'login': login, 'type': kind}, 'commit_id': commit, 'state': 'COMMENTED'}


def run(slug, status='completed', conclusion='success'):
    return {'app': {'slug': slug}, 'status': status, 'conclusion': conclusion}


def status(login, state, context='Review', kind='Bot'):
    return {'creator': {'login': login, 'type': kind}, 'state': state, 'context': context}


class Readiness(unittest.TestCase):
    def fixture(self):
        return {
            'pulls/7': {'head': {'sha': HEAD}, 'labels': []},
            REVIEWS: [review(login) for login in BOTS],
            # The reviewer apps of this repository commented on code before.
            readiness.RECENT_COMMENTS: [{'user': {'login': login, 'type': 'Bot'}} for login in BOTS],
            RUNS: {'check_runs': []},
            STATUSES: [],
            'threads': {'nodes': [{'isResolved': True, 'isOutdated': False}],
                        'pageInfo': {'hasNextPage': False}},
        }

    def test_an_app_that_never_reviews_code_neither_counts_nor_holds_it_back(self):
        # A deployment preview's finished check run is no review.
        data = self.fixture()
        data[REVIEWS] = []
        data[RUNS]['check_runs'] = [run('vercel')]
        data[STATUSES] = [status('cloudflare-workers-and-pages[bot]', 'success', context='Preview')]
        self.refused(data, 'No reviewer bot has finished')
        # Nor does one that stays queued keep a reviewed commit from its check.
        data = self.fixture()
        data[RUNS]['check_runs'] = [run('vercel', status='queued')]
        data[STATUSES] = [status('cloudflare-workers-and-pages[bot]', 'pending', context='Preview')]
        self.check(data)

    def test_a_reviewer_known_from_this_repository_still_working_holds_back_a_first_commit(self):
        # It has not reviewed this PR yet, but it reviews code here: another bot's finished review is not enough.
        data = self.fixture()
        data[REVIEWS] = [review(BOTS[1])]
        data[RUNS]['check_runs'] = [run('coderabbitai', status='in_progress')]
        self.refused(data, 'still working on this commit: coderabbitai[bot]')

    def check(self, data):
        def fetch(path):
            empty = {'check_runs': []} if '/check-runs?' in path else []
            return copy.deepcopy(data.get(path, empty))
        return readiness.check(fetch, lambda: copy.deepcopy(data['threads']), 7, HEAD)

    def refused(self, data, words):
        with self.assertRaises(ValueError) as refusal:
            self.check(data)
        self.assertIn(words, str(refusal.exception))

    def test_every_reviewer_on_the_head_and_every_thread_resolved_is_ready(self):
        self.check(self.fixture())

    def test_any_one_reviewer_bot_that_finished_on_the_head_is_enough_whichever_it_is(self):
        # A review, a completed check run of its own (even skipped, with nothing to say) or a final status.
        for reviews, runs, statuses in (([review('some-new-reviewer[bot]')], [], []),
                                        ([], [run('coderabbitai', conclusion='skipped')], []),
                                        ([], [], [status('devin-ai-integration[bot]', 'success')])):
            with self.subTest(reviews=reviews, runs=runs, statuses=statuses):
                data = self.fixture()
                data[REVIEWS], data[RUNS]['check_runs'], data[STATUSES] = reviews, runs, statuses
                self.check(data)

    def test_a_reviewer_bot_still_working_on_the_head_holds_it_back_whatever_the_others_did(self):
        for runs, statuses in (([run('greptile-apps', status='in_progress')], []),
                               ([run('greptile-apps', status='queued')], []),
                               ([], [status('devin-ai-integration[bot]', 'pending')])):
            with self.subTest(runs=runs, statuses=statuses):
                data = self.fixture()
                data[RUNS]['check_runs'], data[STATUSES] = runs, statuses
                self.refused(data, 'still working on this commit')
        # A newer state of the same context supersedes the pending one.
        data = self.fixture()
        data[STATUSES] = [status('devin-ai-integration[bot]', 'success'), status('devin-ai-integration[bot]', 'pending')]
        self.check(data)

    def test_no_bot_on_the_head_a_person_or_the_workflows_is_not_ready(self):
        for reviews, runs, statuses in (([], [], []), ([review(BOTS[0], OLDER)], [], []),
                                        ([review('DarkPhilosophy', kind='User')], [], []),
                                        ([review('github-actions[bot]')], [run('github-actions')], []),
                                        ([review('pretend[bot]', kind='User')], [], []),
                                        ([], [], [status('DarkPhilosophy', 'success', kind='User')])):
            with self.subTest(reviews=reviews, runs=runs, statuses=statuses):
                data = self.fixture()
                data[REVIEWS], data[RUNS]['check_runs'], data[STATUSES] = reviews, runs, statuses
                self.refused(data, 'No reviewer bot has finished')

    def test_the_repository_s_own_workflows_running_never_hold_it_back(self):
        data = self.fixture()
        data[RUNS]['check_runs'] = [run('github-actions', status='in_progress')]
        self.check(data)

    def test_an_open_current_thread_is_not_addressed_but_an_outdated_one_is(self):
        data = self.fixture()
        data['threads']['nodes'].append({'isResolved': False, 'isOutdated': False})
        self.refused(data, 'still unresolved')
        data['threads']['nodes'][-1]['isOutdated'] = True
        self.check(data)

    def test_unbounded_or_moved_state_is_refused(self):
        for change in ('more-threads', 'head-moved', 'endless-reviews'):
            with self.subTest(change=change):
                data = self.fixture()
                if change == 'more-threads':
                    data['threads']['pageInfo']['hasNextPage'] = True
                elif change == 'head-moved':
                    data['pulls/7']['head']['sha'] = 'c' * 40
                else:
                    for page in range(1, readiness.MAX_PAGES + 1):
                        data[f'pulls/7/reviews?per_page=100&page={page}'] = [review(BOTS[0])] * 100
                with self.assertRaises(ValueError):
                    self.check(data)

    def test_a_maintainer_waiver_skips_the_reviewer_bots_but_never_open_threads(self):
        data = self.fixture()
        data[REVIEWS] = []
        data[RUNS]['check_runs'] = [run('greptile-apps', status='in_progress')]
        data['pulls/7']['labels'] = [{'name': readiness.WAIVER}]
        self.check(data)
        data['threads']['nodes'].append({'isResolved': False, 'isOutdated': False})
        self.refused(data, 'still unresolved')

    def test_a_reviewer_that_reviewed_the_head_once_counts_among_many_reviews(self):
        data = self.fixture()
        data[REVIEWS] = [review(BOTS[0], OLDER)] * 100
        data['pulls/7/reviews?per_page=100&page=2'] = [review(BOTS[2])]
        self.check(data)


if __name__ == '__main__':
    unittest.main(verbosity=2)
