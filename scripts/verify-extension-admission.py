#!/usr/bin/env python3
"""Read-only admission gate: bind an exact successful source run to human approval."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
from urllib.request import Request, urlopen


def check(fetch, run_id, pr_number, commit):
    run = fetch(f'actions/runs/{run_id}')
    if (run.get('event') != 'pull_request' or run.get('conclusion') != 'success'
            or run.get('status') != 'completed' or run.get('head_sha') != commit
            or run.get('path') != '.github/workflows/extension-validate.yml'
            or not any(pr.get('number') == pr_number for pr in run.get('pull_requests', []))):
        raise ValueError('Admission requires the successful source-submission CI run for this exact PR and commit')
    pr = fetch(f'pulls/{pr_number}')
    if pr.get('base', {}).get('ref') != 'extension' or pr.get('head', {}).get('sha') != commit:
        raise ValueError('The reviewed PR must still target extension and have the same head commit')
    if pr.get('state') != 'open' and not pr.get('merged'):
        raise ValueError('A closed, unmerged submission cannot be admitted')
    # A PR may alter its own workflow. Recheck its immutable changed-file scope here,
    # from trusted default-branch code, before accepting any green CI signal.
    count = pr.get('changed_files')
    if type(count) is not int or not 0 < count < 3000:
        raise ValueError('Submission file count is missing or exceeds API coverage')
    paths, fetched = [], 0
    for page in range(1, (count + 99) // 100 + 1):
        files = fetch(f'pulls/{pr_number}/files?per_page=100&page={page}')
        if not isinstance(files, list):
            raise ValueError('Invalid changed-file response')
        fetched += len(files)
        for file in files:
            paths.append(file['filename'])
            if 'previous_filename' in file:
                paths.append(file['previous_filename'])
    if fetched != count:
        raise ValueError('Changed-file coverage is incomplete')
    specification = importlib.util.spec_from_file_location(
        'extension_submission_paths', Path(__file__).with_name('plan-extension-submission.py'))
    planner = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(planner)
    submission = planner.plan(paths)
    ownership_spec = importlib.util.spec_from_file_location(
        'extension_ownership', Path(__file__).with_name('verify-extension-ownership.py'))
    ownership = importlib.util.module_from_spec(ownership_spec)
    ownership_spec.loader.exec_module(ownership)
    latest = {}
    page = 1
    while True:
        reviews = fetch(f'pulls/{pr_number}/reviews?per_page=100&page={page}')
        if not isinstance(reviews, list):
            raise ValueError('Invalid review response')
        for review in reviews:
            if review.get('state') != 'PENDING':
                latest[review.get('user', {}).get('login')] = review
        if len(reviews) < 100:
            break
        page += 1
        if page > 100:
            raise ValueError('Review history exceeds admission bounds')
    ownership.authorize(fetch, pr, submission, commit, list(latest.values()))
    approved = False
    for login, review in latest.items():
        user = review.get('user', {})
        if not login or user.get('type') != 'User' or login == pr.get('user', {}).get('login'):
            continue
        if review.get('commit_id') != commit or review.get('state') != 'APPROVED':
            continue
        # Review text and association labels do not grant maintainer authority.
        permission = fetch(f'collaborators/{login}/permission').get('permission')
        if permission in ('admin', 'maintain', 'write'):
            approved = True
    if not approved:
        raise ValueError('The exact source commit needs an independent human maintainer approval')
    return submission


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--run', type=int, required=True)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--review', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository)
                or not re.fullmatch(r'[0-9a-f]{40}', args.commit)
                or not re.fullmatch(r'[0-9a-f]{64}', args.sha256)
                or args.run <= 0 or args.pr <= 0):
            raise ValueError('Invalid admission inputs')
        import hashlib
        raw = args.review.read_bytes()
        if hashlib.sha256(raw).hexdigest() != args.sha256:
            raise ValueError('The review bundle is not the exact document the maintainer reviewed')
        review = json.loads(raw)
        if (review.get('schema') != 1 or review.get('commit') != args.commit
                or not re.fullmatch(r'[0-9a-f]{40}', review.get('sdk', ''))):
            raise ValueError('The review document is not pinned to the approved source and SDK')
        token = os.environ.get('GH_TOKEN')
        if not token:
            raise ValueError('Read-only GitHub token is required')
        def fetch(path):
            from urllib.error import HTTPError
            route = path if path.startswith('users/') else f'repos/{args.repository}/{path}'
            request = Request(f'https://api.github.com/{route}', headers={
                'Authorization': f'Bearer {token}', 'Accept': 'application/vnd.github+json',
                'X-GitHub-Api-Version': '2022-11-28'})
            try:
                with urlopen(request, timeout=30) as response:
                    return json.load(response)
            except HTTPError as error:
                error.close()
                if error.code == 404 and path.startswith('contents/'):
                    return None
                raise
        submission = check(fetch, args.run, args.pr, args.commit)
        if review.get('id') != submission['id']:
            raise ValueError('Reviewed artifact identity differs from the submitted plugin directory')
        print('Exact source CI, review digest and independent human maintainer approval verified.')
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Never include HTTP headers or response bodies (which may contain private data).
        print(f'Admission refused: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
