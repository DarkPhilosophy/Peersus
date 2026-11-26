#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("dependency-news.py")
spec = importlib.util.spec_from_file_location("dependency_news", SCRIPT)
news = importlib.util.module_from_spec(spec)
spec.loader.exec_module(news)

REGISTRY = 'source = "registry+https://github.com/rust-lang/crates.io-index"\n'


def registry(**crates):
    return lambda name: (
        None
        if crates.get(name) is None
        else {"versions": [{"num": v.rstrip("!"), "yanked": v.endswith("!")} for v in crates[name]]}
    )


class Precedence(unittest.TestCase):
    def test_a_pre_release_is_below_its_release_and_above_the_previous_one(self):
        order = ["0.5.2", "0.6.0-alpha.1", "0.6.0-alpha.2", "0.6.0-beta.1", "0.6.0"]
        self.assertEqual(sorted(reversed(order), key=news.key), order)
        self.assertLess(news.key("0.6.0-alpha.9"), news.key("0.6.0-alpha.10"))

    def test_an_exact_pin_or_a_caret_is_compared_by_its_version(self):
        self.assertTrue(news.behind("=0.5.2", "0.6.0-alpha.1"))
        self.assertFalse(news.behind("=0.6.0-alpha.1", "0.6.0-alpha.1"))
        self.assertFalse(news.behind("1", "1.0.0"))
        self.assertTrue(news.behind("1", "1.0.1"))


class Report(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        (self.root / "vendor/copy").mkdir(parents=True)
        (self.root / "vendor/copy/Cargo.toml").write_text('[package]\nname = "copied"\nversion = "1.0.0"\n')
        (self.root / "Cargo.toml").write_text(
            "[workspace.dependencies]\n"
            'toolkit = "=0.5.2"\n'
            'steady = { version = "2", features = ["x"] }\n'
            'renamed = { version = "1", package = "real-name" }\n'
            'local = { path = "crates/local" }\n'
            'gone = "1"\n'
            "[patch.crates-io]\n"
            'copied = { path = "vendor/copy" }\n'
        )
        # What is built: "steady" resolved to 2.1.3 although the requirement only says "2".
        (self.root / "Cargo.lock").write_text(
            f'[[package]]\nname = "steady"\nversion = "2.1.3"\n{REGISTRY}'
            f'[[package]]\nname = "toolkit"\nversion = "0.5.2"\n{REGISTRY}'
            f'[[package]]\nname = "real-name"\nversion = "1.0.0"\n{REGISTRY}'
            '[[package]]\nname = "local"\nversion = "9.0.0"\n'
        )

    def test_newer_releases_pre_releases_and_moved_vendored_copies_are_listed(self):
        text = news.report(
            self.root,
            registry(
                toolkit=["0.5.2", "0.6.0-alpha.1"],
                steady=["2.1.0", "2.1.3", "2.2.0", "3.0.0-rc.1!"],
                **{"real-name": ["1.0.0"]},
                copied=["1.0.0", "1.1.0"],
            ),
        )
        self.assertIn("| `toolkit` | `=0.5.2` | `0.5.2` | `0.6.0-alpha.1` (pre-release) | `0.5.2` |", text)
        # Compared with the locked 2.1.3, not with "2"; a yanked pre-release is not offered.
        self.assertIn("| `steady` | `2` | `2.1.3` | `2.2.0` (release) | `2.2.0` |", text)
        self.assertNotIn("real-name", text)
        self.assertNotIn("local", text)
        self.assertIn("| `copied` | `1.0.0` | `1.1.0` | `vendor/copy/PATCHES.md` |", text)
        # A crate the registry did not answer for is said, not dropped.
        self.assertIn("did not answer for: `gone`", text)

    def test_a_short_requirement_already_resolved_to_the_newest_is_not_listed(self):
        text = news.report(
            self.root,
            registry(
                toolkit=["0.5.2"],
                steady=["2.1.0", "2.1.3"],
                **{"real-name": ["1.0.0"]},
                gone=["1.0.0"],
                copied=["1.0.0"],
            ),
        )
        self.assertIn("Every dependency and vendored copy is at the newest release.", text)


if __name__ == "__main__":
    unittest.main()
