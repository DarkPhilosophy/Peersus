#!/usr/bin/env python3
"""Fold every open Dependabot pull request into ONE commit on the `dev` branch.

Run by `.github/workflows/dependency-roll.yml`. `dev` is made from `main` the first time. Each open
pull request opened by Dependabot against `main` or `dev` is squashed onto `dev` in turn; one that
does not apply cleanly is left out and named, never resolved by guessing. What applied becomes a single
commit authored and committed as the owner named in the repository variables `DEV_COMMIT_NAME` and
`DEV_COMMIT_EMAIL` (no identity is guessed: without them nothing is done). `dev` is pushed without force,
each folded pull request gets a comment naming the commit (Dependabot closes it itself once `dev` holds
the update), and the quality gate is started on `dev`.

Nothing is merged into `main`, tagged or published.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from typing import Callable

BRANCH = "dev"

Run = Callable[..., subprocess.CompletedProcess]


def run(*args: str, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(args, check=check, text=True, capture_output=capture)


def output(*args: str) -> str:
    return run(*args, capture=True).stdout.strip()


def open_pull_requests() -> list[dict]:
    listed = output(
        "gh", "pr", "list", "--state", "open", "--author", "app/dependabot", "--limit", "100",
        "--json", "number,title,headRefName,baseRefName",
    )
    return json.loads(listed or "[]")


def plan(pull_requests: list[dict]) -> list[dict]:
    """The main code line only, oldest first: `extension` keeps its own workflows and is never folded into `dev`."""
    return sorted((pr for pr in pull_requests if pr["baseRefName"] in ("main", BRANCH)), key=lambda pr: pr["number"])


def message(folded: list[dict], left_out: list[dict]) -> str:
    lines = [f"Dependency updates ({len(folded)} pull request{'s' if len(folded) != 1 else ''} folded)", ""]
    lines += [f"- #{pr['number']} {pr['title']}" for pr in folded]
    if left_out:
        lines += ["", "Left out (did not apply cleanly on dev):"]
        lines += [f"- #{pr['number']} {pr['title']}" for pr in left_out]
    return "\n".join(lines) + "\n"


def fold(pull_requests: list[dict], name: str, email: str) -> tuple[list[dict], list[dict]]:
    """Make `dev` locally: one commit holding every pull request that applies, in the owner's name."""
    # The commit's author and committer, through git's own environment variables (the runner has no identity of its own).
    os.environ.update(GIT_AUTHOR_NAME=name, GIT_AUTHOR_EMAIL=email, GIT_COMMITTER_NAME=name, GIT_COMMITTER_EMAIL=email)
    commit = ["git", "commit", "--no-gpg-sign", "-q"]
    run("git", "fetch", "-q", "--no-tags", "origin", "+refs/heads/*:refs/remotes/origin/*")
    exists = run("git", "rev-parse", "--verify", "--quiet", f"origin/{BRANCH}", check=False, capture=True).returncode == 0
    run("git", "checkout", "-q", "-B", BRANCH, f"origin/{BRANCH}" if exists else "origin/main")
    start = output("git", "rev-parse", "HEAD")
    folded, left_out = [], []
    for pr in plan(pull_requests):
        squashed = run("git", "merge", "--squash", f"origin/{pr['headRefName']}", check=False, capture=True)
        if squashed.returncode != 0:
            run("git", "reset", "-q", "--hard", "HEAD")
            left_out.append(pr)
            continue
        if run("git", "diff", "--cached", "--quiet", check=False).returncode != 0:
            # A step of its own until the end, so the next pull request applies on top of this one.
            run(*commit, "-m", f"#{pr['number']}")
            folded.append(pr)
    if folded:
        run("git", "reset", "-q", "--soft", start)
        run(*commit, "-m", message(folded, left_out))
    return folded, left_out


def publish(folded: list[dict], left_out: list[dict], call: Run = run) -> None:
    """Push `dev` (never forced), tell each folded pull request where it went, and start the gate there."""
    call("git", "push", "origin", f"HEAD:refs/heads/{BRANCH}")
    commit = call("git", "rev-parse", "HEAD", capture=True).stdout.strip()
    for pr in folded:
        call("gh", "pr", "comment", str(pr["number"]), "--body", f"Folded into `{BRANCH}` at {commit}.", check=False)
    call("gh", "workflow", "run", "ci.yml", "--ref", BRANCH)


def main() -> int:
    name, email = os.environ.get("DEV_COMMIT_NAME", ""), os.environ.get("DEV_COMMIT_EMAIL", "")
    if not name or not email:
        print("DEV_COMMIT_NAME and DEV_COMMIT_EMAIL must be set as repository variables; nothing done.")
        return 1
    folded, left_out = fold(open_pull_requests(), name, email)
    if not folded:
        print(message(folded, left_out) if left_out else "No Dependabot pull request to fold.")
        return 0
    publish(folded, left_out)
    print(message(folded, left_out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
