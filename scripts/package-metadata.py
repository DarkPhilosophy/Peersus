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

def bundled_plugins():
    for path in sorted(path for path in (ROOT / "crates").glob("*/plugin.*") if path.name in ("plugin.json", "plugin.toml")):
        manifest = (json.loads if path.suffix == ".json" else tomllib.loads)(path.read_text())
        if manifest.get("platforms") is not None and "linux" not in manifest["platforms"]:
            continue
        programs = [manifest["program"], *manifest.get("files", [])]
        if any(not isinstance(name, str) or Path(name).name != name or name in (".", "..") for name in programs):
            raise SystemExit("bundled plugin program/files must be plain filenames")
        yield path, programs

# The programs the owner finds BESIDE its own executable, by name. One list, here: `check` proves the Rust code that
# looks for them names exactly these, and that every surface that stages binaries ships every one. A shipping list that
# forgets one makes the feature that needs it fail only at run time, on a user's machine.
HELPERS = ("peersus-wasm-host", "peersus-native-host", "peersus-store-fetch")
HELPER_SOURCES = ("crates/peersus-plugin-host/src/module.rs", "crates/peersus-app/src/store_runtime.rs")

def flatpak_commands(plugins):
    packages = " ".join("-p " + shlex.quote(path.parent.name) for path, _ in plugins)
    commands = [
        "rustc --version", f"cargo build --locked --release -p peersus-ui -p peersus-app -p peersus-plugin-runtime -p peersus-store-fetch {packages}",
        "cargo run --locked -p xtask -- native-host",
        "install -Dm755 target/release/peersus-ui /app/bin/peersus-ui",
        "install -Dm755 target/release/peersus /app/bin/peersus",
        *(f"install -Dm755 target/release/{name} /app/bin/{name}" for name in HELPERS),
        "command -v zenity",
        f"install -Dm644 packaging/linux/{ID}.desktop /app/share/applications/{ID}.desktop",
        f"install -Dm644 packaging/linux/{ID}.metainfo.xml /app/share/metainfo/{ID}.metainfo.xml",
        f"install -Dm644 crates/peersus-ui/assets/peersus-icon.svg /app/share/icons/hicolor/scalable/apps/{ID}.svg",
    ]
    for path, programs in plugins:
        package = path.parent.name.removeprefix("peersus-")
        destination = f"/app/share/peersus/plugins/{package}"
        for program in programs:
            commands.append(f"install -Dm755 {shlex.quote('target/release/' + program)} {shlex.quote(destination + '/' + program)}")
        commands.append(f"install -Dm644 {shlex.quote(str(path.relative_to(ROOT)))} {shlex.quote(destination + '/' + path.name)}")
    return commands

def check_helpers():
    named_in_code = set()
    for source in HELPER_SOURCES:
        named_in_code |= set(re.findall(r"(peersus-(?:wasm-host|native-host|store-fetch))\{", (ROOT / source).read_text()))
    if named_in_code != set(HELPERS):
        raise SystemExit(f"helpers looked for in the code {sorted(named_in_code)} differ from HELPERS {sorted(HELPERS)}")
    commands = flatpak_commands(list(bundled_plugins()))
    for name in HELPERS:
        if not any(command.startswith(f"install -Dm755 target/release/{name} ") for command in commands):
            raise SystemExit(f"the flatpak does not install {name}")
    if "-p peersus-store-fetch" not in commands[1]:
        raise SystemExit("the flatpak build does not compile peersus-store-fetch")
    staged = (ROOT / "scripts/stage-linux.py").read_text()
    for name in HELPERS:
        if name not in staged:
            raise SystemExit(f"scripts/stage-linux.py does not ship {name}")
    if '"-p" "peersus-store-fetch"' not in (ROOT / "packaging/nix/source-package.nix").read_text():
        raise SystemExit("packaging/nix/source-package.nix does not build peersus-store-fetch")

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
    for path, programs in bundled_plugins():
        if not programs:
            raise SystemExit(f"bundled plugin has no program: {path}")
    check_helpers()

def stage(destination):
    # Fresh output only: never traverse build output, backups, home or user state.
    destination.mkdir(parents=True, exist_ok=False)
    allowed = ["Cargo.toml", "Cargo.lock", "LICENSE", "NOTICE", "crates", "vendor", "xtask", ".cargo", "packaging/linux"]
    for name in allowed:
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

def flatpak():
    check()
    # No prebuilt distro binary crosses into this runtime: compile in the SDK ABI.
    output = ROOT / "target/distribution"
    output.mkdir(parents=True, exist_ok=True)
    import tempfile
    workspace = Path(tempfile.mkdtemp(prefix="flatpak-", dir=output))
    source = workspace / "source"
    stage(source)
    commands = flatpak_commands(list(bundled_plugins()))
    manifest = {
        "app-id": ID, "runtime": "org.freedesktop.Platform", "runtime-version": "26.08",
        "sdk": "org.freedesktop.Sdk", "sdk-extensions": ["org.freedesktop.Sdk.Extension.rust-stable", "org.freedesktop.Sdk.Extension.llvm22"],
        "command": "peersus-ui", "build-options": {
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
    print("Import the bundled plugin through the application and explicitly approve it; packaging never approves code.")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("check", "flatpak"))
    args = parser.parse_args()
    try:
        {"check": check, "flatpak": flatpak}[args.task]()
    except (OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
