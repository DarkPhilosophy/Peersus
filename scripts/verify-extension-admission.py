#!/usr/bin/env python3
"""Read-only admission gate: bind an exact successful source run to human approval.

The human approval is a maintainer's (write, maintain or admin) APPROVED review of the exact commit by someone other than the
PR's author. A submission whose author is a repository admin (the maintainer's own extension) has no such reviewer when
there is one maintainer: GitHub does not let an author approve their own PR. Its approval is then an admin's approval of this
admission run's protected environment (`extension-approval`, Review deployments), read back from the run's approval history.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
from urllib.request import Request, urlopen


# The protected environment of the admission workflow, whose required reviewer approves each run by hand.
ENVIRONMENT = 'extension-approval'


def approved_by_admin_for_run(fetch, admission_run):
    """Whether a human admin approved `admission_run`'s deployment to the admission environment."""
    history = fetch(f'actions/runs/{admission_run}/approvals')
    if not isinstance(history, list):
        raise ValueError('Invalid approval history')
    for approval in history:
        user = approval.get('user', {})
        environments = {environment.get('name') for environment in approval.get('environments', [])}
        if (approval.get('state') != 'approved' or ENVIRONMENT not in environments
                or user.get('type') != 'User' or not user.get('login')):
            continue
        if fetch(f'collaborators/{user["login"]}/permission').get('permission') == 'admin':
            return True
    return False


def inspect(fetch, run_id, pr_number, commit):
    """Everything admission requires except the human approval: the exact green run, the PR still at that commit, its
    changed-file scope and the plugin's ownership. Returns what was read, for the gate and for the summary."""
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
    return {'run': run, 'pr': pr, 'paths': paths, 'submission': submission, 'reviews': latest}


def check(fetch, run_id, pr_number, commit, admission_run=None):
    seen = inspect(fetch, run_id, pr_number, commit)
    pr, latest = seen['pr'], seen['reviews']
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
    author = pr.get('user', {})
    if (not approved and admission_run is not None and author.get('type') == 'User' and author.get('login')
            and fetch(f'collaborators/{author["login"]}/permission').get('permission') == 'admin'):
        approved = approved_by_admin_for_run(fetch, admission_run)
    if not approved:
        raise ValueError('The exact source commit needs an independent human maintainer approval '
                         '(or, for an admin\'s own submission, an admin\'s approval of this admission run)')
    return seen['submission']


def plain(text, limit=300):
    """Text a contributor wrote, shown as text: one line, bounded, with nothing Markdown or HTML would act on."""
    line = ' '.join(str(text).split())
    if len(line) > limit:
        line = line[:limit] + '…'
    return ''.join('\\' + c if c in '\\`*_[]<>|!#~{}()' else c for c in line)


def size(count):
    return f'{count / 1024:.0f} KiB' if count < 1024 * 1024 else f'{count / 1024 / 1024:.1f} MiB'


def approval_subject(seen, review, expected=None):
    """Short, single-line identity for the protected job, derived only from the pinned review."""
    plugin = seen['submission']['id']
    packages = review.get('packages')
    if not isinstance(packages, list) or not packages or not re.fullmatch(r'[a-z][a-z0-9_-]{0,63}', plugin):
        raise ValueError('Cannot identify the builds awaiting approval')
    version = packages[0].get('version', '')
    if (not re.fullmatch(r'(0|[1-9][0-9]{0,9})(\.(0|[1-9][0-9]{0,9})){2}', version)
            or review.get('id') != plugin
            or any(p.get('id') != plugin or p.get('version') != version for p in packages)):
        raise ValueError('The builds do not identify one extension version')
    subject = f'{plugin}@{version}'
    if expected is not None and expected != subject:
        raise ValueError('The requested extension and version do not match the pinned review')
    return subject


def benchmark_evidence(directory, review, review_sha, previous):
    """Read only evidence from the selected successful source run, bound to this review and current predecessor."""
    receipt_bytes = (directory / 'extension-input.json').read_bytes()
    receipt = json.loads(receipt_bytes)
    expected = {'schema': 1, 'id': review['id'], 'version': review['packages'][0]['version'],
                'commit': review['commit'], 'previous': previous, 'review_sha256': review_sha}
    if receipt != expected:
        raise ValueError('The benchmark does not bind this exact review and current published predecessor; rerun validation')
    report = (directory / 'impact.md').read_bytes()
    lines = report.decode().splitlines()
    verdict = re.fullmatch(r'## Resource impact: (PASS|REGRESSION|INCONCLUSIVE) — (.+)', lines[0] if lines else '')
    if not verdict:
        raise ValueError('The benchmark artifact has no verdict')
    state, reason = verdict.groups()
    if previous is None:
        if state != 'INCONCLUSIVE' or 'First release: the store has no predecessor. All three submitted samples completed; this is not a benchmark pass.' not in lines:
            raise ValueError('A first release needs three measured submitted samples, not a comparison verdict')
    elif state == 'INCONCLUSIVE':
        raise ValueError('An update requires a complete benchmark comparison')
    if state == 'REGRESSION' and re.match(r'[1-9][0-9]* blocking', reason):
        if 'Blocking regression accepted by a maintainer (label `impact:accepted`); the verdict is unchanged.' not in lines:
            raise ValueError('The source run has an unaccepted blocking regression')
        reason += '; blocking growth explicitly accepted in that run'

    fingerprint = hashlib.sha256(receipt_bytes + b'\0' + report)

    def agreement(side):
        digests = set()
        for sample in range(1, 4):
            raw = (directory / f'extension-{side}-{sample}.json').read_bytes()
            evidence = json.loads(raw)
            digest = evidence.get('agreement_sha256', '')
            if (evidence.get('schema') != 3 or evidence.get('workload', {}).get('profile') != 'extension-startup-idle'
                    or not re.fullmatch(r'[0-9a-f]{64}', digest)):
                raise ValueError('A scheduled extension sample is invalid or lacks its agreement')
            digests.add(digest)
            fingerprint.update(b'\0' + raw)
        if len(digests) != 1:
            raise ValueError('The agreement differs between samples of the same build')
        return digests.pop()

    submitted = agreement('change')
    agreement_state = 'new' if previous is None else (
        'unchanged' if submitted == agreement('baseline') else 'changed')
    return {'verdict': state, 'reason': reason, 'agreement': agreement_state}, fingerprint.hexdigest()


def verify_review_archives(directory, review):
    """Confirm the preview's names and digests describe the archives that would be staged."""
    names = set()
    for package in review['packages']:
        name, digest = package['archive'], package['archive_sha256']
        if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.tar\.gz', name)
                or name in names or not re.fullmatch(r'[0-9a-f]{64}', digest)):
            raise ValueError('Invalid or duplicate archive in the reviewed document')
        names.add(name)
        path = directory / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size != package['archive_size']:
            raise ValueError(f'Reviewed archive {name} is missing or has the wrong size')
        with path.open('rb') as archive:
            if hashlib.file_digest(archive, 'sha256').hexdigest() != digest:
                raise ValueError(f'Reviewed archive {name} has the wrong SHA-256')
    if {archive.name for archive in directory.glob('*.tar.gz')} != names:
        raise ValueError('The archive bundle does not match the reviewed inventory')


def summary(seen, review, sha256, previous, evidence):
    """What approving this run admits, in Markdown for the run's summary: read before the person approves."""
    packages = review.get('packages', [])
    first = packages[0] if packages else {}
    pr, run = seen['pr'], seen['run']
    version = plain(first.get('version', '?'), 40)
    subject = f'{plain(review["id"], 64)}@{version}'
    earlier = f'`{plain(previous, 40)}`' if previous else 'No published version'
    agreement = {'new': 'First agreement (no published predecessor)', 'changed': 'Changed',
                 'unchanged': 'Unchanged'}[evidence['agreement']]
    lines = [
        f'## Awaiting approval: sign and stage `{subject}`',
        '',
        f'**{plain(first.get("description", ""))}**',
        '',
        '| | |', '|---|---|',
        f'| Extension version | `{subject}`: {earlier} → `{version}` |',
        f'| Author | {plain(first.get("author", "?"), 100)} |',
        f'| Submitted in | PR #{pr.get("number")}: {plain(pr.get("title", ""), 200)}, by {plain(pr.get("user", {}).get("login", "?"), 64)} |',
        f'| Source | commit `{review["commit"]}`, {len(set(seen["paths"]))} files, all under `{plain(seen["submission"]["directory"], 100)}/` |',
        f'| Checked by | source-check run {run.get("id")}: {plain(run.get("conclusion", "?"), 20)} |',
        f'| Benchmark | **{evidence["verdict"]}** — {plain(evidence["reason"])} (in the exact source-check run above) |',
        f'| Licence and notice | {agreement} |',
        f'| Review document | SHA-256 `{sha256}` (matches what was dispatched) |',
        '',
        '| Build | Platforms | Runs as | Size | SHA-256 |', '|---|---|---|---|---|',
    ]
    for package in packages:
        lines.append(
            f'| `{plain(package["archive"], 135)}` | {plain(", ".join(package["platforms"]), 80)}'
            f' ({plain(package.get("architecture", "?"), 20)}) | {plain(package.get("execution", {}).get("kind", "?"), 20)}'
            f' | {size(int(package.get("archive_size", 0)))} | `{plain(package.get("archive_sha256", "?"), 64)}` |')
    capabilities = sorted({name for package in packages for name in package.get('capabilities', [])})
    lines += [
        '',
        'Permissions of its own: ' + (', '.join(f'`{plain(name, 64)}`' for name in capabilities) or 'none') + '.',
        '',
        'This list covers extension-defined permissions, not every host capability a device may be asked to grant. '
        'Signing grants none of them. Native alternatives run with OS-user privileges if installed manually.',
        '',
        '### What the buttons do',
        '',
        '- **Approve and deploy**: if the pinned evidence and approvals still match, sign the catalog entry for the '
        'WASM builds and stage the exact archives listed above in a workflow artifact for 90 days. Native builds '
        'are not catalog install targets.',
        '- **Will not**: merge the PR, publish a release, install on any device or grant any runtime permission. '
        f'Publishing `extension/{plain(review["id"], 64)}/v{version}` is a separate decision.',
        '- **Reject**: nothing is signed and nothing changes. The submission stays as it is.',
    ]
    return '\n'.join(lines) + '\n'


def previous_version(fetch, plugin):
    """The newest version of `plugin` the store already published, from its release tags."""
    prefix = f'extension/{plugin}/v'
    versions = []
    for page in range(1, 101):
        releases = fetch(f'releases?per_page=100&page={page}')
        if not isinstance(releases, list):
            raise ValueError('Invalid published-release listing')
        for release in releases:
            tag = release.get('tag_name', '')
            if not release.get('draft') and tag.startswith(prefix):
                version = tag[len(prefix):]
                if re.fullmatch(r'(0|[1-9][0-9]{0,9})(\.(0|[1-9][0-9]{0,9})){2}', version):
                    versions.append(version)
        if len(releases) < 100:
            return max(versions, key=lambda v: tuple(map(int, v.split('.'))), default=None)
    raise ValueError('Published-release history exceeds the comparison search bounds')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--run', type=int, required=True)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--review', type=Path, required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--impact', type=Path, required=True, help='Benchmark artifact downloaded from the exact source run')
    parser.add_argument('--expected-evidence', help='Benchmark fingerprint shown before this approval was requested')
    parser.add_argument('--admission-run', type=int,
                        help="This admission workflow run (GITHUB_RUN_ID): its environment approval can admit an admin's own submission")
    parser.add_argument('--preview', action='store_true',
                        help='Before approval: check everything but the approval and print what approving admits, in Markdown')
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository)
                or not re.fullmatch(r'[0-9a-f]{40}', args.commit)
                or not re.fullmatch(r'[0-9a-f]{64}', args.sha256)
                or args.run <= 0 or args.pr <= 0
                or (args.admission_run is not None and args.admission_run <= 0)):
            raise ValueError('Invalid admission inputs')
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
        if args.preview:
            seen = inspect(fetch, args.run, args.pr, args.commit)
            if review.get('id') != seen['submission']['id']:
                raise ValueError('Reviewed artifact identity differs from the submitted plugin directory')
            verify_review_archives(args.review.parent, review)
            subject = approval_subject(seen, review, os.environ.get('EXPECTED_CANDIDATE'))
            previous = previous_version(fetch, review['id'])
            evidence, fingerprint = benchmark_evidence(args.impact, review, args.sha256, previous)
            print(summary(seen, review, args.sha256, previous, evidence), end='')
            if output := os.environ.get('GITHUB_OUTPUT'):
                with open(output, 'a', encoding='utf-8') as stream:
                    print(f'subject={subject}', file=stream)
                    print(f'transition={previous or "new"} → {subject}', file=stream)
                    print(f'evidence={fingerprint}', file=stream)
            return 0
        submission = check(fetch, args.run, args.pr, args.commit, args.admission_run)
        if review.get('id') != submission['id']:
            raise ValueError('Reviewed artifact identity differs from the submitted plugin directory')
        approval_subject({'submission': submission}, review, os.environ.get('EXPECTED_CANDIDATE'))
        verify_review_archives(args.review.parent, review)
        previous = previous_version(fetch, review['id'])
        _, fingerprint = benchmark_evidence(args.impact, review, args.sha256, previous)
        if args.expected_evidence != fingerprint:
            raise ValueError('Benchmark evidence changed since the pre-approval summary')
        print('Exact source CI, review and benchmark evidence, and human maintainer approval verified.')
        return 0
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        # Never include HTTP headers or response bodies (which may contain private data).
        print(f'Admission refused: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
