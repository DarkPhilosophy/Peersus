#!/usr/bin/env python3
"""Prepare the source-only extension tree without changing Git refs or publishing."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

PLUGINS = ("peersus-clipboard", "peersus-upload", "peersus-control")


def source_paths(repo: Path, root: Path) -> list[Path]:
    """Every file of one extension's source directory, relative to `repo`, without build output."""
    paths = []
    for directory, children, files in os.walk(repo / root, followlinks=False):
        children[:] = sorted(name for name in children if name not in {"target", ".git", ".backup", "__pycache__"})
        for name in children:
            if (Path(directory) / name).is_symlink():
                raise ValueError("source directories must not be symbolic links")
        paths.extend((Path(directory) / name).relative_to(repo) for name in sorted(files))
    return paths


def check_source(repo: Path, source: Path) -> None:
    """Validate an input before anything is created. Never dereference source links
    or accidentally export local credentials alongside source files."""
    name = source.name.lower()
    if (name.startswith(".env") or name in {".netrc", ".npmrc", "id_rsa", "id_ed25519"}
            or any(word in name for word in ("token", "api_key", "private_key", "secret"))
            or name.endswith((".pem", ".key"))):
        raise ValueError("credential-like source filename cannot be exported")
    if not source.is_file() or source.is_symlink():
        raise ValueError("source inputs must be regular files")
    # System-level home aliases are allowed; links inside the repository are
    # not, since they can redirect source reads outside the selected scope.
    current = source.parent
    while current != repo:
        if current.is_symlink():
            raise ValueError("source directories must not be symbolic links")
        current = current.parent


def prepare(repo: Path, destination: Path) -> None:
    repo = repo.resolve(strict=True)
    if destination.is_symlink():
        raise ValueError("output must not be a symbolic link")
    destination = destination.resolve()
    if destination.exists():
        raise ValueError("output must not already exist")
    if destination.is_relative_to(repo):
        raise ValueError("output must be outside the application checkout")
    destination.parent.resolve(strict=True)
    sources = [(repo / "scripts/extension-branch/README.md", Path("README.md")),
               (repo / ".github/PULL_REQUEST_TEMPLATE/extension.md",
                Path(".github/PULL_REQUEST_TEMPLATE.md")),
               (repo / ".github/workflows/extension-validate.yml",
                Path(".github/workflows/extension-validate.yml")),
               (repo / ".github/workflows/triage.yml", Path(".github/workflows/triage.yml")),
               (repo / "LICENSE", Path("LICENSE")), (repo / "NOTICE", Path("NOTICE"))]
    for plugin in PLUGINS:
        root = Path("plugins") / plugin
        paths = source_paths(repo, root)
        native = all(root / name in paths for name in ("Cargo.toml", "plugin.json"))
        guest = all(root / "wasm" / name in paths for name in ("Cargo.toml", "plugin.json"))
        if not (native or guest) or any(root / name not in paths for name in ("submission.toml", "ownership.json")):
            raise ValueError(f"incomplete source plugin: {plugin}")
        for path in paths:
            relative = path.relative_to(root)
            sources.append((repo / path, Path("extension") / plugin / relative))
    for source, _ in sources:
        check_source(repo, source)
    with tempfile.TemporaryDirectory(prefix="extension-tree-", dir=destination.parent) as temp:
        tree = Path(temp) / "tree"
        tree.mkdir()
        for source, relative in sources:
            target = tree / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o755 if os.access(source, os.X_OK) else 0o644)
        # Reserve the name without overwriting an existing tree, even if empty.
        destination.mkdir()
        for item in tree.iterdir():
            item.rename(destination / item.name)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        prepare(args.repo, args.out)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"extension preparation failed: {error}\n")
    print("Prepared independent plugin source directories, submission workflow and README; no Git refs changed.")


if __name__ == "__main__":
    main()
