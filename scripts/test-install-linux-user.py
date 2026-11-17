#!/usr/bin/env python3
"""Run the real user install against a made-up staged tree and a made-up prefix."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install-linux-user.py")
ID = "io.github.DarkPhilosophy.Peersus"
ENTRY = f"share/applications/{ID}.desktop"
PROGRAMS = ("peersus", "peersus-ui", "peersus-wasm-host")


def staged(root, entry="[Desktop Entry]\nName=Peersus\nExec=peersus-ui\nIcon=" + ID + "\n"):
    stage = root / "stage"
    for name in PROGRAMS:
        path = stage / "bin" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("program:" + name)
        path.chmod(0o755)
    for name, text in ((ENTRY, entry), (f"share/metainfo/{ID}.metainfo.xml", "metadata"), (f"share/icons/hicolor/scalable/apps/{ID}.svg", "icon")):
        path = stage / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return stage


def install(stage, prefix):
    return subprocess.run([sys.executable, str(SCRIPT), str(stage), "--prefix", str(prefix)], capture_output=True, text=True)


class Install(unittest.TestCase):
    def test_everything_staged_is_put_in_the_prefix_and_the_entry_starts_the_installed_window(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage, prefix = staged(root), root / "prefix"
            result = install(stage, prefix)
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in PROGRAMS:
                self.assertEqual((prefix / "bin" / name).read_text(), "program:" + name)
                self.assertTrue(os.access(prefix / "bin" / name, os.X_OK))
            self.assertEqual((prefix / f"share/metainfo/{ID}.metainfo.xml").read_text(), "metadata")
            self.assertEqual((prefix / f"share/icons/hicolor/scalable/apps/{ID}.svg").read_text(), "icon")
            entry = (prefix / ENTRY).read_text().splitlines()
            # Only the line that starts the window differs from what was staged.
            self.assertEqual(entry, ["[Desktop Entry]", "Name=Peersus", f"Exec={prefix}/bin/peersus-ui", "Icon=" + ID])
            self.assertFalse(os.access(prefix / ENTRY, os.X_OK))

    def test_the_address_a_link_opens_the_window_with_is_kept_on_the_rewritten_line(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage = staged(root, entry="[Desktop Entry]\nExec=peersus-ui %u\nMimeType=x-scheme-handler/peersus;\n")
            prefix = root / "prefix"
            result = install(stage, prefix)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"Exec={prefix}/bin/peersus-ui %u", (prefix / ENTRY).read_text().splitlines())

    def test_a_second_install_replaces_what_is_there_and_leaves_everything_else_alone(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage, prefix = staged(root), root / "prefix"
            self.assertEqual(install(stage, prefix).returncode, 0)
            other = prefix / "bin/another-program"
            other.write_text("not ours")
            # A program that is running holds the file it was started from: the new one must be a new file, not the old one rewritten.
            before = (prefix / "bin/peersus").stat().st_ino
            held = open(prefix / "bin/peersus")
            (stage / "bin/peersus").write_text("program:newer")
            self.assertEqual(install(stage, prefix).returncode, 0)
            self.assertEqual((prefix / "bin/peersus").read_text(), "program:newer")
            self.assertNotEqual((prefix / "bin/peersus").stat().st_ino, before)
            self.assertEqual(held.read(), "program:peersus")
            held.close()
            self.assertEqual(other.read_text(), "not ours")
            # Nothing is left behind of the files written on the way.
            self.assertEqual(sorted(path.name for path in (prefix / "bin").iterdir()), sorted((*PROGRAMS, "another-program")))

    def test_a_prefix_with_a_space_is_quoted_in_the_entry(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage, prefix = staged(root), root / "my prefix"
            self.assertEqual(install(stage, prefix).returncode, 0)
            self.assertIn(f'Exec="{prefix}/bin/peersus-ui"', (prefix / ENTRY).read_text().splitlines())

    def test_an_entry_that_starts_something_else_or_a_tree_that_is_not_whole_installs_nothing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage = staged(root, entry="[Desktop Entry]\nExec=sh -c something\n")
            result = install(stage, root / "prefix")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "prefix").exists())
            (stage / "bin/peersus-ui").unlink()
            result = install(stage, root / "other")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "other").exists())


if __name__ == "__main__":
    unittest.main()
