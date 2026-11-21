#!/usr/bin/env python3
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

SCRIPT = Path(__file__).with_name("dependency-roll.py")
spec = importlib.util.spec_from_file_location("dependency_roll", SCRIPT)
roll = importlib.util.module_from_spec(spec)
spec.loader.exec_module(roll)


def git(cwd, *args):
    result = subprocess.run(["git", *args], cwd=cwd, text=True, capture_output=True)
    if result.returncode != 0:
        raise AssertionError(f"git {' '.join(args)}: {result.stderr}")
    return result.stdout.strip()


PRS = [
    {"number": 13, "title": "Bump serde from 1.0.1 to 1.0.3", "headRefName": "dependabot/cargo/serde-other", "baseRefName": "main"},
    {"number": 11, "title": "Bump serde from 1.0.1 to 1.0.2", "headRefName": "dependabot/cargo/serde", "baseRefName": "main"},
    {"number": 12, "title": "Bump checkout from 6 to 7", "headRefName": "dependabot/actions/checkout", "baseRefName": "main"},
    {"number": 14, "title": "Bump an extension action", "headRefName": "dependabot/x", "baseRefName": "extension"},
]


class Fold(unittest.TestCase):
    def setUp(self):
        # The "origin" is a plain local repository that is only fetched from: nothing is pushed anywhere.
        self.cwd, self.environ = os.getcwd(), dict(os.environ)
        self.base = Path(tempfile.mkdtemp())
        # The machine's own identity when it has one (the test overrides none); a CI runner has none, so a placeholder.
        configured = [
            subprocess.run(["git", "config", "--get", key], text=True, capture_output=True).stdout.strip()
            for key in ("user.name", "user.email")
        ]
        if all(configured):
            self.owner = tuple(configured)
        else:
            self.owner = ("Peersus test", "test@example.invalid")
            os.environ.update(
                GIT_AUTHOR_NAME=self.owner[0], GIT_AUTHOR_EMAIL=self.owner[1],
                GIT_COMMITTER_NAME=self.owner[0], GIT_COMMITTER_EMAIL=self.owner[1],
            )
        self.origin = self.base / "origin"
        git(self.base, "init", "-q", "-b", "main", str(self.origin))
        (self.origin / "Cargo.toml").write_text('serde = "1.0.1"\nrand = "0.10.1"\n')
        (self.origin / "ci.yml").write_text("uses: checkout@v6\n")
        git(self.origin, "add", ".")
        git(self.origin, "commit", "--no-gpg-sign", "-q", "-m", "main")
        # Three Dependabot branches: two that apply, one that edits the same line as the first.
        for branch, path, text in [
            ("dependabot/cargo/serde", "Cargo.toml", 'serde = "1.0.2"\nrand = "0.10.1"\n'),
            ("dependabot/actions/checkout", "ci.yml", "uses: checkout@v7\n"),
            ("dependabot/cargo/serde-other", "Cargo.toml", 'serde = "1.0.3"\nrand = "0.10.1"\n'),
        ]:
            git(self.origin, "checkout", "-q", "-B", branch, "main")
            (self.origin / path).write_text(text)
            git(self.origin, "commit", "--no-gpg-sign", "-q", "-am", branch)
        git(self.origin, "checkout", "-q", "main")
        self.work = self.base / "work"
        git(self.base, "clone", "-q", str(self.origin), str(self.work))
        os.chdir(self.work)

    def tearDown(self):
        os.chdir(self.cwd)
        os.environ.clear()
        os.environ.update(self.environ)

    def test_the_pull_requests_that_apply_become_one_commit_on_dev_in_the_owners_name(self):
        name, email = self.owner
        folded, left_out = roll.fold(PRS, name, email)
        self.assertEqual([pr["number"] for pr in folded], [11, 12])
        # Oldest first: #13 edits the line #11 already changed, so it is left out and named, not merged by guessing.
        self.assertEqual([pr["number"] for pr in left_out], [13])
        self.assertEqual(git(self.work, "rev-parse", "--abbrev-ref", "HEAD"), "dev")
        self.assertEqual(git(self.work, "rev-list", "--count", "origin/main..dev"), "1")
        author = git(self.work, "log", "-1", "--format=%an\n%ae\n%cn\n%ce", "dev").splitlines()
        # Compared without printing: the identity is a person's.
        self.assertTrue(author == [name, email, name, email], "author or committer is not the owner given")
        body = git(self.work, "log", "-1", "--format=%B", "dev")
        self.assertIn("- #11 Bump serde from 1.0.1 to 1.0.2\n- #12 Bump checkout from 6 to 7", body)
        self.assertIn("Left out (did not apply cleanly on dev):\n- #13", body)
        self.assertNotIn("#14", body)
        self.assertEqual(git(self.work, "show", "dev:Cargo.toml"), 'serde = "1.0.2"\nrand = "0.10.1"')
        self.assertEqual(git(self.work, "show", "dev:ci.yml"), "uses: checkout@v7")
        self.assertEqual(git(self.work, "status", "--porcelain"), "")

    def test_with_nothing_that_applies_no_commit_is_made(self):
        name, email = self.owner
        folded, left_out = roll.fold([PRS[3]], name, email)
        self.assertEqual((folded, left_out), ([], []))
        self.assertEqual(git(self.work, "rev-list", "--count", "origin/main..dev"), "0")


class Publish(unittest.TestCase):
    def test_dev_is_pushed_without_force_each_folded_request_is_told_and_the_gate_started(self):
        calls = []

        def call(*args, **kwargs):
            calls.append(list(args))
            return SimpleNamespace(stdout="abc123\n", returncode=0)

        roll.publish(PRS[1:3], [PRS[0]], call)
        self.assertEqual(calls[0], ["git", "push", "origin", "HEAD:refs/heads/dev"])
        self.assertFalse(any("--force" in c or "-f" in c for c in calls))
        self.assertIn(["gh", "pr", "comment", "11", "--body", "Folded into `dev` at abc123."], calls)
        self.assertIn(["gh", "pr", "comment", "12", "--body", "Folded into `dev` at abc123."], calls)
        self.assertNotIn("13", [c[3] for c in calls if c[:3] == ["gh", "pr", "comment"]])
        self.assertEqual(calls[-1], ["gh", "workflow", "run", "ci.yml", "--ref", "dev"])

    def test_without_the_owners_identity_nothing_is_done(self):
        saved = {k: os.environ.pop(k) for k in ("DEV_COMMIT_NAME", "DEV_COMMIT_EMAIL") if k in os.environ}
        try:
            self.assertEqual(roll.main(), 1)
        finally:
            os.environ.update(saved)


if __name__ == "__main__":
    unittest.main()
