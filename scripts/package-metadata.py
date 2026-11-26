#!/usr/bin/env python3
"""Validate metadata or build a local self-hosted Flatpak from source inside its SDK."""
import argparse
import json
import re
import tomllib
from pathlib import Path
import shutil
import shlex
import subprocess
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
ID = "io.github.DarkPhilosophy.Peersus"

def run(*args, **kwargs):
    subprocess.run(args, check=True, cwd=ROOT, **kwargs)

# The programs the owner finds BESIDE its own executable, by name. One list, here: `check` proves the Rust code that
# looks for them names exactly these, and that every surface that stages binaries ships every one. A shipping list that
# forgets one makes the feature that needs it fail only at run time, on a user's machine.
HELPERS = ("peersus-wasm-host", "peersus-wasm-canvas-host", "peersus-native-host", "peersus-store-fetch", "peersus-diag")
HELPER_SOURCES = ("crates/peersus-plugin/src/module.rs", "crates/peersus-app/src/store_runtime.rs", "crates/peersus-diagnostics/src/crash_watch.rs")

# The application ships no extension: extensions come only from the store or a manual install.
def flatpak_commands():
    commands = [
        "rustc --version", "cargo build --locked --release -p peersus-ui -p peersus-app -p peersus-plugin-runtime -p peersus-plugin-canvas -p peersus-store-fetch -p peersus-diag",
        "cargo run --locked -p xtask -- native-host",
        "install -Dm755 target/release/peersus-ui /app/bin/peersus-ui",
        "install -Dm755 target/release/peersus /app/bin/peersus",
        *(f"install -Dm755 target/release/{name} /app/bin/{name}" for name in HELPERS),
        "command -v zenity",
        f"install -Dm644 packaging/linux/{ID}.desktop /app/share/applications/{ID}.desktop",
        f"install -Dm644 packaging/linux/{ID}.metainfo.xml /app/share/metainfo/{ID}.metainfo.xml",
        f"install -Dm644 crates/peersus-ui/assets/peersus-icon.svg /app/share/icons/hicolor/scalable/apps/{ID}.svg",
    ]
    return commands

def check_helpers():
    named_in_code = set()
    for source in HELPER_SOURCES:
        named_in_code |= set(re.findall(r"(peersus-(?:wasm-(?:canvas-)?host|native-host|store-fetch|diag))\{", (ROOT / source).read_text()))
    if named_in_code != set(HELPERS):
        raise SystemExit(f"helpers looked for in the code {sorted(named_in_code)} differ from HELPERS {sorted(HELPERS)}")
    commands = flatpak_commands()
    for name in HELPERS:
        if not any(command.startswith(f"install -Dm755 target/release/{name} ") for command in commands):
            raise SystemExit(f"the flatpak does not install {name}")
    for package in ("peersus-plugin-runtime", "peersus-plugin-canvas", "peersus-store-fetch", "peersus-diag"):
        if f"-p {package}" not in commands[1]:
            raise SystemExit(f"the flatpak build does not compile {package}")
    staged = (ROOT / "scripts/stage-linux.py").read_text()
    for name in HELPERS:
        if name not in staged:
            raise SystemExit(f"scripts/stage-linux.py does not ship {name}")
    nix = (ROOT / "packaging/nix/source-package.nix").read_text()
    for package in ("peersus-plugin-runtime", "peersus-plugin-canvas", "peersus-store-fetch", "peersus-diag"):
        if f'"-p" "{package}"' not in nix:
            raise SystemExit(f"packaging/nix/source-package.nix does not build {package}")

def check():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    xml = ET.parse(ROOT / f"packaging/linux/{ID}.metainfo.xml").getroot()
    if xml.findtext("id") != ID or xml.find("releases/release").get("version") != version:
        raise SystemExit("AppStream id/version differs from the workspace")
    for command in ("appstreamcli", "desktop-file-validate"):
        if not shutil.which(command):
            raise SystemExit(f"required metadata validator missing: {command}")
    run("appstreamcli", "validate", "--no-net", str(ROOT / f"packaging/linux/{ID}.metainfo.xml"))
    run("desktop-file-validate", str(ROOT / f"packaging/linux/{ID}.desktop"))
    check_helpers()

# What the Flatpak build gets of this source: everything Cargo reads to build the workspace, nothing else.
SOURCE_PARTS = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "LICENSE", "NOTICE", "build-support", "crates", "vendor", "xtask", ".cargo", "packaging/linux"]

def stage(destination):
    # Fresh output only: never traverse build output, backups, home or user state.
    destination.mkdir(parents=True, exist_ok=False)
    for name in SOURCE_PARTS:
        source = ROOT / name
        paths = [source, *source.rglob("*")] if source.is_dir() else [source]
        for path in paths:
            if path.is_symlink():
                raise SystemExit(f"source snapshot refuses symbolic link: {path.relative_to(ROOT)}")
            if path.name.startswith(".env") or path.suffix in (".pem", ".key"):
                raise SystemExit("source snapshot contains credential-like file")
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_dir():
            shutil.copytree(source, target)
        else:
            shutil.copy2(source, target)
    # A build script that includes a file by `#[path]` from outside its crate fails only deep in the Flatpak build: refused here.
    for script in sorted(destination.glob("crates/*/build.rs")):
        for included in re.findall(r'#\[path\s*=\s*"([^"]+)"\]', script.read_text()):
            if not (script.parent / included).is_file():
                raise SystemExit(f"{script.relative_to(destination)} includes {included}, which the source copy lacks")

def flatpak():
    check()
    # No prebuilt distro binary crosses into this runtime: compile in the SDK ABI.
    output = ROOT / "target/distribution"
    output.mkdir(parents=True, exist_ok=True)
    import tempfile
    workspace = Path(tempfile.mkdtemp(prefix="flatpak-", dir=output))
    source = workspace / "source"
    stage(source)
    commands = flatpak_commands()
    manifest = {
        "app-id": ID, "runtime": "org.freedesktop.Platform", "runtime-version": "26.08",
        "sdk": "org.freedesktop.Sdk", "sdk-extensions": ["org.freedesktop.Sdk.Extension.rust-stable", "org.freedesktop.Sdk.Extension.llvm22"],
        "command": "peersus", "build-options": {
            "append-path": "/usr/lib/sdk/rust-stable/bin:/usr/lib/sdk/llvm22/bin",
            "env": {"FORCE_SKIA_BUILD": "1", "LIBCLANG_PATH": "/usr/lib/sdk/llvm22/lib"},
            "build-args": ["--share=network"]},
        "finish-args": ["--share=network", "--share=ipc", "--socket=wayland", "--socket=fallback-x11", "--device=dri", "--talk-name=org.freedesktop.secrets"],
        # Freedesktop 26.08 supplies /usr/bin/zenity for rfd Linux message dialogs.
        # Verify it in the actual build sandbox instead of assuming host availability.
        "modules": [{"name": "peersus", "buildsystem": "simple", "build-commands": commands,
                     "sources": [{"type": "dir", "path": str(source)}]}],
    }
    manifest_path = workspace / f"{ID}.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    run("flatpak-builder", "--user", "--disable-rofiles-fuse", "--repo=" + str(workspace / "repo"), str(workspace / "build"), str(manifest_path))
    run("flatpak", "build-bundle", str(workspace / "repo"), str(workspace / "Peersus.flatpak"), ID)
    print(f"Local unsigned bundle and repository: {workspace}")
    print("Not a Flathub submission. No upload, installation or runtime validation performed.")
    print("Clipboard: Wayland data-control may be unavailable in a sandbox; X11 access is not equivalent isolation.")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("check", "flatpak"))
    args = parser.parse_args()
    try:
        {"check": check, "flatpak": flatpak}[args.task]()
    except (OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
