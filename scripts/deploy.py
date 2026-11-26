#!/usr/bin/env python3
"""Plan or explicitly publish signed committed-source snapshots, never stage files."""
import argparse
from datetime import datetime, timezone
from pathlib import Path
import re
import subprocess
import tomllib


class Failure(Exception):
    pass


def git(repo, *args):
    result = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, text=True)
    if result.returncode:
        raise Failure('Git operation failed: ' + args[0] + '; no unsigned fallback is permitted')
    return result.stdout.strip()


def destination(mode, released, requested=None):
    branch = requested or ('main' if mode == 'release' or not released else 'dev')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._/-]*', branch):
        raise Failure('Invalid destination branch')
    if mode == 'release' and branch != 'main':
        raise Failure('Releases must target main')
    if mode == 'dev' and released and branch == 'main':
        raise Failure('After the first release, main is reserved for releases; use dev')
    return branch


def remote_refs(repo, remote):
    refs = {}
    for line in git(repo, 'ls-remote', '--heads', '--tags', remote).splitlines():
        oid, ref = line.split()
        if not re.fullmatch(r'[0-9a-f]{40}', oid):
            raise Failure('Unsupported remote object identity')
        refs[ref] = oid
    return refs


def plan(repo, mode, remote, target=None, version=None):
    if git(repo, 'diff', '--name-only', 'HEAD', '--'):
        raise Failure('Commit tracked changes before publication; this tool never stages files')
    source = git(repo, 'rev-parse', 'HEAD')
    tree = git(repo, 'rev-parse', 'HEAD^{tree}')
    refs = remote_refs(repo, remote)
    released = any(re.fullmatch(r'refs/tags/v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?', ref) for ref in refs)
    branch = destination(mode, released, target)
    git(repo, 'check-ref-format', '--branch', branch)
    if mode == 'release':
        if not version or not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?', version):
            raise Failure('Release requires an explicit vMAJOR.MINOR.PATCH tag')
        if 'refs/tags/' + version in refs:
            raise Failure('The release tag already exists; never replace a release')
        manifest = tomllib.loads(git(repo, 'show', 'HEAD:Cargo.toml'))
        if manifest['workspace']['package']['version'] != version.removeprefix('v'):
            raise Failure('Release tag must match the committed workspace version')
    elif version:
        raise Failure('Development snapshots do not create release tags')
    old = refs.get('refs/heads/' + branch)
    # Before release, main is a fresh one-commit development snapshot. The first
    # release starts its own history; subsequent release/dev commits append.
    fresh = branch == 'main' and not released
    backup = 'refs/heads/backup/main-' + old[:7] if old and fresh else None
    if backup and backup in refs and refs[backup] != old:
        raise Failure('The backup name is already used for a different commit')
    return {'source': source, 'tree': tree, 'refs': refs, 'branch': branch, 'old': old,
            'fresh': fresh, 'backup': backup, 'version': version, 'mode': mode}


def open_pull_requests(repo, branch):
    """Open pull requests whose base is `branch`; a fresh root closes every one of them for good."""
    try:
        result = subprocess.run(['gh', 'pr', 'list', '--base', branch, '--state', 'open', '--json', 'number',
                                 '--jq', '.[].number'], cwd=repo, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        result = None
    if result is None or result.returncode:
        raise Failure('Open pull requests on ' + branch + ' could not be listed; nothing was published')
    return [line for line in result.stdout.split() if line]


def publish(repo, remote, prepared):
    # Revalidate the lease immediately before any publication, not only at plan time.
    current = remote_refs(repo, remote)
    ref = 'refs/heads/' + prepared['branch']
    if current.get(ref) != prepared['old']:
        raise Failure('Destination moved after planning; nothing was published')
    if prepared['version'] and 'refs/tags/' + prepared['version'] in current:
        raise Failure('Release tag appeared after planning; nothing was published')
    if prepared['fresh'] and prepared['old']:
        # Replacing the branch with an unrelated root makes GitHub close every PR based on it,
        # and such a PR can no longer be reopened. Push fixes to the PR's own branch instead.
        targeting = open_pull_requests(repo, prepared['branch'])
        if targeting:
            raise Failure('Open pull requests target ' + prepared['branch'] + ' (#' + ', #'.join(targeting)
                          + '); replacing it would close them; nothing was published')
    backup = prepared['backup']
    if backup and current.get(backup, prepared['old']) != prepared['old']:
        raise Failure('The backup name now belongs to a different commit; nothing was published')
    parent = prepared['old'] if not prepared['fresh'] else None
    if prepared['old']:
        # Fetch the exact branch for either its append parent or backup object,
        # without moving the caller branch, index or working tree.
        git(repo, 'fetch', '--no-tags', remote, ref)
        if git(repo, 'rev-parse', 'FETCH_HEAD') != prepared['old']:
            raise Failure('Destination moved while fetching its previous tip')
    args = ['commit-tree', '-S', prepared['tree']]
    if parent:
        args += ['-p', parent]
    message = ('Release ' + prepared['version'] if prepared['mode'] == 'release'
               else 'Development snapshot')
    commit = git(repo, *args, '-m', message)
    git(repo, 'verify-commit', commit)
    push = ['push', '--atomic', '--force-with-lease=' + ref + ':' + (prepared['old'] or '')]
    refspecs = [commit + ':' + ref]
    if backup:
        push += ['--force-with-lease=' + backup + ':' + current.get(backup, '')]
        refspecs.append(prepared['old'] + ':' + backup)
    tag = prepared['version']
    temporary_tag = None
    if tag:
        # A unique local tag is only a staging reference. The remote receives the
        # requested public name; no existing local or remote release is replaced.
        temporary_tag = 'publish-' + datetime.now(timezone.utc).strftime('%Y%m%d%H%M%S%f')
        git(repo, 'tag', '-s', temporary_tag, commit, '-m', 'Release ' + tag)
        git(repo, 'verify-tag', temporary_tag)
        push += ['--force-with-lease=refs/tags/' + tag + ':']
        refspecs.append('refs/tags/' + temporary_tag + ':refs/tags/' + tag)
    try:
        git(repo, *push, remote, *refspecs)
        actual = remote_refs(repo, remote)
        if (actual.get(ref) != commit
                or (backup and actual.get(backup) != prepared['old'])
                or (tag and actual.get('refs/tags/' + tag + '^{}') != commit)):
            raise Failure('Remote verification failed; inspect actual state before retrying')
    finally:
        if temporary_tag:
            git(repo, 'tag', '-d', temporary_tag)
    return commit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('dev', 'release'))
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument('--remote', default='origin')
    parser.add_argument('--target', help='Development destination; defaults to main before release, dev afterwards')
    parser.add_argument('--version', help='Explicit signed release tag, e.g. v0.1.0')
    parser.add_argument('--publish', action='store_true', help='Authorize the described atomic remote write')
    args = parser.parse_args()
    try:
        prepared = plan(args.repo, args.mode, args.remote, args.target, args.version)
        print('Mode:', args.mode)
        print('Destination:', prepared['branch'])
        print('History:', 'fresh signed root with atomic backup' if prepared['fresh'] else 'signed append')
        if prepared['backup']:
            print('Preserve previous main:', prepared['old'], 'as', prepared['backup'])
        print('Source: committed HEAD only; index and working tree are never changed')
        if not args.publish:
            print('Plan only: no commits, tags or remote writes created; use --publish explicitly')
            return
        publish(args.repo, args.remote, prepared)
        print('Published and verified; source checkout left unchanged')
    except (Failure, OSError, ValueError) as error:
        parser.exit(1, str(error) + '\n')


if __name__ == '__main__':
    main()
