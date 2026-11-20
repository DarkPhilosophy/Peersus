#!/usr/bin/env python3
"""Put a staged Linux tree (scripts/stage-linux.py) in a user's own prefix, so that the desktop finds the application.

Programs go to <prefix>/bin, each replaced in one step, so a program that is running keeps the file it was started
from. Everything under the tree's share/ goes beside them: the desktop entry, the metadata, the icon, the licences.
The entry's Exec line is rewritten to name the installed window by its full path, because a user's bin directory is
not on the path of every desktop session.

Nothing else is touched: no data, no preferences and no extension is installed, approved or removed, and nothing is
started or stopped. Whoever runs this restarts the application.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ID = "io.github.DarkPhilosophy.Peersus"
WINDOW = "peersus-ui"
LAUNCHER = "peersus-launch"
ENTRY = Path("share/applications") / f"{ID}.desktop"
# What an Exec argument may hold unquoted is narrower than this in the specification; quoting more is always allowed.
PLAIN = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_./+@:,")


def exec_argument(path):
    """One argument of an Exec line, quoted as the Desktop Entry Specification says when it has to be."""
    text = str(path)
    if set(text) <= PLAIN:
        return text
    for special in ("\\", '"', "`", "$"):
        text = text.replace(special, "\\" + special)
    # A backslash inside a quoted argument is written twice in the file: once for the entry, once for the argument.
    return '"' + text.replace("\\", "\\\\") + '"'


def entry_for(text, launcher):
    lines = text.splitlines()
    execs = [index for index, line in enumerate(lines) if line.startswith("Exec=")]
    # The window, with the address the system opens it with (`%u`, a store link) when the entry passes one.
    forms = {f"Exec={LAUNCHER}": "", f"Exec={LAUNCHER} %u": " %u"}
    if len(execs) != 1 or lines[execs[0]] not in forms:
        raise SystemExit(f"the desktop entry does not start {LAUNCHER} in the way this script rewrites")
    lines[execs[0]] = "Exec=" + exec_argument(launcher) + forms[lines[execs[0]]]
    return "\n".join(lines) + "\n"


def put(content, target, mode):
    """Write `content` (bytes) as `target` in one step: whoever has the old file open keeps it whole."""
    target.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix="." + target.name + ".", dir=target.parent)
    try:
        with os.fdopen(descriptor, "wb") as file:
            file.write(content)
        os.chmod(temporary, mode)
        os.replace(temporary, target)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise


def install(stage, prefix):
    for needed in (Path("bin") / WINDOW, Path("bin/peersus"), Path("bin") / LAUNCHER, ENTRY):
        if not (stage / needed).is_file():
            raise SystemExit(f"{stage / needed} is missing: run scripts/stage-linux.py first")
    # Read before anything is written: an entry this cannot rewrite installs nothing.
    entry = entry_for((stage / ENTRY).read_text(), prefix / "bin" / LAUNCHER).encode()
    installed = []
    for source in sorted((stage / "bin").iterdir()):
        if source.is_symlink() or not source.is_file():
            raise SystemExit(f"{source} is not a plain file")
        put(source.read_bytes(), prefix / "bin" / source.name, 0o755)
        installed.append(Path("bin") / source.name)
    for source in sorted(path for path in (stage / "share").rglob("*") if not path.is_dir()):
        if source.is_symlink():
            raise SystemExit(f"{source} is a symbolic link")
        relative = source.relative_to(stage)
        content = entry if relative == ENTRY else source.read_bytes()
        put(content, prefix / relative, 0o755 if os.access(source, os.X_OK) else 0o644)
        installed.append(relative)
    # The desktop reads the entries from a cache where it has one. Not every system has the tool, and an entry that is
    # in place is found without it after the next login.
    refresh = shutil.which("update-desktop-database")
    if refresh:
        subprocess.run([refresh, str(prefix / "share/applications")], check=False, capture_output=True)
    return installed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("stage", type=Path, help="the tree scripts/stage-linux.py printed")
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local", help="where to install (default: ~/.local)")
    arguments = parser.parse_args()
    for path in install(arguments.stage.resolve(), arguments.prefix.expanduser().resolve()):
        print(path)
