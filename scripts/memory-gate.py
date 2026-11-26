#!/usr/bin/env python3
"""Compare one Linux Peersus installation/profile with an explicitly recorded baseline.

Reads procfs and installed executable/module files. The operator supplies window
and peer conditions, not the application. All memory is in KiB.
Exit status: 0 passed/recorded, 1 grew, 2 inconclusive, 3 legacy baseline.
"""
import argparse
from collections import defaultdict
from dataclasses import dataclass, replace
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import time

VERSION = 3
SAMPLES = 5
INTERVAL_S = 15
MIN_AGE_S = 300
AGE_SLACK_S = 120
ABSOLUTE_SLACK_KIB = 2048
RELATIVE_SLACK_PERCENT = 15
PROGRAMS = ("peersus", "peersus-ui", "peersus-wasm-host", "peersus-wasm-canvas-host", "peersus-native-host")
PAGES = ("send", "receive", "peers", "transfers", "settings", "extension")
CANVAS = ("unused", "opened-and-closed", "open")
RENDERERS = ("software", "opengl", "vulkan")
CONDITIONS = ("page", "extension_pages", "canvas", "paired", "online", "renderer", "window_size")
WINDOW_SIZE = re.compile(r"([1-9][0-9]{0,4})x([1-9][0-9]{0,4})\Z")
MAX_MODULE_BYTES = 256 * 1024 * 1024  # Like the installed-program fingerprint in peersus-app.
MMAP_KEY = b"MALLOC_MMAP_THRESHOLD_="
FIXED_MMAP = MMAP_KEY + b"4194304"
MEMORY = {"Rss": "rss_kib", "Pss": "pss_kib", "Pss_Anon": "anon_kib",
          "SwapPss": "swap_pss_kib", "AnonHugePages": "anon_huge_kib"}
DIGEST = re.compile(r"[0-9a-f]{64}\Z")


class Inconclusive(Exception):
    """A fixed, privacy-safe reason, never raw procfs contents or a filename."""


class LegacyBaseline(Exception):
    pass


class Parser(argparse.ArgumentParser):
    def error(self, _message):
        self.exit(2, "invalid arguments; see --help\nRESULT=INCONCLUSIVE\n")


@dataclass(frozen=True)
class Process:
    pid: int
    parent: int
    started: int
    program: str
    module: tuple[Path, tuple[int, ...]] | None = None


def process_ids(entry):
    # comm can contain spaces and parentheses; the last ')' ends field 2.
    fields = (entry / "stat").read_text(errors="replace").rsplit(")", 1)[1].split()
    return int(fields[1]), int(fields[19])


def executable_identity(info):
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns


def profile_lock(entry, lock):
    try:
        for fd in (entry / "fd").iterdir():
            try:
                if fd.readlink() != lock:
                    continue
                for line in (entry / "fdinfo" / fd.name).read_text().splitlines():
                    if line.startswith("lock:") and line.split()[2:6] == ["FLOCK", "ADVISORY", "WRITE", entry.name]:
                        return True
            except FileNotFoundError:
                continue
    except FileNotFoundError:
        pass
    return False


def below(proc, child, ancestor):
    seen = {child.pid}
    parent = child.parent
    while parent and parent not in seen:
        if parent == ancestor.pid:
            return True
        if parent == 1:
            break
        seen.add(parent)
        parent, _ = process_ids(proc / str(parent))
    return False


def slots(roles):
    result = {role: role for role in ("owner", "window") if role in roles}
    for index, role in enumerate(sorted(set(roles) - result.keys()), 1):
        result[role] = f"extension-{index}"
    return result


def digest(value):
    return hashlib.sha256(os.fsencode(value)).hexdigest()


def file_digest(path, expected):
    # Refuse links and special files even if the path is replaced between discovery and open.
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or executable_identity(info) != expected:
            raise Inconclusive("an installed artifact changed while identifying the workload")
        fingerprint = hashlib.file_digest(stream, "sha256").hexdigest()
        if executable_identity(os.fstat(stream.fileno())) != expected:
            raise Inconclusive("an installed artifact changed while identifying the workload")
        return fingerprint


def fixed_mmap_threshold(entry, label):
    # Only retain a bounded prefix of any NUL-terminated environment item. No values are
    # reported or saved, and a substring in another variable cannot pass for a setting.
    prefix = b""
    found = False
    with (entry / "environ").open("rb") as stream:
        for block in iter(lambda: stream.read(4096), b""):
            fields = block.split(b"\0")
            for index, field in enumerate(fields):
                prefix += field[:len(FIXED_MMAP) + 1 - len(prefix)]
                if index == len(fields) - 1:
                    break
                # An empty value is still explicit: glibc can interpret it as zero.
                if prefix.startswith(MMAP_KEY):
                    if found or prefix != FIXED_MMAP:
                        raise Inconclusive(f"condition=allocator role={label} cannot compare a custom mmap threshold")
                    found = True
                prefix = b""
    return found


class Scope:
    def __init__(self, proc, bin_dir, data_dir):
        self.proc = proc
        self.data = data_dir.resolve()
        self.programs = {}
        for program in PROGRAMS:
            try:
                path = (bin_dir / program).resolve(strict=True)
            except FileNotFoundError:
                continue
            self.programs[program] = path, executable_identity(path.stat())
        self.by_path = {path: program for program, (path, _) in self.programs.items()}
        if not {"peersus", "peersus-ui"} <= self.programs.keys() or len(self.by_path) != len(self.programs):
            raise Inconclusive("the installed owner and window executables could not be distinguished")

    def discover(self):
        candidates = defaultdict(list)
        for entry in self.proc.iterdir():
            if not (entry.name.isascii() and entry.name.isdecimal()):
                continue
            try:
                target = entry.joinpath("exe").readlink()
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            program = self.by_path.get(Path(str(target).removesuffix(" (deleted)")))
            if program is None:
                continue
            try:
                parent, started = process_ids(entry)
            except (FileNotFoundError, ProcessLookupError):
                continue
            candidates[program].append(Process(int(entry.name), parent, started, program))

        groups = defaultdict(list)
        for process in candidates["peersus"]:
            if profile_lock(self.proc / str(process.pid), self.data / "instance.lock"):
                groups["owner"].append(process)
        owner = self.unique(groups["owner"], "owner")
        # An out-of-tree window using the exact same executable cannot be safely assigned to a
        # profile without reading its environment or querying the app. Do not choose between them.
        window = self.unique(candidates["peersus-ui"], "window")
        if not (below(self.proc, window, owner) or below(self.proc, owner, window)):
            raise Inconclusive("window is not in the verified owner's process tree")
        groups["window"].append(window)
        for program in PROGRAMS[2:]:
            for worker in candidates[program]:
                if not below(self.proc, worker, owner):
                    continue
                args = (self.proc / str(worker.pid) / "cmdline").read_bytes().split(b"\0")
                module = Path(os.fsdecode(args[1])) if len(args) > 1 else Path()
                if (not module.is_absolute() or module.parent.parent != self.data / "plugins"
                        or module.resolve(strict=True) != module):
                    raise Inconclusive("an installed helper in the owner's tree has an unverified module location")
                info = module.lstat()
                if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_MODULE_BYTES:
                    raise Inconclusive("an installed helper has an invalid module file")
                groups["extension:" + digest(module.parent.name)].append(
                    replace(worker, module=(module, executable_identity(info)))
                )

        result = {role: self.unique(groups[role], label) for role, label in slots(groups).items()}
        for role, label in slots(result).items():
            self.verify(result[role], label)
        return result

    @staticmethod
    def unique(candidates, label):
        if len(candidates) > 1:
            raise Inconclusive(f"ambiguous role={label} candidates={len(candidates)}")
        if not candidates:
            raise Inconclusive(f"no verified role={label} in the selected installation/profile")
        return candidates[0]

    def verify(self, process, label):
        entry = self.proc / str(process.pid)
        path, expected = self.programs[process.program]
        if ((entry / "exe").readlink() != path or executable_identity((entry / "exe").stat()) != expected
                or process_ids(entry) != (process.parent, process.started)):
            raise Inconclusive(f"process or installed executable changed for role={label}")

    def builds(self, processes):
        components = {}
        for program in sorted({process.program for process in processes.values()}):
            path, expected = self.programs[program]
            components[program] = file_digest(path, expected)
        return components


def read_memory(entry):
    result = {}
    for line in (entry / "smaps_rollup").read_text().splitlines():
        key, _, rest = line.partition(":")
        if key in MEMORY:
            number, unit = rest.split()
            if unit != "kB" or int(number) < 0:
                raise Inconclusive("invalid smaps_rollup memory measure")
            result[MEMORY[key]] = int(number)
    if result.keys() != set(MEMORY.values()):
        raise Inconclusive("smaps_rollup lacks a required RSS, PSS, swap or huge-page field")
    return result


def compared(memory):
    return memory["anon_kib"] + memory["swap_pss_kib"]


def limit_kib(baseline):
    return baseline + max(ABSOLUTE_SLACK_KIB, baseline * RELATIVE_SLACK_PERCENT // 100)


def capture(scope, conditions, pause):
    processes = scope.discover()
    binaries = scope.builds(processes)
    build = digest(json.dumps(binaries, sort_keys=True))
    ticks = os.sysconf("SC_CLK_TCK")
    uptime = float((scope.proc / "uptime").read_text().split()[0])
    ages = {role: int(uptime - process.started / ticks) for role, process in processes.items()}
    samples = {role: [] for role in processes}
    labels = slots(processes)
    workload = {}
    for role, process in processes.items():
        allocator = fixed_mmap_threshold(scope.proc / str(process.pid), labels[role])
        workload[role] = {"fixed_mmap": allocator}
        if process.module is not None:
            workload[role].update(helper_sha256=binaries[process.program], module_sha256=file_digest(*process.module))
    for index in range(SAMPLES):
        if index:
            pause(INTERVAL_S)
            if scope.discover() != processes:
                raise Inconclusive("processes or roles changed during sampling")
        for role, process in processes.items():
            samples[role].append(read_memory(scope.proc / str(process.pid)))
            scope.verify(process, labels[role])
    if scope.discover() != processes:
        raise Inconclusive("processes or roles changed during sampling")
    roles = {}
    for role, process in processes.items():
        ordered = sorted(samples[role], key=compared)
        midpoint = compared(ordered[SAMPLES // 2])
        tied = [sample for sample in ordered if compared(sample) == midpoint]
        # Use one deterministic tied sample for diagnostics, but keep the maximum THP
        # reading as well: both endpoints can affect whether a comparison is conclusive.
        chosen = min(tied, key=lambda sample: (sample["anon_huge_kib"], *[sample[key] for key in MEMORY.values()]))
        roles[role] = {**chosen, **workload[role], "age_s": ages[role], "program": process.program,
                       "anon_huge_max_kib": max(sample["anon_huge_kib"] for sample in tied)}
    return {"version": VERSION, "build": build, "profile": digest(scope.data), "conditions": conditions, "roles": roles}


def nonnegative(value):
    return type(value) is int and value >= 0


def valid_size(value):
    bounds = WINDOW_SIZE.fullmatch(value) if isinstance(value, str) else None
    return bounds is not None and all(int(axis) <= 32768 for axis in bounds.groups())


def valid_conditions(value):
    return (isinstance(value, dict) and set(value) == set(CONDITIONS)
            and value["page"] in PAGES and value["canvas"] in CANVAS
            and value["renderer"] in RENDERERS and valid_size(value["window_size"])
            and all(nonnegative(value[key]) for key in ("extension_pages", "paired", "online"))
            and value["online"] <= value["paired"]
            and (value["extension_pages"] > 0 or (value["page"] != "extension" and value["canvas"] != "open")))


def valid_digest(value):
    return isinstance(value, str) and DIGEST.fullmatch(value) is not None


def load_baseline(path):
    data = json.loads(path.read_text())
    if (isinstance(data, dict) and data and "version" not in data
            and all(isinstance(role, dict) and {"rss", "pss", "private_dirty"} <= role.keys()
                    for role in data.values())):
        raise LegacyBaseline
    if not isinstance(data, dict) or data.get("version") != VERSION or not valid_conditions(data.get("conditions")):
        raise Inconclusive("unsupported or invalid baseline format; use --record for a new reference")
    if not all(valid_digest(data.get(key)) for key in ("build", "profile")):
        raise Inconclusive("baseline has an invalid build or profile fingerprint")
    roles = data.get("roles")
    if not isinstance(roles, dict) or not {"owner", "window"} <= roles.keys():
        raise Inconclusive("baseline is missing required roles")
    for key, entry in roles.items():
        extension = key not in ("owner", "window")
        kinds = PROGRAMS[2:] if extension else ("peersus",) if key == "owner" else ("peersus-ui",)
        if ((extension and not (key.startswith("extension:") and valid_digest(key[10:])))
                or not isinstance(entry, dict) or entry.get("program") not in kinds
                or not all(nonnegative(entry.get(field)) for field in (*MEMORY.values(), "age_s", "anon_huge_max_kib"))
                or entry["anon_huge_kib"] > entry["anon_huge_max_kib"]
                or type(entry.get("fixed_mmap")) is not bool
                or (extension and not all(valid_digest(entry.get(field)) for field in ("helper_sha256", "module_sha256")))):
            raise Inconclusive("baseline has invalid role data")
    return data


def compare_conditions(current, baseline):
    differences = []
    conditions = current["conditions"]
    roles = current["roles"]
    labels = slots(roles)
    if baseline is not None:
        for key, value in conditions.items():
            before = baseline["conditions"][key]
            if value != before:
                differences.append(f"condition={key} baseline={before} current={value}")
        if current["profile"] != baseline["profile"]:
            differences.append("condition=profile selected data directory differs")
        old = baseline["roles"]
        if {k: v["program"] for k, v in roles.items()} != {k: v["program"] for k, v in old.items()}:
            differences.append("condition=roles role set or helper kind differs")
        for role, label in labels.items():
            if role not in old:
                continue
            for key, condition in (("fixed_mmap", "allocator"), ("helper_sha256", "helper_binary"),
                                   ("module_sha256", "module")):
                if old[role].get(key) != roles[role].get(key):
                    differences.append(f"condition={condition} role={label} differs from reference")
            before, now = old[role]["age_s"], roles[role]["age_s"]
            if abs(now - before) > AGE_SLACK_S or before < MIN_AGE_S:
                differences.append(f"condition=process_age role={label} baseline_s={before} current_s={now}")
    if conditions["extension_pages"]:
        differences.append("condition=extension_pages close contributed pages before comparison")
    for role, label in labels.items():
        if roles[role]["age_s"] < MIN_AGE_S:
            differences.append(f"condition=process_age role={label} below minimum {MIN_AGE_S} s")
    return differences


def decision(current, baseline):
    before, now = compared(baseline), compared(current)
    grew = now > limit_kib(before)
    # THP bytes are RSS, not proportional private bytes. Never subtract them to grant a pass.
    # Bound the possible padding on the side with more THP; if it could change the result, refuse.
    # The alternative is monotone in delta; the extreme pairs cover every median tie
    # on either side without choosing based on the order in which samples arrived.
    for delta in (current["anon_huge_kib"] - baseline["anon_huge_max_kib"],
                  current["anon_huge_max_kib"] - baseline["anon_huge_kib"]):
        alternative = max(0, now - max(0, delta)) > limit_kib(max(0, before + min(0, delta)))
        if grew != alternative:
            return None
    return grew


def describe(current, baseline, comparisons=None):
    print(f"build={current['build']}")
    if baseline is not None:
        print(f"baseline_build={baseline['build']}")
    print(" ".join(f"{key}={value}" for key, value in current["conditions"].items()))
    print(f"samples={SAMPLES} interval_s={INTERVAL_S}")
    for role, label in slots(current["roles"]).items():
        entry = current["roles"][role]
        line = (f"role={label} program={entry['program']} age_s={entry['age_s']} rss_kib={entry['rss_kib']} "
                f"pss_kib={entry['pss_kib']} anon_swap_kib={compared(entry)} anon_huge_kib={entry['anon_huge_kib']}")
        if entry["anon_huge_max_kib"] != entry["anon_huge_kib"]:
            line += f" anon_huge_max_kib={entry['anon_huge_max_kib']}"
        if comparisons is not None:
            old = compared(baseline["roles"][role])
            line += f" baseline_anon_swap_kib={old} limit_kib={limit_kib(old)} verdict={'GREW' if comparisons[role] else 'OK'}"
        print(line)


def record(path, current):
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as output:
            temporary = Path(output.name)
            json.dump(current, output, indent=2, sort_keys=True)
            output.write("\n")
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main(argv=None, *, proc=Path("/proc"), pause=time.sleep):
    parser = Parser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--record", metavar="BASELINE", type=Path)
    action.add_argument("--check", metavar="BASELINE", type=Path)
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/lib/peersus",
                        help="directory containing the actual installed programs (default: user install)")
    parser.add_argument("--data-dir", type=Path,
                        default=Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share") / "peersus",
                        help="directory of the application's instance lock (default: current XDG profile)")
    parser.add_argument("--page", choices=PAGES, help="visible window page")
    parser.add_argument("--extension-pages", type=int, help="number of mounted contributed pages")
    parser.add_argument("--canvas", choices=CANVAS, help="canvas workload phase")
    parser.add_argument("--paired", type=int, help="number of paired devices")
    parser.add_argument("--online", type=int, help="number of online paired devices")
    parser.add_argument("--renderer", choices=RENDERERS, help="active renderer, not preference")
    parser.add_argument("--window-size", help="visible window content size in pixels (WIDTHxHEIGHT)")
    args = parser.parse_args(argv)
    try:
        baseline = load_baseline(args.check) if args.check else None
        conditions = {key: getattr(args, key) for key in CONDITIONS}
        if not valid_conditions(conditions):
            raise Inconclusive("supply consistent --page, --extension-pages, --canvas, --paired, --online, "
                               "--renderer and --window-size conditions")
        current = capture(Scope(proc, args.bin_dir, args.data_dir), conditions, pause)
        differences = compare_conditions(current, baseline)
        comparisons = None
        if baseline is not None and not differences:
            comparisons = {role: decision(entry, baseline["roles"][role]) for role, entry in current["roles"].items()}
            differences.extend(f"condition=THP residency uncertain for role={label}"
                               for role, label in slots(comparisons).items() if comparisons[role] is None)
        if differences:
            describe(current, baseline)
            raise Inconclusive("\n".join(differences))
        if args.record:
            record(args.record, current)
            describe(current, None)
            print("RESULT=RECORDED")
            return 0
        describe(current, baseline, comparisons)
        grew = any(comparisons.values())
        print(f"RESULT={'GREW' if grew else 'PASS'}")
        return int(grew)
    except LegacyBaseline:
        print("Legacy RSS-only baseline: record it again with --record and matched conditions.\nRESULT=LEGACY_BASELINE")
        return 3
    except Inconclusive as error:
        print(f"{error}\nRESULT=INCONCLUSIVE")
        return 2
    except (OSError, ValueError, IndexError, RuntimeError):
        print("Could not read stable process, installation or baseline data, or write the requested baseline.\nRESULT=INCONCLUSIVE")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
