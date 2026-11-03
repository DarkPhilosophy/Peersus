#!/usr/bin/env python3
"""Emit the exact embedded-metadata record for a plugin from its manifest's `package` block.

    embed-metadata.py header plugin.json   -> C header defining PEERSUS_METADATA_BYTES
    embed-metadata.py bin    plugin.json   -> the raw record (magic, u32 LE length, JSON)

The record is the SAME `package` block the manifest carries, so the manifest stays the single source
and the binary cannot drift from it. Reads one file, writes stdout, executes nothing from the plugin.
"""
import json
import struct
import sys
from pathlib import Path

MAGIC = b"PSUSMETA"
MAX_RECORD = 16 * 1024
ALLOWED = {"name", "version", "description", "changelog", "author", "link"}


def record(manifest_path: Path) -> bytes:
    if not manifest_path.is_file() or manifest_path.is_symlink():
        raise SystemExit("manifest must be a regular file")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    package = manifest.get("package")
    if not isinstance(package, dict) or not {"name", "version"} <= package.keys():
        raise SystemExit("manifest has no valid `package` block")
    if set(package) - ALLOWED:
        raise SystemExit("`package` has a key the host would refuse")
    link = package.get("link", "")
    if link and not link.startswith("https://"):
        raise SystemExit("`package.link` must be https")
    body = json.dumps(package, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()
    if not 0 < len(body) <= MAX_RECORD:
        raise SystemExit("record is empty or too large")
    return MAGIC + struct.pack("<I", len(body)) + body


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[1] not in ("header", "bin"):
        raise SystemExit(__doc__)
    data = record(Path(sys.argv[2]))
    if sys.argv[1] == "bin":
        sys.stdout.buffer.write(data)
        return
    body = ",".join(str(byte) for byte in data)
    print(f"#define PEERSUS_METADATA_BYTES {body}")


if __name__ == "__main__":
    main()
