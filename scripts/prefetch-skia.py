#!/usr/bin/env python3
"""Generate real Nix fixed-output hashes for the locked Skia external source graph.
Requires Nix with nix-command enabled. Hashes extracted archive trees; never executes DEPS.
"""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import subprocess
import io
import tarfile
import shlex
import tempfile
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--deps", type=Path, help="optional DEPS override; must match the pinned original Skia source")
parser.add_argument("--nix-command", default="nix", help="Nix command prefix, e.g. '/tmp/nix-portable nix'")
parser.add_argument("output", type=Path, nargs="?", default=Path("nix/skia-externals.json"))
args = parser.parse_args()
if args.deps:
    deps_bytes = args.deps.read_bytes()
else:
    request = urllib.request.Request("https://codeload.github.com/rust-skia/skia/tar.gz/61e7ca4e99062cdd0ab69445d5963fb3365778f6", headers={"User-Agent": "Peersus-packaging"})
    data = urllib.request.urlopen(request).read()
    if hashlib.sha256(data).hexdigest() != "8401d0de878582a03abc1029bb62330aea58cda89904ae6ab187302e6d48ba5d":
        raise SystemExit("Skia source archive checksum mismatch")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        deps_bytes = archive.extractfile("skia-61e7ca4e99062cdd0ab69445d5963fb3365778f6/DEPS").read()
if hashlib.sha256(deps_bytes).hexdigest() != "765be0ad1aff9088cdaad44d06ea9253293aec89a9647a5f5f4e13cdd6750e02":
    raise SystemExit("DEPS differs from the pinned original Skia source")
tree = ast.parse(deps_bytes.decode())
assignments = [node for node in tree.body if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "deps" for t in node.targets)]
if len(assignments) != 1 or not isinstance(assignments[0].value, ast.Dict):
    raise SystemExit("unexpected Skia DEPS format")
result = {}
for key, value in zip(assignments[0].value.keys, assignments[0].value.values):
    name = ast.literal_eval(key)
    if not name.startswith("third_party/externals/"):
        continue
    source = ast.literal_eval(value)
    url, revision = source.rsplit("@", 1)
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise SystemExit("Skia external is not pinned to a Git commit")
    archive_url = url.removesuffix(".git") + "/+archive/" + revision + ".tar.gz"
    request = urllib.request.Request(archive_url, headers={"User-Agent": "Peersus-packaging"})
    archive_bytes = urllib.request.urlopen(request, timeout=120).read()
    with tempfile.TemporaryDirectory(prefix="peersus-skia-pin-") as temporary:
        directory = Path(temporary)
        with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz") as archive:
            archive.extractall(directory, filter="data")
        nar_hash = subprocess.check_output(
            [*shlex.split(args.nix_command), "hash", "path", str(directory)], text=True
        ).strip()
    result[name] = {
        "url": url, "rev": revision, "archive": archive_url,
        "sha256": hashlib.sha256(archive_bytes).hexdigest(), "hash": nar_hash,
    }
if not result:
    raise SystemExit("no pinned Skia externals discovered")
payload = {"depsSha256": hashlib.sha256(deps_bytes).hexdigest(), "externals": result}
args.output.parent.mkdir(parents=True, exist_ok=True)
# Exclusive creation prevents silently replacing reviewed dependency hashes.
with args.output.open("x") as stream:
    json.dump(payload, stream, indent=2)
    stream.write("\n")
