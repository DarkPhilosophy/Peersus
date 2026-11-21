#!/usr/bin/env python3
"""Generate only HTML-comment-delimited documentation; --check never writes.

Sources: Cargo.toml and the members' manifests, the folders under each member's src/, the
public items of the contract crate, rust-toolchain.toml, bundled plugin manifests, workflow
names, and docs/changes.json. No network, Git history, tokens, timestamps or publication.
"""
import argparse
import html
import json
import os
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def replace_block(text, name, body):
    start, end = f"<!-- {name}-START -->", f"<!-- {name}-END -->"
    if text.count(start) != 1 or text.count(end) != 1:
        raise ValueError(f"Expected exactly one pair of {name} markers")
    before, rest = text.split(start, 1)
    previous, after = rest.split(end, 1) if end in rest else (None, None)
    if previous is None:
        raise ValueError(f"Reversed {name} markers")
    return before + start + "\n" + body.rstrip() + "\n" + end + after


def markdown(value):
    if not isinstance(value, str) or not value.strip() or any(ord(c) < 32 for c in value):
        raise ValueError("Expected nonempty single-line documentation text")
    return html.escape(value, quote=False).replace("|", "&#124;")


def changes(root):
    data = json.loads((root / "docs/changes.json").read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != {"unreleased", "releases"}:
        raise ValueError("Unexpected changelog source shape")
    if not isinstance(data["releases"], list):
        raise ValueError("Expected a release list")
    sections = [("Unreleased", data["unreleased"])]
    versions = set()
    for release in data["releases"]:
        if not isinstance(release, dict) or set(release) != {"version", "date", "changes"}:
            raise ValueError("Unexpected release shape")
        version, date = release["version"], release["date"]
        if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version) or version in versions:
            raise ValueError("Invalid or duplicate release version")
        from datetime import date as calendar_date
        if not isinstance(date, str) or calendar_date.fromisoformat(date).isoformat() != date:
            raise ValueError("Invalid release date")
        versions.add(version)
        sections.append((f"{version} — {date}", release["changes"]))
    rendered = []
    for heading, groups in sections:
        if not isinstance(groups, dict):
            raise ValueError("Expected changelog categories")
        lines = [f"## {heading}"]
        if not groups:
            lines += ["", "No changes recorded."]
        for category, bullets in groups.items():
            if not isinstance(bullets, list) or not bullets:
                raise ValueError("Expected nonempty change entries")
            lines += ["", f"### {markdown(category)}", ""]
            lines += [f"- {markdown(bullet)}" for bullet in bullets]
        rendered.append("\n".join(lines))
    return "\n\n".join(rendered), rendered[0]


CONTRACT = "crates/peersus-api"


def uses(manifest, names):
    """Workspace crates a manifest builds against. Test-only dependencies are not architecture."""
    tables = [manifest.get("dependencies", {}), manifest.get("build-dependencies", {})]
    for target in manifest.get("target", {}).values():
        tables += [target.get("dependencies", {}), target.get("build-dependencies", {})]
    found = set()
    for table in tables:
        for key, value in table.items():
            name = value.get("package", key) if isinstance(value, dict) else key
            if name in names:
                found.add(name)
    return sorted(found)


def modules(source):
    """Top-level modules of a crate: its files and folders directly under src/."""
    names = set()
    for path in source.iterdir():
        if path.is_dir() and path.name != "bin" and any(path.glob("*.rs")):
            names.add(path.name)
        elif path.suffix == ".rs" and path.stem not in {"lib", "main", "tests"}:
            names.add(path.stem)
    return sorted(names)


def crates(root, members, page):
    """The workspace as Cargo declares it, lowest crate first."""
    packages = {}
    for member in members:
        manifest = tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))
        packages[manifest["package"]["name"]] = (member, manifest)
    needs = {name: uses(manifest, set(packages) - {name}) for name, (_, manifest) in packages.items()}
    depth = {}

    def level(name, path=()):
        if name in path:
            raise ValueError(f"Workspace crates depend on each other in a circle: {' -> '.join((*path, name))}")
        if name not in depth:
            depth[name] = 1 + max((level(other, (*path, name)) for other in needs[name]), default=0)
        return depth[name]

    ordered = sorted(packages, key=lambda name: (level(name), name))
    lines = ["| Crate | What it is, in its own manifest | Builds against, of this workspace |", "| --- | --- | --- |"]
    for name in ordered:
        member, manifest = packages[name]
        link = Path(os.path.relpath(root / member, page.parent)).as_posix()
        against = ", ".join(f"`{markdown(other)}`" for other in needs[name]) or "none"
        said = manifest["package"].get("description")
        if not said:
            raise ValueError(f"{member}/Cargo.toml does not say what the crate is (description)")
        lines.append(f"| [`{markdown(name)}`]({link}) | {markdown(said)} | {against} |")
    lines += ["", "Modules directly under each crate's `src/`:", ""]
    for name in ordered:
        found = modules(root / packages[name][0] / "src")
        lines.append(f"- `{markdown(name)}`: " + (", ".join(f"`{markdown(module)}`" for module in found) or "none"))
    return "\n".join(lines)


ITEM = re.compile(r"^pub (?:(?:const|async|unsafe) )*(fn) ([A-Za-z_]\w*)|^pub (struct|enum|trait|type|const|static) ([A-Za-z_]\w*)")


def sentence(doc):
    """The first sentence of a doc comment's first paragraph, or nothing."""
    paragraph = []
    for line in doc:
        if not line:
            break
        paragraph.append(line)
    text = " ".join(paragraph)
    end = re.search(r"[.:](?= |$)", text)
    return text[:end.start()] if end else text


def contract(root, page):
    """Every public item at the top of each module of the contract crate, with what it says it is."""
    source = root / CONTRACT / "src"
    lines = []
    for path in sorted(source.glob("*.rs")):
        if path.stem in {"lib", "main", "tests"}:
            continue
        items, doc = [], []
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.startswith("#[cfg(test)]"):
                break
            if line.startswith("///"):
                doc.append(line[3:].strip())
                continue
            if line.startswith("#["):
                continue
            match = ITEM.match(line)
            if match:
                kind, name = (match[1], match[2]) if match[1] else (match[3], match[4])
                said = sentence(doc)
                items.append(f"- {kind} `{markdown(name)}`" + (f": {markdown(said)}" if said else ""))
            doc = []
        if items:
            link = Path(os.path.relpath(path, page.parent)).as_posix()
            lines += [f"### `{markdown(path.stem)}` ([source]({link}))", "", *items, ""]
    if not lines:
        raise ValueError("The contract crate declares nothing public")
    return "\n".join(lines)


def outputs(root):
    readme_directory = (root / "README.md").resolve().parent

    def readme_link(path):
        return Path(os.path.relpath(path, readme_directory)).as_posix()

    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
    package = workspace["package"]
    status = (f"Workspace version: **{markdown(package['version'])}** (development; not a release claim). "
              f"Rust toolchain: **{markdown(toolchain)}**; declared MSRV: **{markdown(package['rust-version'])}**. "
              f"License: **{markdown(package['license'])}**.")
    plugins = ["| Extension in this workspace | Declared platforms | Exported services |", "| --- | --- | --- |"]
    for member in sorted(workspace["members"]):
        directory = root / member
        manifests = []
        # A platform guest belongs to its parent extension, not a second plugin.
        for implementation in (directory, directory / "wasm"):
            for suffix, parse in (("json", json.loads), ("toml", tomllib.loads)):
                path = implementation / f"plugin.{suffix}"
                if path.is_file():
                    manifests.append((path, parse(path.read_text(encoding="utf-8"))))
                    break
        if not manifests:
            continue
        platforms = ", ".join(markdown(platform) for platform in sorted({
            platform for _, manifest in manifests for platform in manifest.get("platforms", [])
        })) or "Not declared"
        services = ", ".join(f"`{markdown(service)}` v{version}" for service, version in sorted({
            (export["service"], export["version"])
            for _, manifest in manifests for export in manifest.get("exports", [])
        })) or "None"
        plugins.append(f"| [{markdown(Path(member).name)}]({readme_link(manifests[0][0])}) | {platforms} | {services} |")
    workflows = ["These are workflow definitions, **not live passing-status claims**.", ""]
    for path in sorted((root / ".github/workflows").glob("*.yml")):
        match = re.search(r"^name:\s*(.+)$", path.read_text(encoding="utf-8"), re.M)
        if not match:
            raise ValueError(f"Missing workflow name: {path.name}")
        workflows.append(f"- [{markdown(match[1].strip().strip(chr(39) + chr(34)))}]({readme_link(path)})")
    history, latest = changes(root)
    readme = (root / "README.md").read_text(encoding="utf-8")
    for name, body in {"STATUS": status, "PLUGINS": "\n".join(plugins), "CI": "\n".join(workflows), "LATEST": latest}.items():
        readme = replace_block(readme, name, body)
    changelog = replace_block((root / "CHANGELOG.md").read_text(encoding="utf-8"), "CHANGES", history)
    architecture = root / "docs/architecture.md"
    index = root / "docs/api/contract-index.md"
    return {
        root / "README.md": readme,
        root / "CHANGELOG.md": changelog,
        architecture: replace_block(architecture.read_text(encoding="utf-8"), "CRATES", crates(root, workspace["members"], architecture)),
        index: replace_block(index.read_text(encoding="utf-8"), "CONTRACT", contract(root, index)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Refuse stale generated blocks; write nothing")
    args = parser.parse_args()
    try:
        # Validate every output before writing any of them.
        rendered = outputs(ROOT)
        stale = [path for path, text in rendered.items() if path.read_text(encoding="utf-8") != text]
        if args.check:
            for path in stale:
                print(f"{path.relative_to(ROOT)} is stale; run python3 scripts/gen-docs.py", file=sys.stderr)
            return int(bool(stale))
        for path in stale:
            path.write_text(rendered[path], encoding="utf-8")
            print(f"Updated {path.relative_to(ROOT)}")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Documentation generation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
