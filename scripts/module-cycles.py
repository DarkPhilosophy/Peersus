#!/usr/bin/env python3
"""List the groups of modules of each crate that need each other.

Cargo refuses a cycle between crates; between the modules of one crate nothing does. A module here is
a file or a folder directly under a crate's `src/`. One module needs another when its code, outside
comments and test modules, names it as `crate::other`, in a `use crate::{other, ..}` group, or (from a
file directly under `src/`) as `super::other`. Deeper `super::super::` paths and names reached only
through a re-export are not followed, so an empty answer is "none found this way", not a proof.

Prints each group with who needs whom and how many times. Exit status 1 when any group is found.

With no argument the crates of the workspace are read. Each argument is either one crate (a folder with
`src/`) or a folder of crates, so the working copy of the extensions can be read the same way:
`module-cycles.py plugins/peersus-control/wasm plugins/peersus-clipboard`.
"""
import os
import re
import sys
from collections import defaultdict

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "crates")
TEST_MODULE = re.compile(
    r"#\[cfg\((?:all\(\s*)?test\b[^\]]*\]\s*(?:pub(?:\([a-z]+\))?\s+)?mod\s+\w+\s*\{"
)


def without_comments(source):
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.S)
    kept = []
    for line in source.split("\n"):
        in_text, at, cut = False, 0, None
        while at < len(line):
            if line[at] == "\\" and in_text:
                at += 2
                continue
            if line[at] == '"':
                in_text = not in_text
            elif not in_text and line.startswith("//", at):
                cut = at
                break
            at += 1
        kept.append(line if cut is None else line[:cut])
    return "\n".join(kept)


def closing(source, at):
    """The index just past the brace that closes the one opened right before `at`."""
    depth = 1
    while at < len(source) and depth:
        depth += {"{": 1, "}": -1}.get(source[at], 0)
        at += 1
    return at


def without_tests(source):
    kept, done = [], 0
    for found in TEST_MODULE.finditer(source):
        if found.start() < done:
            continue
        kept.append(source[done : found.start()])
        done = closing(source, found.end())
    kept.append(source[done:])
    return "".join(kept)


def first_names(group):
    names, depth, name = [], 0, ""
    for character in group:
        depth += {"{": 1, "}": -1}.get(character, 0)
        if character == "," and depth == 0:
            names.append(name)
            name = ""
        else:
            name += character
    names.append(name)
    return [found.group(1) for found in (re.match(r"\s*(\w+)", name) for name in names) if found]


def needs(crate_folder):
    source_root = os.path.join(crate_folder, "src")
    modules = set()
    for entry in os.listdir(source_root):
        if entry.endswith(".rs") and entry not in ("lib.rs", "main.rs"):
            modules.add(entry[:-3])
        elif os.path.isdir(os.path.join(source_root, entry)) and entry != "bin":
            modules.add(entry)
    edges = defaultdict(lambda: defaultdict(int))
    for folder, _, files in os.walk(source_root):
        for file in files:
            if not file.endswith(".rs"):
                continue
            parts = os.path.relpath(os.path.join(folder, file), source_root).split(os.sep)
            if parts[0] == "bin" or parts in (["lib.rs"], ["main.rs"]):
                continue
            if parts[-1] == "tests.rs" or "tests" in parts[:-1]:
                continue
            module = parts[0][:-3] if parts[0].endswith(".rs") else parts[0]
            with open(os.path.join(folder, file), encoding="utf-8") as opened:
                source = without_tests(without_comments(opened.read()))
            named = [found.group(1) for found in re.finditer(r"\bcrate::(\w+)", source)]
            for found in re.finditer(r"\bcrate::\{", source):
                named += first_names(source[found.end() : closing(source, found.end()) - 1])
            if len(parts) == 1:
                named += [found.group(1) for found in re.finditer(r"\bsuper::(\w+)", source)]
            for name in named:
                if name in modules and name != module:
                    edges[module][name] += 1
    return modules, edges


def groups(modules, edges):
    """Tarjan's strongly connected components, those of more than one module."""
    order, low, stack, on_stack, found, count = {}, {}, [], set(), [], [0]

    def visit(module):
        order[module] = low[module] = count[0]
        count[0] += 1
        stack.append(module)
        on_stack.add(module)
        for other in edges[module]:
            if other not in order:
                visit(other)
                low[module] = min(low[module], low[other])
            elif other in on_stack:
                low[module] = min(low[module], order[other])
        if low[module] == order[module]:
            group = []
            while True:
                other = stack.pop()
                on_stack.discard(other)
                group.append(other)
                if other == module:
                    break
            if len(group) > 1:
                found.append(sorted(group))

    for module in sorted(modules):
        if module not in order:
            visit(module)
    return found


def crate_folders(arguments):
    """Every crate named or held by the arguments, or the workspace's when there are none."""
    found = []
    for folder in arguments or [ROOT]:
        if os.path.isdir(os.path.join(folder, "src")):
            found.append(folder)
            continue
        for entry in sorted(os.listdir(folder)):
            if os.path.isdir(os.path.join(folder, entry, "src")):
                found.append(os.path.join(folder, entry))
    return found


def main():
    sys.setrecursionlimit(10_000)
    any_group = False
    for folder in crate_folders(sys.argv[1:]):
        modules, edges = needs(folder)
        found = groups(modules, edges)
        name = os.path.relpath(folder, os.path.join(ROOT, "..")) if sys.argv[1:] else os.path.basename(folder)
        print(f"{name}: {len(modules)} modules, {len(found)} groups")
        for group in found:
            any_group = True
            print(f"  group of {len(group)}: {', '.join(group)}")
            for module in group:
                inside = sorted((other, times) for other, times in edges[module].items() if other in group)
                if inside:
                    print(f"    {module} -> " + ", ".join(f"{other} ({times})" for other, times in inside))
    return 1 if any_group else 0


if __name__ == "__main__":
    sys.exit(main())
