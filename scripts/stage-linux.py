#!/usr/bin/env python3
"""Stage release binaries with metadata and the unapproved bundled plugin packages."""
from pathlib import Path
import shutil
import json
import tomllib
import os
from build_layout import cargo_artifact_directory
root = Path(__file__).resolve().parents[1]
build = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
if not build.is_absolute():
    build = root / build
release = cargo_artifact_directory("release")
if not release.is_absolute():
    release = root / release
out = build / "distribution/linux"
out.mkdir(parents=True, exist_ok=False)
for name in ("peersus", "peersus-ui", "peersus-wasm-host", "peersus-native-host", "peersus-store-fetch"):
    target = out / "bin" / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(release / name, target)
for manifest_path in sorted(path for path in (root / "crates").glob("*/plugin.*") if path.name in ("plugin.json", "plugin.toml")):
    manifest = (json.loads if manifest_path.suffix == ".json" else tomllib.loads)(manifest_path.read_text())
    if manifest.get("platforms") is not None and "linux" not in manifest["platforms"]:
        continue
    package = manifest_path.parent.name.removeprefix("peersus-")
    plugin = out / "share/peersus/plugins" / package
    plugin.mkdir(parents=True)
    for program in [manifest["program"], *manifest.get("files", [])]:
        if Path(program).name != program or program in (".", ".."):
            raise SystemExit("bundled plugin program/files must be plain filenames")
        shutil.copy2(release / program, plugin / program)
    shutil.copy2(manifest_path, plugin / manifest_path.name)
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
