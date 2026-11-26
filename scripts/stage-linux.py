#!/usr/bin/env python3
"""Stage Linux release binaries and metadata into a new directory; never replace an older build."""
import argparse
from pathlib import Path
import shutil
# The application ships no extension: extensions come only from the store or a manual install.
import os
from build_layout import cargo_artifact_directory
root = Path(__file__).resolve().parents[1]
build = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
if not build.is_absolute():
    build = root / build
release = cargo_artifact_directory("release")
if not release.is_absolute():
    release = root / release
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--out", type=Path, help="new staging directory (default: distribution/linux under the build target)")
out = parser.parse_args().out or build / "distribution/linux"
out.mkdir(parents=True, exist_ok=False)
for name in ("peersus", "peersus-ui", "peersus-wasm-host", "peersus-wasm-canvas-host", "peersus-native-host", "peersus-store-fetch", "peersus-diag"):
    target = out / "bin" / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(release / name, target)
for source, target in (
    ("packaging/linux/io.github.DarkPhilosophy.Peersus.desktop", "share/applications/io.github.DarkPhilosophy.Peersus.desktop"),
    ("packaging/linux/io.github.DarkPhilosophy.Peersus.metainfo.xml", "share/metainfo/io.github.DarkPhilosophy.Peersus.metainfo.xml"),
    ("crates/peersus-ui/assets/peersus-icon.svg", "share/icons/hicolor/scalable/apps/io.github.DarkPhilosophy.Peersus.svg"),
    ("LICENSE", "share/licenses/peersus/LICENSE"),
    ("NOTICE", "share/licenses/peersus/NOTICE"),
):
    destination = out / target
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(root / source, destination)
print(out)
