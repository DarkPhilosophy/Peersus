#!/usr/bin/env python3
"""Authorize plugin changes using authenticated identities and current base ownership."""
import argparse
import base64
import json
import os
import re
import sys
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def registry(blob):
    if not isinstance(blob, dict) or blob.get('type') != 'file' or blob.get('encoding') != 'base64':
        raise ValueError('Every plugin must have a regular ownership.json registry')
    encoded = blob.get('content', '')
    if not isinstance(encoded, str) or len(encoded) > 32768:
        raise ValueError('Ownership registry exceeds bounds')
    data = json.loads(base64.b64decode(''.join(encoded.split()), validate=True))
    if (not isinstance(data, dict) or set(data) != {'schema', 'owner', 'contributors'}
            or type(data['schema']) is not int or data['schema'] != 1
            or not isinstance(data['contributors'], list) or len(data['contributors']) > 64):
        raise ValueError('Invalid ownership registry schema')
    seen = set()
    logins = set()
    for person in [data['owner'], *data['contributors']]:
        if (not isinstance(person, dict) or set(person) != {'login', 'id'}
                or type(person['id']) is not int or person['id'] <= 0
                or not isinstance(person['login'], str)
                or not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?', person['login'])
                or person['id'] in seen or person['login'].lower() in logins):
            raise ValueError('Ownership roles require distinct verified GitHub accounts')
        seen.add(person['id'])
        logins.add(person['login'].lower())
    return data


def latest_reviews(fetch, number):
    latest = {}
    for page in range(1, 101):
        reviews = fetch(f'pulls/{number}/reviews?per_page=100&page={page}')
        if not isinstance(reviews, list):
            raise ValueError('Invalid ownership consent review response')
        for review in reviews:
            if review.get('state') != 'PENDING':
                latest[review.get('user', {}).get('id')] = review
        if len(reviews) < 100:
            return list(latest.values())
    raise ValueError('Review history exceeds ownership bounds')


def authorize(fetch, pr, submission, commit, reviews):
    directory = submission['directory']
    if not re.fullmatch(r'extension/[a-z][a-z0-9_-]{0,79}', directory):
        raise ValueError('Invalid ownership directory')
    current = fetch('git/ref/heads/extension')['object']['sha']
    if not re.fullmatch(r'[0-9a-f]{40}', current) or not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Ownership must be read from pinned revisions')
    proposed = registry(fetch(f'contents/{directory}/ownership.json?ref={commit}'))
    for person in [proposed['owner'], *proposed['contributors']]:
        user = fetch('users/' + person['login'])
        if (not isinstance(user, dict) or user.get('type') != 'User'
                or user.get('id') != person['id']
                or user.get('login', '').lower() != person['login'].lower()):
            raise ValueError('Ownership account name and immutable GitHub ID disagree')
    entries = fetch(f'contents/extension?ref={current}')
    if entries is not None:
        if not isinstance(entries, list) or len(entries) >= 1000:
            raise ValueError('Extension identity coverage is incomplete')
        plugin_name = directory.split('/')[1]
        plugin_id = plugin_name.removeprefix('peersus-')
        if any(entry.get('name', '').removeprefix('peersus-') == plugin_id
               and entry.get('name') != plugin_name for entry in entries):
            raise ValueError('This stable plugin ID already belongs to another source directory')
    consenting = set()
    actor = pr.get('user', {})
    if actor.get('type') == 'User' and type(actor.get('id')) is int:
        consenting.add(actor['id'])
    for review in reviews:
        user = review.get('user', {})
        if (user.get('type') == 'User' and type(user.get('id')) is int
                and review.get('state') == 'APPROVED' and review.get('commit_id') == commit):
            consenting.add(user['id'])
    previous_blob = fetch(f'contents/{directory}/ownership.json?ref={current}')
    if previous_blob is None:
        if fetch(f'contents/{directory}?ref={current}') is not None:
            raise ValueError('An existing plugin without ownership cannot be claimed through a source PR')
        if proposed['owner']['id'] not in consenting:
            raise ValueError('The first submission needs the authenticated plugin owner consent')
        return proposed
    previous = registry(previous_blob)
    owner = previous['owner']['id']
    permitted = {owner, *(person['id'] for person in previous['contributors'])}
    if not consenting & permitted:
        raise ValueError('An update needs the current owner or a current contributor consent')
    roster_changed = (owner != proposed['owner']['id'] or
                      {person['id'] for person in previous['contributors']} !=
                      {person['id'] for person in proposed['contributors']})
    if roster_changed and owner not in consenting:
        raise ValueError('Only the current owner can authorize contributor or ownership changes')
    if owner != proposed['owner']['id'] and proposed['owner']['id'] not in consenting:
        raise ValueError('Ownership transfer also needs the receiving owner consent')
    return proposed


def github_fetch(repository, token):
    def fetch(path):
        route = path if path.startswith('users/') else f'repos/{repository}/{path}'
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
    return fetch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--commit', required=True)
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository)
                or args.pr <= 0 or not re.fullmatch(r'[0-9a-f]{40}', args.commit)):
            raise ValueError('Invalid ownership check inputs')
        token = os.environ.get('GH_TOKEN')
        if not token:
            raise ValueError('Read-only GitHub token is required')
        fetch = github_fetch(args.repository, token)
        pr = fetch(f'pulls/{args.pr}')
        if pr.get('base', {}).get('ref') != 'extension' or pr.get('head', {}).get('sha') != args.commit:
            raise ValueError('Ownership check requires the exact current extension PR head')
        # The trusted path planner emits the directory; it is data, not executable source.
        directory = os.environ['EXTENSION_DIRECTORY']
        authorize(fetch, pr, {'directory': directory}, args.commit, latest_reviews(fetch, args.pr))
        print('Plugin owner, contributor permissions and exact-revision consent verified.')
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'Ownership refused: {type(error).__name__}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
