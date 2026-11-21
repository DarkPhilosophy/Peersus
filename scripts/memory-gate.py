#!/usr/bin/env python3
"""Compare the memory of this computer's running Peersus processes with a recorded baseline.

Roles: the window (`peersus-ui`), the owner (`peersus`) and one entry per module helper, including `peersus-wasm-canvas-host`.
For each, RSS (what a task manager shows), PSS and private dirty memory are read from /proc/<pid>/smaps_rollup.

    scripts/memory-gate.py --record target/memory-baseline.json   # store the current numbers as the baseline
    scripts/memory-gate.py --check target/memory-baseline.json    # exit 1 if any role grew past the tolerance

A role missing from the baseline is reported, not failed. Linux only; read-only; prints no paths or identifiers beyond the role.
"""
import argparse
import json
import os
import pathlib
import sys

TOLERANCE_MIB = 2.0  # absolute slack for normal noise (small helpers must not double unnoticed)
TOLERANCE_RATIO = 0.15  # and relative slack


def roles() -> dict[str, int]:
    found: dict[str, int] = {}
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        program = os.path.basename(argv[0].decode(errors="replace")) if argv and argv[0] else ""
        if program == "peersus-ui":
            found["window"] = int(entry.name)
        elif program == "peersus":
            found["owner"] = int(entry.name)
        elif program.startswith(("peersus-wasm-host", "peersus-wasm-canvas-host")) or program == "peersus-native-host":
            module = argv[1].decode(errors="replace") if len(argv) > 1 else ""
            parts = pathlib.PurePath(module).parts
            plugin = parts[-2] if len(parts) >= 2 else "unknown"
            found[f"extension:{plugin}"] = int(entry.name)
    return found


def measure(pid: int) -> dict[str, float]:
    values: dict[str, float] = {}
    with open(f"/proc/{pid}/smaps_rollup") as rollup:
        for line in rollup:
            key, _, rest = line.partition(":")
            if key in ("Rss", "Pss", "Private_Dirty"):
                values[key.lower()] = int(rest.split()[0]) / 1024
    return values


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--record", metavar="BASELINE")
    action.add_argument("--check", metavar="BASELINE")
    args = parser.parse_args()
    now = {role: measure(pid) for role, pid in sorted(roles().items())}
    if not now:
        raise SystemExit("no running Peersus process")
    if args.record:
        pathlib.Path(args.record).write_text(json.dumps(now, indent=2) + "\n")
        for role, v in now.items():
            print(f"{role:28} rss {v['rss']:7.1f}  pss {v['pss']:7.1f}  private {v['private_dirty']:7.1f} MiB")
        return
    baseline = json.loads(pathlib.Path(args.check).read_text())
    failed = False
    for role, v in now.items():
        base = baseline.get(role)
        if base is None:
            print(f"{role:28} rss {v['rss']:7.1f} MiB  (no baseline)")
            continue
        limit = base["rss"] + max(TOLERANCE_MIB, base["rss"] * TOLERANCE_RATIO)
        verdict = "ok" if v["rss"] <= limit else "GREW"
        failed |= verdict != "ok"
        print(f"{role:28} rss {v['rss']:7.1f} vs {base['rss']:7.1f} (limit {limit:7.1f})  pss {v['pss']:7.1f}  {verdict}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
