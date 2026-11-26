#!/usr/bin/env python3
"""Read-only gate run before the costly build: every reviewer bot that started on this exact commit has finished, at
least one did, and every open conversation on it is resolved, so runner time is spent once, on code that is ready."""
import argparse
import json
import os
import re
import sys
from urllib.request import Request, urlopen

# A maintainer-only label for when no reviewer bot works on a branch (down, out of credits, uninstalled): it skips the
# reviewer bots, never open threads.
WAIVER = 'review:waived'
MAX_PAGES = 10
# Whichever reviewer bots are installed count (CodeRabbit, Devin, Greptile or another app), none named here. Not the
# repository's own workflows, which run as this app (this gate among them) and could otherwise vouch for their own PR.
WORKFLOWS = 'github-actions'
THREADS_QUERY = ('query($owner:String!,$name:String!,$pr:Int!){repository(owner:$owner,name:$name)'
                 '{pullRequest(number:$pr){reviewThreads(first:100){nodes{isResolved isOutdated}'
                 'pageInfo{hasNextPage}}}}}')


def reviewer_bot(user):
    """An installed app's own account; a person or the repository's workflows do not count."""
    login = user.get('login') or ''
    return user.get('type') == 'Bot' and login.endswith('[bot]') and login != f'{WORKFLOWS}[bot]'


def pages(fetch, path, items):
    """Every item of a paged list (`items` names the list inside an object answer, or None for a bare list)."""
    found = []
    for page in range(1, MAX_PAGES + 1):
        answer = fetch(f'{path}per_page=100&page={page}')
        batch = answer.get(items) if items and isinstance(answer, dict) else answer
        if not isinstance(batch, list):
            raise ValueError('Unexpected answer from GitHub')
        found.extend(batch)
        if len(batch) < 100:
            return found
    raise ValueError('Too much history to check within the gate bounds')


# The repository's newest review comments read to learn which apps review code here.
RECENT_COMMENTS = 'pulls/comments?sort=created&direction=desc&per_page=100'


def reviewer_bots(fetch, pr_number, commit):
    """The reviewer bots on `commit`, each with whether it finished there. A reviewer is an app that reviewed this PR (at
    any commit) or left a review comment among the repository's newest; other apps (deployment previews, for example)
    neither count nor hold anything back. On `commit`, a reviewer finished with a review there, with its check runs once
    completed (whatever the conclusion: what it found is in its threads), or with the newest status of each context it
    set once no longer pending."""
    reviews = pages(fetch, f'pulls/{pr_number}/reviews?', None)
    comments = fetch(RECENT_COMMENTS)
    if not isinstance(comments, list):
        raise ValueError('Unexpected answer from GitHub')
    reviewers = {user['login'] for item in reviews + comments
                 for user in [item.get('user') or {}] if reviewer_bot(user)}
    bots = {}
    for review in reviews:
        if review.get('commit_id') == commit and (review.get('user') or {}).get('login') in reviewers:
            bots.setdefault(review['user']['login'], True)
    for run in pages(fetch, f'commits/{commit}/check-runs?filter=latest&', 'check_runs'):
        login = f"{(run.get('app') or {}).get('slug') or ''}[bot]"
        if login in reviewers:
            bots[login] = bots.get(login, True) and run.get('status') == 'completed'
    contexts = set()
    # Newest first: an older state of a context is superseded.
    for status in pages(fetch, f'commits/{commit}/statuses?', None):
        context, login = status.get('context'), (status.get('creator') or {}).get('login')
        if context in contexts:
            continue
        contexts.add(context)
        if login in reviewers:
            bots[login] = bots.get(login, True) and status.get('state') != 'pending'
    return bots


def check(fetch, threads, pr_number, commit):
    pr = fetch(f'pulls/{pr_number}')
    if pr.get('head', {}).get('sha') != commit:
        raise ValueError('The PR moved to another commit; label it again for the new one')
    waived = any(label.get('name') == WAIVER for label in pr.get('labels', []))
    if not waived:
        bots = reviewer_bots(fetch, pr_number, commit)
        working = sorted(login for login, finished in bots.items() if not finished)
        if working:
            raise ValueError(f'Reviewer bots still working on this commit: {", ".join(working)}')
        if not bots:
            raise ValueError('No reviewer bot has finished on this commit yet')
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
    # Ignored. A pull_request run executes the workflow in the PR's merge ref, which GitHub recomputes only later; a
    # ref made before the reviewer bots stopped being configured still passes their old list.
    parser.add_argument('--reviewers', help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository)
                or not re.fullmatch(r'[0-9a-f]{40}', args.commit) or args.pr <= 0):
            raise ValueError('Invalid readiness inputs')
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

        check(fetch, threads, args.pr, args.commit)
        print('Every reviewer bot on this commit finished and every current review thread is resolved.')
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Never include HTTP headers or response bodies.
        print(f'Not ready for CI: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
