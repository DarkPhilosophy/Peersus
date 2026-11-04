#!/usr/bin/env python3
"""Validate one extension submission WITHOUT executing it, and print its review report as JSON.

    validate-extension.py <package-dir-or-.tar.gz> [--report report.json]

Used by the pull-request check (untrusted input, no secrets) and by the maintainer's approval job, so
the rule exists once. It never imports, loads or runs anything from the package. It checks structure
and identity, and prints the archive SHA-256 the maintainer approves.

What it enforces:
  * a plain tar/tar.gz with no links, devices, absolute or parent paths, bounded entries and size;
  * exactly one manifest, plugin.json. The host also reads plugin.toml, but store submissions are JSON
    only on purpose: one format means one thing for the maintainer to review. It must carry a valid
    `package` block (name, version, author, https link) and no manifest keys the host would refuse;
  * the package root directory name is the plugin id and equals a lowercase `name.valid` identifier;
  * the manifest's `program` exists and the embedded record in that file equals `package`.
"""
import hashlib
import io
import json
import re
import struct
import sys
import tarfile
from pathlib import Path
from extension_format import wasm_custom_sections

MAX_FILES = 512
MAX_TOTAL = 128 * 1024 * 1024
MAX_RECORD = 16 * 1024
MAGIC = b"PSUSMETA"
ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
PACKAGE_KEYS = {"name", "version", "description", "changelog", "author", "link"}
CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f]")


def fail(message: str):
    raise SystemExit(f"refused: {message}")


def read_members(path: Path) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    total = 0
    with tarfile.open(path, "r:*") as archive:
        for member in archive:
            name = member.name
            parts = Path(name).parts
            if (
                name.startswith("/") or "\\" in name or ".." in parts or CONTROL.search(name)
                or not (member.isfile() or member.isdir())
            ):
                fail(f"unsafe archive entry {name!r}")
            if member.isdir():
                continue
            if len(files) >= MAX_FILES:
                fail("too many files")
            total += member.size
            if total > MAX_TOTAL:
                fail("package too large")
            extracted = archive.extractfile(member)
            if name in files:
                fail("the same file twice")
            files[name] = extracted.read(member.size) if extracted else b""
    return files


def wasm_custom(data: bytes, wanted: str):
    try:
        return wasm_custom_sections(data, wanted)
    except ValueError:
        fail("invalid WebAssembly container")


def elf_section(data: bytes, wanted: str):
    if data[:4] != b"\x7fELF" or data[5] != 1:  # little-endian only here; the host handles the rest
        return None
    is64 = data[4] == 2
    if is64:
        shoff, = struct.unpack_from("<Q", data, 0x28)
        shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    else:
        shoff, = struct.unpack_from("<I", data, 0x20)
        shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x2E)

    def header(i):
        base = shoff + i * shentsize
        if is64:
            name, _, _, _, offset, size = struct.unpack_from("<IIQQQQ", data, base)
        else:
            name, _, _, _, offset, size = struct.unpack_from("<IIIIII", data, base)
        return name, offset, size

    _, str_off, str_size = header(shstrndx)
    strings = data[str_off:str_off + str_size]
    found = []
    for i in range(shnum):
        name, offset, size = header(i)
        end = strings.find(b"\0", name)
        if strings[name:end].decode("utf-8", "replace") == wanted:
            found.append(data[offset:offset + size])
    return found


def embedded(program: bytes):
    sections = wasm_custom(program, "peersus.meta")
    if sections is None:
        sections = elf_section(program, "peersus.meta")
    if sections is None:
        fail("program is not a recognised binary here (ELF or WebAssembly); other formats are read by the host")
    if len(sections) != 1:
        fail("program must carry exactly one peersus.meta record")
    record = sections[0]
    if not record.startswith(MAGIC):
        fail("embedded record has no magic")
    length, = struct.unpack_from("<I", record, 8)
    if not 0 < length <= MAX_RECORD or len(record) < 12 + length:
        fail("embedded record length is not valid")
    if any(record[12 + length:]):
        fail("embedded record has trailing data")
    return json.loads(record[12:12 + length])


def check_package(package) -> None:
    if not isinstance(package, dict) or not {"name", "version"} <= package.keys():
        fail("manifest has no valid `package` block")
    if set(package) - PACKAGE_KEYS:
        fail("`package` has a key the host would refuse")
    if not VERSION.match(package["version"]):
        fail("version must be major.minor.patch")
    for key in ("name", "author", "description", "changelog"):
        if key in package and (not isinstance(package[key], str) or CONTROL.search(package[key])):
            fail(f"{key} is not plain text")
    link = package.get("link", "")
    if not link.startswith("https://") or any(c.isspace() for c in link) or len(link) > 2048:
        fail("`package.link` must be one https address")
    if "@" in link.split("/")[2] or "?" in link or "#" in link:
        fail("`package.link` must have no credentials, query or fragment")
    if not package.get("author"):
        fail("`package.author` is required for the store")


def main() -> None:
    args = [a for a in sys.argv[1:] if not a.startswith("--report")]
    if len(args) != 1:
        raise SystemExit(__doc__)
    source = Path(args[0])
    if source.is_symlink() or not source.exists():
        fail("source must exist and not be a symbolic link")
    if source.is_dir():
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as tar:
            tar.add(source, arcname=source.name, recursive=True)
        archive_bytes = buffer.getvalue()
        path = source
        files = {str(p.relative_to(source.parent)): p.read_bytes() for p in source.rglob("*") if p.is_file() and not p.is_symlink()}
    else:
        archive_bytes = source.read_bytes()
        files = read_members(source)
    roots = {Path(name).parts[0] for name in files}
    if len(roots) != 1:
        fail("the package must have exactly one top directory")
    plugin_id = roots.pop()
    if not ID.match(plugin_id) or plugin_id.startswith("."):
        fail("the top directory name is not a valid plugin id")
    manifest_bytes = files.get(f"{plugin_id}/plugin.json")
    if manifest_bytes is None or f"{plugin_id}/plugin.toml" in files:
        fail("exactly one manifest, plugin.json, is required")
    manifest = json.loads(manifest_bytes)
    package = manifest.get("package")
    check_package(package)
    # Only formats the isolated module host runs. A legacy `process` package (no `execution`) would run
    # directly with the user's rights, so the store never distributes one. Mirrors the application.
    execution = manifest.get("execution")
    if (
        not isinstance(execution, dict)
        or execution.get("kind") not in ("native_library", "wasm")
        or execution.get("abi") != 1
        or set(execution) != {"kind", "abi"}
    ):
        fail("`execution` must be {\"kind\": \"native_library\" or \"wasm\", \"abi\": 1}")
    program_name = manifest.get("program", "plugin")
    if "/" in program_name or program_name in ("", ".", ".."):
        fail("program must be one plain file name")
    program = files.get(f"{plugin_id}/{program_name}")
    if program is None:
        fail("the manifest's program is missing")
    record = embedded(program)
    if record != package:
        fail("the program's embedded metadata differs from the manifest `package` block")
    report = {
        "id": plugin_id,
        "version": package["version"],
        "author": package["author"],
        "link": package["link"],
        "archive_sha256": hashlib.sha256(archive_bytes).hexdigest(),
        "archive_size": len(archive_bytes),
        "files": len(files),
        "capabilities": sorted(manifest.get("capabilities", {})),
        "description": package.get("description", ""),
    }
    text = json.dumps(report, indent=2, sort_keys=True)
    for arg in sys.argv[1:]:
        if arg.startswith("--report="):
            Path(arg.split("=", 1)[1]).write_text(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
