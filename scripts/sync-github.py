#!/usr/bin/env python3
"""Plan or explicitly publish what this checkout holds for GitHub: main, each open extension submission, languages.

Without --publish nothing is written anywhere. With it, every write is a signed commit pushed
under an exact lease: `main` by the development snapshot of `deploy.py`, a submission branch by one
commit that replaces only `extension/<directory>/` with `plugins/<directory>/`, `languages` by a
fast-forward of its local branch. A branch this checkout holds no source for is never touched.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import tempfile


def script(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DEPLOY = script('deploy')
EXPORT = script('prepare-extension-branch')
Failure = DEPLOY.Failure
TARGETS = ('main', 'extensions', 'languages')
SUBMISSION = re.compile(r'refs/heads/submission/([a-z][a-z0-9_-]{0,63})-([0-9]{8})')
LANGUAGES = 'refs/heads/languages'
# Tests publish to a local remote without a signing key; nothing else changes these.
SIGN = ('-S',)


def git(repo, *args, env=None, stdin=None):
    result = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, input=stdin,
                            env=None if env is None else {**os.environ, **env})
    if result.returncode:
        raise Failure('Git operation failed: ' + args[0])
    return result.stdout.decode('utf-8').strip()


def holds(repo, revision):
    return subprocess.run(['git', '-C', str(repo), 'cat-file', '-e', revision],
                          capture_output=True).returncode == 0


def obtain(repo, remote, ref, oid):
    """The commit a remote branch points at, fetched when this checkout does not hold it; no local ref moves."""
    if not holds(repo, oid + '^{commit}'):
        git(repo, 'fetch', '--no-tags', remote, ref)
        if not holds(repo, oid + '^{commit}'):
            raise Failure(ref.removeprefix('refs/heads/') + ' moved while it was fetched')


def descends(repo, ancestor, descendant):
    return subprocess.run(['git', '-C', str(repo), 'merge-base', '--is-ancestor', ancestor, descendant],
                          capture_output=True).returncode == 0


def verify_signed(repo, commits):
    for commit in commits if SIGN else ():
        git(repo, 'verify-commit', commit)


def push(repo, remote, ref, old, new):
    """One leased push of a descendant of `old`, then the remote is read back."""
    git(repo, 'push', '--force-with-lease=' + ref + ':' + old, remote, new + ':' + ref)
    if DEPLOY.remote_refs(repo, remote).get(ref) != new:
        raise Failure('Remote verification failed for ' + ref.removeprefix('refs/heads/')
                      + '; inspect its actual state before retrying')


def local_branch(repo, ref):
    result = subprocess.run(['git', '-C', str(repo), 'rev-parse', '--verify', '--quiet', ref + '^{commit}'],
                            capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def parent_of(repo, remote, ref, remote_oid):
    """The commit a new one is added to: the remote tip, or the local branch when it only adds signed commits to it."""
    obtain(repo, remote, ref, remote_oid)
    local = local_branch(repo, ref)
    if local is None or local == remote_oid or descends(repo, local, remote_oid):
        return remote_oid
    if not descends(repo, remote_oid, local):
        raise Failure(ref.removeprefix('refs/heads/') + ' differs locally and on the remote; nothing was published')
    verify_signed(repo, git(repo, 'rev-list', remote_oid + '..' + local).split())
    return local


# --- main -------------------------------------------------------------------------------------------------------

def plan_main(repo, remote):
    prepared = DEPLOY.plan(repo, 'dev', remote)
    ref = 'refs/heads/' + prepared['branch']
    old = prepared['old']
    if old:
        obtain(repo, remote, ref, old)
        if git(repo, 'rev-parse', old + '^{tree}') == prepared['tree']:
            return None, prepared['branch'] + ': in sync'
    return prepared, (prepared['branch'] + ': publish the committed tree ' + prepared['tree'][:12]
                      + (' as a fresh signed root; previous tip kept as ' + prepared['backup'].removeprefix('refs/heads/')
                         if prepared['backup'] else ' as a signed commit'))


# --- extension submissions --------------------------------------------------------------------------------------

def submissions(repo, refs):
    """Each remote submission branch whose extension has a source directory here, as (ref, directory)."""
    found = {}
    for ref in sorted(refs):
        matched = SUBMISSION.fullmatch(ref)
        if not matched:
            continue
        directory = 'peersus-' + matched.group(1)
        if not (repo / 'plugins' / directory).is_dir():
            continue
        if directory in found:
            raise Failure('More than one submission branch exists for ' + directory + '; nothing was published')
        found[directory] = ref
    return [(ref, directory) for directory, ref in sorted(found.items())]


def local_entries(repo, directory, write):
    """`{path on the branch: (mode, blob)}` of the working copy of one extension."""
    root = Path('plugins') / directory
    try:
        paths = EXPORT.source_paths(repo, root)
        for path in paths:
            EXPORT.check_source(repo, repo / path)
    except ValueError as error:
        raise Failure(directory + ': ' + str(error)) from error
    if not paths:
        raise Failure(directory + ': no source files')
    if any('\n' in str(path) for path in paths):
        raise Failure(directory + ': unsupported file name')
    names = '\n'.join(str(path) for path in paths)
    blobs = git(repo, 'hash-object', *(('-w',) if write else ()), '--no-filters', '--stdin-paths',
                stdin=(names + '\n').encode('utf-8')).split()
    if len(blobs) != len(paths):
        raise Failure(directory + ': could not read every source file')
    entries = {}
    for path, blob in zip(paths, blobs):
        mode = '100755' if os.access(repo / path, os.X_OK) else '100644'
        entries[str(Path('extension') / directory / path.relative_to(root))] = (mode, blob)
    return entries


def branch_entries(repo, commit):
    entries = {}
    for line in git(repo, 'ls-tree', '-r', '-z', commit).split('\0'):
        if line:
            meta, path = line.split('\t', 1)
            mode, kind, blob = meta.split()
            entries[path] = (mode, kind, blob)
    return entries


def plan_submission(repo, remote, refs, ref, directory, write=False):
    parent = parent_of(repo, remote, ref, refs[ref])
    existing = branch_entries(repo, parent)
    prefix = 'extension/' + directory + '/'
    before = {path: (mode, blob) for path, (mode, _, blob) in existing.items() if path.startswith(prefix)}
    after = local_entries(repo, directory, write)
    changed = sorted(path for path in before.keys() | after.keys() if before.get(path) != after.get(path))
    branch = ref.removeprefix('refs/heads/')
    if not changed:
        if parent != refs[ref]:
            return {'ref': ref, 'old': refs[ref], 'parent': parent, 'entries': None}, \
                branch + ': push the local commits already made (source in sync)'
        return None, branch + ': in sync'
    kept = {path: entry for path, entry in existing.items() if not path.startswith(prefix)}
    return ({'ref': ref, 'old': refs[ref], 'parent': parent, 'directory': directory,
             'entries': {**kept, **{path: (mode, 'blob', blob) for path, (mode, blob) in after.items()}}},
            branch + ': ' + str(len(changed)) + ' file' + ('' if len(changed) == 1 else 's') + ' of '
            + directory + ' differ from plugins/' + directory)


def publish_submission(repo, remote, prepared, message):
    if DEPLOY.remote_refs(repo, remote).get(prepared['ref']) != prepared['old']:
        raise Failure(prepared['ref'].removeprefix('refs/heads/') + ' moved after planning; nothing was published')
    commit = prepared['parent']
    if prepared['entries'] is not None:
        with tempfile.TemporaryDirectory(prefix='sync-index-') as temp:
            index = {'GIT_INDEX_FILE': str(Path(temp) / 'index')}
            listing = ''.join(mode + ' ' + kind + ' ' + blob + '\t' + path + '\0'
                              for path, (mode, kind, blob) in sorted(prepared['entries'].items()))
            git(repo, 'update-index', '-z', '--index-info', env=index, stdin=listing.encode('utf-8'))
            tree = git(repo, 'write-tree', env=index)
        commit = git(repo, 'commit-tree', *SIGN, tree, '-p', prepared['parent'], '-m', message)
        verify_signed(repo, [commit])
    push(repo, remote, prepared['ref'], prepared['old'], commit)
    return commit


def default_message(directory):
    name = directory.removeprefix('peersus-')
    return name[:1].upper() + name[1:] + ': the source as it is in the development copy'


# --- languages --------------------------------------------------------------------------------------------------

def plan_languages(repo, remote, refs):
    local = local_branch(repo, LANGUAGES)
    if local is None:
        return None, 'languages: no local branch; nothing to publish'
    for block in git(repo, 'worktree', 'list', '--porcelain').split('\n\n'):
        lines = block.splitlines()
        if 'branch ' + LANGUAGES in lines:
            checkout = lines[0].removeprefix('worktree ')
            if git(checkout, 'status', '--porcelain'):
                raise Failure('languages: its checkout has uncommitted changes; commit them first')
    old = refs.get(LANGUAGES)
    if old == local:
        return None, 'languages: in sync'
    if old:
        obtain(repo, remote, LANGUAGES, old)
        if not descends(repo, old, local):
            raise Failure('languages: the local branch does not continue the remote one; nothing was published')
    new = git(repo, 'rev-list', (old + '..' if old else '') + local).split()
    verify_signed(repo, new)
    return ({'old': old or '', 'new': local},
            'languages: push ' + str(len(new)) + ' signed commit' + ('' if len(new) == 1 else 's'))


def publish_languages(repo, remote, prepared):
    if DEPLOY.remote_refs(repo, remote).get(LANGUAGES, '') != prepared['old']:
        raise Failure('languages moved after planning; nothing was published')
    push(repo, remote, LANGUAGES, prepared['old'], prepared['new'])


# --- entry ------------------------------------------------------------------------------------------------------

def synchronize(repo, remote, targets, publish, messages, say=print):
    """Plan every chosen target first; publish only after all of them planned. Returns whether all are in sync."""
    repo = Path(repo).resolve(strict=True)
    refs = DEPLOY.remote_refs(repo, remote)
    steps = []
    if 'main' in targets:
        prepared, line = plan_main(repo, remote)
        steps.append((line, prepared and (lambda p=prepared: DEPLOY.publish(repo, remote, p))))
    if 'extensions' in targets:
        known = set()
        for ref, directory in submissions(repo, refs):
            known.add(directory.removeprefix('peersus-'))
            prepared, line = plan_submission(repo, remote, refs, ref, directory, write=publish)
            message = messages.get(directory.removeprefix('peersus-'), default_message(directory))
            steps.append((line, prepared and (lambda p=prepared, m=message: publish_submission(repo, remote, p, m))))
        unknown = sorted(set(messages) - known)
        if unknown:
            raise Failure('No submission branch with local source for: ' + ', '.join(unknown))
    if 'languages' in targets:
        prepared, line = plan_languages(repo, remote, refs)
        steps.append((line, prepared and (lambda p=prepared: publish_languages(repo, remote, p))))
    pending = False
    for line, act in steps:
        say(line)
        pending = pending or bool(act)
    if not publish:
        say('Plan only: no commits or remote writes created; use --publish explicitly')
        return not pending
    for line, act in steps:
        if act:
            act()
            say('Published and verified: ' + line.split(':', 1)[0])
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--repo', type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument('--remote', default='origin')
    parser.add_argument('--only', action='append', choices=TARGETS, help='Limit to this target; repeatable')
    parser.add_argument('--message', action='append', default=[], metavar='ID=TEXT',
                        help='Commit message for one extension submission, e.g. upload="Upload: ..."')
    parser.add_argument('--publish', action='store_true', help='Authorize the described remote writes')
    args = parser.parse_args()
    try:
        messages = {}
        for item in args.message:
            name, separator, text = item.partition('=')
            if not separator or not text.strip() or name in messages:
                raise Failure('Use --message ID=TEXT once per extension')
            messages[name] = text.strip()
        synchronize(args.repo, args.remote, args.only or TARGETS, args.publish, messages)
    except (Failure, OSError, ValueError) as error:
        parser.exit(1, str(error) + '\n')


if __name__ == '__main__':
    main()
