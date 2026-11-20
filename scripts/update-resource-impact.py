#!/usr/bin/env python3
"""Update only the benchmark section of the PR description; never post a comment.

The workflow invokes this trusted tool with its pull-request write token, not with code
from the measured revision. Only a live PR for the measured revision is eligible.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

START = '<!-- peersus-benchmark:start -->'
END = '<!-- peersus-benchmark:end -->'
LIMIT = 65_536


def replace_section(author, report):
    if any(marker in report for marker in (START, END)):
        raise ValueError('Benchmark report contains reserved PR markers')
    section = f'{START}\n{report.rstrip()}\n{END}'
    start, end = author.find(START), author.find(END)
    if start == end == -1:
        gap = ('' if not author or author.endswith('\n\n') else '\n' if author.endswith('\n') else '\n\n')
        updated = author + gap + section
    elif start >= 0 and end > start and author.count(START) == author.count(END) == 1:
        updated = author[:start] + section + author[end + len(END):]
    else:
        raise ValueError('PR has ambiguous or incomplete benchmark markers; its description was not changed')
    if len(updated.encode('utf-8')) > LIMIT:
        raise ValueError('Benchmark would exceed the PR body limit; its description was not changed')
    return updated


def update(get, patch, head, base, report):
    def current(pr):
        return pr['state'] == 'open' and pr['head']['sha'] == head and (base is None or pr['base']['sha'] == base)

    pr = get()
    if not current(pr):
        return False
    author = pr['body'] or ''
    updated = replace_section(author, report)
    if updated == author:
        return False
    # GitHub replaces the entire body. Do not overwrite a concurrent description edit
    # that the second read can see, or attach a report to a stale head/base.
    latest = get()
    if not current(latest) or (latest['body'] or '') != author:
        return False
    patch(updated)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--pr', type=int, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--head', required=True, help='The measured PR head SHA')
    parser.add_argument('--base', help='The measured application PR base SHA')
    args = parser.parse_args()
    try:
        if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository) or args.pr <= 0
                or not re.fullmatch(r'[0-9a-f]{40}', args.head)
                or (args.base is not None and not re.fullmatch(r'[0-9a-f]{40}', args.base))):
            raise ValueError('Invalid repository, pull request or measured commit')
        if not args.report.is_file():
            print('No report was written; the PR body was not modified.')
            return 0

        def api(*arguments, stdin=None):
            return subprocess.run(['gh', 'api', *arguments], input=stdin, check=True, capture_output=True, text=True,
                                  timeout=60).stdout

        route = f'repos/{args.repository}/pulls/{args.pr}'
        def get():
            return json.loads(api(route))

        def patch(updated):
            api('--method', 'PATCH', route, '--input', '-', stdin=json.dumps({'body': updated}))

        report = args.report.read_text()
        # The top summary and per-program table go in the PR; the full table is in the
        # job summary and run artifact. Avoid exceeding GitHub's limit with detail rows.
        details = re.search(r'\n<details><summary>Every compared number .*?</details>\n', report, flags=re.S)
        if details:
            report = report[:details.start()] + '\nSee the benchmark job summary for all sampled metrics.\n' + report[details.end():]
        if update(get, patch, args.head, args.base, report):
            print('Updated the marked benchmark section of the PR description.')
        else:
            print('No update: the report is unchanged, the PR moved/closed, or its description was edited concurrently.')
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'The PR description could not be updated: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
