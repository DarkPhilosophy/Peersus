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
PROGRAMS = ("peersus", "peersus-ui", "peersus-launch", "peersus-wasm-host", "peersus-wasm-canvas-host")
LAUNCHER = SCRIPT.parent.parent / "packaging/linux/peersus-launch"


def staged(root, entry="[Desktop Entry]\nName=Peersus\nExec=peersus-launch\nIcon=" + ID + "\n"):
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
            self.assertEqual(entry, ["[Desktop Entry]", "Name=Peersus", f"Exec={prefix}/bin/peersus-launch", "Icon=" + ID])
            self.assertFalse(os.access(prefix / ENTRY, os.X_OK))

    def test_the_address_a_link_opens_the_window_with_is_kept_on_the_rewritten_line(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage = staged(root, entry="[Desktop Entry]\nExec=peersus-launch %u\nMimeType=x-scheme-handler/peersus;\n")
            prefix = root / "prefix"
            result = install(stage, prefix)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"Exec={prefix}/bin/peersus-launch %u", (prefix / ENTRY).read_text().splitlines())

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
            self.assertIn(f'Exec="{prefix}/bin/peersus-launch"', (prefix / ENTRY).read_text().splitlines())

    def test_the_installed_launcher_applies_the_allocator_policy_and_passes_window_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage, prefix = staged(root), root / "my prefix"
            (stage / "bin/peersus-launch").write_bytes(LAUNCHER.read_bytes())
            (stage / "bin/peersus-ui").write_text(
                '#!/bin/sh\nprintf "%s\\n" "$MALLOC_MMAP_THRESHOLD_" "$@"\n'
            )
            self.assertEqual(install(stage, prefix).returncode, 0)
            for extra, threshold in [({}, "4194304"), ({"MALLOC_MMAP_THRESHOLD_": "8388608"}, "8388608")]:
                result = subprocess.run(
                    [prefix / "bin/peersus-launch", "a b", "peersus://store/sample"],
                    env={"PATH": os.defpath, **extra},
                    capture_output=True,
                    text=True,
                    check=True,
                )
                self.assertEqual(result.stdout.splitlines(), [threshold, "a b", "peersus://store/sample"])

    def test_an_entry_that_starts_something_else_or_a_tree_that_is_not_whole_installs_nothing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage = staged(root, entry="[Desktop Entry]\nExec=sh -c something\n")
            result = install(stage, root / "prefix")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "prefix").exists())
            stage = staged(root / "incomplete")
            (stage / "bin/peersus-launch").unlink()
            result = install(stage, root / "other")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "other").exists())


if __name__ == "__main__":
    unittest.main()
