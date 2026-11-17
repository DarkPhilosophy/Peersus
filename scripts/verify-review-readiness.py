#!/usr/bin/env python3
"""Read-only gate run before the costly build: the reviewers must have read this exact commit and every
open conversation on it must be resolved, so runner time is spent once, on code that is ready."""
import argparse
import json
import os
import re
import sys
from urllib.request import Request, urlopen

# A maintainer-only label for a reviewer that is down or never reviews a branch: it skips the reviewers, never open threads.
WAIVER = 'review:waived'
MAX_REVIEW_PAGES = 10
# Reviewers that must read every commit before CI and before a maintainer is asked: Devin is the one relied on.
# Others (CodeRabbit, Greptile) still comment; add them with the repository variable REQUIRED_REVIEWERS
# (comma-separated logins) once their quotas allow a review of every commit.
DEFAULT_REVIEWERS = 'devin-ai-integration[bot]'
THREADS_QUERY = ('query($owner:String!,$name:String!,$pr:Int!){repository(owner:$owner,name:$name)'
                 '{pullRequest(number:$pr){reviewThreads(first:100){nodes{isResolved isOutdated}'
                 'pageInfo{hasNextPage}}}}}')


def reviewer_list(text):
    reviewers = [login.strip() for login in (text or DEFAULT_REVIEWERS).split(',') if login.strip()]
    if not reviewers or not all(re.fullmatch(r'[A-Za-z0-9-]+(\[bot\])?', login) for login in reviewers):
        raise ValueError('Invalid reviewer list')
    return reviewers


def check(fetch, threads, pr_number, commit, reviewers):
    pr = fetch(f'pulls/{pr_number}')
    if pr.get('head', {}).get('sha') != commit:
        raise ValueError('The PR moved to another commit; label it again for the new one')
    waived = any(label.get('name') == WAIVER for label in pr.get('labels', []))
    if not waived:
        reviewed = set()
        for page in range(1, MAX_REVIEW_PAGES + 1):
            batch = fetch(f'pulls/{pr_number}/reviews?per_page=100&page={page}')
            if not isinstance(batch, list):
                raise ValueError('Unexpected review history')
            reviewed.update(review.get('user', {}).get('login') for review in batch
                            if review.get('commit_id') == commit)
            if len(batch) < 100:
                break
        else:
            raise ValueError('Review history exceeds the gate bounds')
        missing = [login for login in reviewers if login not in reviewed]
        if missing:
            raise ValueError(f'Not yet reviewed on this commit by: {", ".join(missing)}')
    conversation = threads()
    if conversation.get('pageInfo', {}).get('hasNextPage'):
        raise ValueError('Too many review threads to check')
    # An outdated thread was answered by changing the code it was about; a current one must be resolved.
    unresolved = sum(1 for thread in conversation.get('nodes', [])
                     if not thread.get('isResolved') and not thread.get('isOutdated'))
    if unresolved:
        raise ValueError(f'{unresolved} review thread(s) on the current code are still unresolved')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--reviewers', default='', help='Comma-separated reviewer logins; empty for the default')
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository)
                or not re.fullmatch(r'[0-9a-f]{40}', args.commit) or args.pr <= 0):
            raise ValueError('Invalid readiness inputs')
        reviewers = reviewer_list(args.reviewers)
        token = os.environ.get('GH_TOKEN')
        if not token:
            raise ValueError('Read-only GitHub token is required')
        headers = {'Authorization': f'Bearer {token}', 'Accept': 'application/vnd.github+json',
                   'X-GitHub-Api-Version': '2022-11-28'}

        def fetch(path):
            request = Request(f'https://api.github.com/repos/{args.repository}/{path}', headers=headers)
            with urlopen(request, timeout=30) as response:
                return json.load(response)

        def threads():
            owner, name = args.repository.split('/')
            body = json.dumps({'query': THREADS_QUERY, 'variables': {'owner': owner, 'name': name, 'pr': args.pr}})
            request = Request('https://api.github.com/graphql', data=body.encode(), headers=headers)
            with urlopen(request, timeout=30) as response:
                data = json.load(response)
            if data.get('errors'):
                raise ValueError('Review threads could not be read')
            return data['data']['repository']['pullRequest']['reviewThreads']

        check(fetch, threads, args.pr, args.commit, reviewers)
        print('Every reviewer read this commit and every current review thread is resolved.')
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Never include HTTP headers or response bodies.
        print(f'Not ready for CI: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
