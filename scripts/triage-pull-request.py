#!/usr/bin/env python3
"""Apply fixed review labels from trusted PR metadata; never approve or merge."""
import json
import os
import re
import urllib.error
import urllib.request

# The app that runs this repository's workflows: only its check runs are CI.
ACTIONS = 'github-actions'
LABELS = {
    'scope:application': ('1d76db', 'Application change targeting main'),
    'scope:extension': ('5319e7', 'Plugin source or extension-branch maintenance'),
    'kind:dependency': ('0366d6', 'Dependency update proposed by Dependabot'),
    'review:maintainer': ('fbca04', 'Independent human maintainer review required'),
}
for platform in ('windows', 'android', 'macos', 'linux'):
    LABELS[f'platform:{platform}'] = ('0e8a16', f'Declared or directly changed {platform} support; not certification')
for artifact in ('binary', 'shared-library', 'wasm', 'apk'):
    LABELS[f'artifact:{artifact}'] = ('7057ff', f'Artifact category: {artifact}')
for format_name in ('elf', 'exe', 'so', 'dll', 'dylib'):
    LABELS[f'format:{format_name}'] = ('c5def5', f'Declared native artifact format: {format_name}')
for state in ('passed', 'failed', 'pending'):
    LABELS[f'ci:{state}'] = ('0e8a16' if state == 'passed' else 'b60205' if state == 'failed' else 'fbca04',
                             f'Current head CI status: {state}; not maintainer approval')
for priority in ('critical', 'high', 'normal', 'low'):
    LABELS[f'priority:{priority}'] = ('b60205' if priority == 'critical' else 'd93f0b',
                                    f'Maintainer-assigned priority: {priority}')


def classify(paths, manifests):
    labels = set()
    for path in paths:
        parts = path.lower().split('/')
        for platform in ('windows', 'android', 'macos', 'linux'):
            if platform in parts:
                labels.add(f'platform:{platform}')
        if path.startswith('android/'):
            labels.add('artifact:apk')
    for manifest in manifests:
        platforms = set(manifest.get('platforms', [])) & {'windows', 'android', 'macos', 'linux'}
        labels.update(f'platform:{platform}' for platform in platforms)
        kind = manifest.get('execution', {}).get('kind')
        if kind == 'wasm':
            labels.add('artifact:wasm')
        elif kind in ('process', 'native_library'):
            labels.add('artifact:binary' if kind == 'process' else 'artifact:shared-library')
            if platforms & {'linux', 'android'}:
                labels.add('format:elf')
                if kind == 'native_library':
                    labels.add('format:so')
            if 'windows' in platforms:
                labels.add('format:exe' if kind == 'process' else 'format:dll')
            if 'macos' in platforms and kind == 'native_library':
                labels.add('format:dylib')
    return sorted(labels)


def ci_state(checks):
    # CI is this repository's own workflows. A reviewer app's check run or commit status (a review skipped for want of
    # credit reports "success") says nothing about whether the code was built and tested.
    checks = [check for check in checks
              if (check.get('app') or {}).get('slug') == ACTIONS and check.get('name') != 'PR metadata labels']
    # Every rerun is its own check suite, so `filter=latest` still returns older runs of the same check:
    # only the newest run of each name counts, or a superseded failure would hide a later success.
    newest = {}
    for check in checks:
        name = check.get('name')
        # A label event can create skipped checks for the same head. It did not rerun this job.
        if check.get('conclusion') == 'skipped':
            continue
        if name not in newest or check.get('id', 0) > newest[name].get('id', 0):
            newest[name] = check
    checks = list(newest.values())
    conclusions = [check.get('conclusion') for check in checks]
    if any(value in ('failure', 'timed_out', 'cancelled', 'action_required', 'startup_failure', 'stale')
           for value in conclusions):
        return 'failed'
    if (any(check.get('status') != 'completed' for check in checks)
            or any(value not in ('success', 'neutral', 'skipped') for value in conclusions)
            or 'success' not in conclusions):
        return 'pending'
    return 'passed'


def metadata_labels(request, repository, pull):
    import base64
    sha = pull.get('head', {}).get('sha', '')
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        return []
    prefix = f'/repos/{repository}'
    count = pull.get('changed_files')
    if type(count) is not int or not 0 < count < 3000:
        raise ValueError('Changed-file coverage is unavailable for classification')
    files = []
    for page in range(1, (count + 99) // 100 + 1):
        files.extend(request('GET', f'{prefix}/pulls/{pull["number"]}/files?per_page=100&page={page}'))
    if len(files) != count:
        raise ValueError('Changed-file classification is incomplete')
    paths = [file['filename'] for file in files]
    roots = { '/'.join(path.split('/')[:2]) for path in paths
              if len(path.split('/')) >= 3 and path.split('/')[0] in ('extension', 'crates') }
    manifests = []
    for root in sorted(roots):
        for suffix in ('plugin.json', 'wasm/plugin.json'):
            try:
                blob = request('GET', f'{prefix}/contents/{root}/{suffix}?ref={sha}')
            except urllib.error.HTTPError as error:
                error.close()
                if error.code == 404:
                    continue
                raise
            if (isinstance(blob, dict) and blob.get('type') == 'file'
                    and blob.get('encoding') == 'base64' and len(blob.get('content', '')) <= 131072):
                manifest = json.loads(base64.b64decode(''.join(blob['content'].split()), validate=True))
                if isinstance(manifest, dict):
                    manifests.append(manifest)
    checks = []
    for page in range(1, 31):
        result = request('GET', f'{prefix}/commits/{sha}/check-runs?filter=latest&per_page=100&page={page}')
        checks.extend(result['check_runs'])
        if len(checks) >= result['total_count']:
            break
    else:
        raise ValueError('Check-run coverage exceeds classification bounds')
    return [*classify(paths, manifests), 'ci:' + ci_state(checks)]


def labels_for(pull):
    branch = pull['base']['ref']
    if branch not in ('main', 'extension'):
        return []
    labels = ['scope:extension' if branch == 'extension' else 'scope:application']
    author = pull['user']
    if author.get('login') == 'dependabot[bot]' and author.get('type') == 'Bot':
        labels.append('kind:dependency')
    return labels


def reviewers_satisfied(request, repository, pull):
    """A reviewer bot read the current head and every current thread is resolved (shared with the CI gate)."""
    import importlib.util
    from pathlib import Path
    spec = importlib.util.spec_from_file_location('review_readiness', Path(__file__).with_name('verify-review-readiness.py'))
    readiness = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(readiness)
    owner, name = repository.split('/')

    def threads():
        data = request('POST', '/graphql', {'query': readiness.THREADS_QUERY,
                                            'variables': {'owner': owner, 'name': name, 'pr': pull['number']}})
        if data.get('errors'):
            raise ValueError('Review threads could not be read')
        return data['data']['repository']['pullRequest']['reviewThreads']
    try:
        readiness.check(lambda path: request('GET', f'/repos/{repository}/{path}'), threads, pull['number'],
                        pull.get('head', {}).get('sha', ''))
    except ValueError:
        return False
    return True


def apply(request, repository, number):
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository) or type(number) is not int or number <= 0:
        raise ValueError('Invalid pull request target')
    pull = request('GET', f'/repos/{repository}/pulls/{number}')
    if pull.get('state') != 'open' or pull.get('number') != number:
        return []
    labels = labels_for(pull)
    if not labels:
        return []
    labels.extend(metadata_labels(request, repository, pull))
    # A maintainer is asked only once the reviewer bots are satisfied and CI passed on this exact head.
    if 'ci:passed' in labels and reviewers_satisfied(request, repository, pull):
        labels.append('review:maintainer')
    # Refresh automation-owned classification while preserving human labels and
    # priority. Never approve, merge or execute contributor code.
    for name in dict.fromkeys([*labels, *(name for name in LABELS if name.startswith('priority:'))]):
        color, description = LABELS[name]
        try:
            request('POST', f'/repos/{repository}/labels',
                    {'name': name, 'color': color, 'description': description})
        except urllib.error.HTTPError as error:
            error.close()
            if error.code != 422:
                raise
            # A 422 can mean malformed data as well as an existing label. Confirm
            # the exact fixed label exists before attempting to apply it.
            from urllib.parse import quote
            existing = request('GET', f'/repos/{repository}/labels/{quote(name, safe="")}')
            if existing.get('name') != name:
                raise ValueError('The expected review label could not be established') from error
    if labels:
        from urllib.parse import quote
        if any(name.startswith('ci:') for name in labels):
            for existing in pull.get('labels', []):
                name = existing.get('name', '')
                if (name in LABELS and name.startswith(('ci:', 'platform:', 'artifact:', 'format:', 'review:'))
                        and name not in labels):
                    encoded = quote(name, safe='')
                    request('DELETE', f'/repos/{repository}/issues/{number}/labels/{encoded}')
        request('POST', f'/repos/{repository}/issues/{number}/labels', {'labels': labels})
    return labels


def open_prs(request, repository, head=None):
    """Every open PR into main or extension, or only those whose head is commit `head`. Superseded events of a PR fold
    into its newest run; a run by hand reconciles them all."""
    numbers = set()
    for page in range(1, 11):
        pulls = request('GET', f'/repos/{repository}/pulls?state=open&per_page=100&page={page}')
        numbers.update(pull['number'] for pull in pulls
                       if pull.get('base', {}).get('ref') in ('main', 'extension')
                       and (head is None or pull.get('head', {}).get('sha') == head))
        if len(pulls) < 100:
            return sorted(numbers)
    raise ValueError('Open PR reconciliation exceeds coverage')


def targets(request, repository, number, head):
    """The PRs an event concerns: its own PR, the PRs whose head a completed workflow ran on, or (by hand) all."""
    if number:
        if not re.fullmatch(r'[1-9][0-9]{0,9}', number):
            raise ValueError('Invalid pull request number')
        return [int(number)]
    if head:
        if not re.fullmatch(r'[0-9a-f]{40}', head):
            raise ValueError('Invalid head commit')
        return open_prs(request, repository, head)
    return open_prs(request, repository)


def main():
    token = os.environ.get('GH_TOKEN')
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    if not token:
        raise SystemExit('A workflow token is required')

    def request(method, path, body=None):
        payload = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request('https://api.github.com' + path, data=payload, method=method,
                                     headers={'Authorization': 'Bearer ' + token,
                                              'Accept': 'application/vnd.github+json',
                                              'X-GitHub-Api-Version': '2022-11-28',
                                              'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as response:
            return json.load(response)

    for value in targets(request, repository, os.environ.get('PR_NUMBER', ''), os.environ.get('HEAD_SHA', '')):
        apply(request, repository, value)
    print('Metadata triage completed; no approval, merge or publication performed.')


if __name__ == '__main__':
    main()
