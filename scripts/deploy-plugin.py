#!/usr/bin/env python3
"""Build a plugin and install it on this computer and on phones over adb, keeping its permissions.

    scripts/deploy-plugin.py control                 # build, package, and say what must be accepted
    scripts/deploy-plugin.py control --accept DIGEST --accept-outside-store-risk
                                                     # install on every reachable device
    scripts/deploy-plugin.py control --to msi rog ...   # only these devices (their Peersus device names)
    scripts/deploy-plugin.py clipboard --no-build ...   # install what was built last

A device is named by its Peersus device name, case-insensitively; `local` is this computer.
An extension is installed only after its licence and notice were accepted, and one from outside the
store only after the application's warning was: this script accepts nothing by itself. Read them with
`peersus plugin-install <unpacked package>`, which prints the digest to give here with `--accept`.
Installing a plugin makes the app forget what was granted to it; the grants it had (its own, and
the ones it holds on each paired device) are read before and given back after, unchanged.
Nothing private is printed: no device ids, addresses or paths outside this repository.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid
from pathlib import Path

# The plugin itself is its `.wasm`; a `.so` is an optional native extra next to it.
MODULES = (".wasm", ".so")
REPO = Path(__file__).resolve().parent.parent
SDK = REPO
PACKAGE = "ro.peersus"
LIMIT = 64 << 20


def frame(request: dict) -> bytes:
    body = json.dumps(request).encode()
    return struct.pack(">I", len(body)) + body


def unframe(data: bytes) -> dict:
    if len(data) < 4:
        raise RuntimeError("the app did not answer (is it running?)")
    size = struct.unpack(">I", data[:4])[0]
    if size > LIMIT or len(data) < 4 + size:
        raise RuntimeError("the app's answer was cut short")
    return json.loads(data[4 : 4 + size])


class Local:
    """This computer, through its owner's control socket."""

    platform = "linux"

    def __init__(self) -> None:
        data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
        self.socket = data / "peersus" / "control.sock"
        self.name = self.snapshot()["settings"]["device_name"]

    def request(self, request: dict) -> dict:
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(60)
            stream.connect(str(self.socket))
            stream.sendall(frame(request))
            head = b""
            while len(head) < 4:
                chunk = stream.recv(4 - len(head))
                if not chunk:
                    break
                head += chunk
            if len(head) < 4:
                raise RuntimeError("the app did not answer (is it running?)")
            size = struct.unpack(">I", head)[0]
            body = b""
            while len(body) < size:
                chunk = stream.recv(min(1 << 20, size - len(body)))
                if not chunk:
                    break
                body += chunk
        return unframe(head + body)

    def snapshot(self) -> dict:
        return self.request({"operation": "Snapshot"})["snapshot"]

    def stage(self, archive: Path) -> str:
        return str(archive)

    def unstage(self, staged: str) -> None:
        pass

    def installed_wasm(self, plugin: str) -> set[str]:
        data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
        root = data / "peersus" / "plugins" / plugin
        return {sha(p.read_bytes()) for suffix in MODULES for p in root.rglob(f"*{suffix}")} if root.exists() else set()


class Phone:
    """A phone over adb, through the app's own control socket (run-as, so only the app's files)."""

    platform = "android"

    def __init__(self, transport: str) -> None:
        self.transport = transport
        self.root = self.shell(f"run-as {PACKAGE} pwd").decode().strip()
        if not self.root:
            raise RuntimeError("the app is not installed or not debuggable")
        self.name = self.snapshot()["settings"]["device_name"]

    def shell(self, command: str, data: bytes | None = None, timeout: int = 60) -> bytes:
        done = subprocess.run(["adb", "-s", self.transport, "shell", "-T", command],
                              input=data, capture_output=True, timeout=timeout)
        return done.stdout

    def request(self, request: dict) -> dict:
        # `shell -T` and not `exec-out`: exec-out does not pass stdin on, and the request is never read.
        # `-q 10`: a phone busy installing may take seconds to answer after the request is sent.
        return unframe(self.shell(f"run-as {PACKAGE} toybox nc -U -q 10 -w5 -W60 files/control.sock",
                                  frame(request), timeout=90))

    def snapshot(self) -> dict:
        return self.request({"operation": "Snapshot"})["snapshot"]

    def stage(self, archive: Path) -> str:
        folder = f"cache/plugin-update-{uuid.uuid4().hex}"
        data = archive.read_bytes()
        self.shell(f"run-as {PACKAGE} mkdir -p {folder}")
        self.shell(f"run-as {PACKAGE} dd of={folder}/{archive.name}", data, timeout=120)
        copied = self.shell(f"run-as {PACKAGE} sha256sum {folder}/{archive.name}").decode().split()
        if not copied or copied[0] != sha(data):
            raise RuntimeError("the archive did not arrive whole on the phone")
        return f"{self.root}/{folder}/{archive.name}"

    def unstage(self, staged: str) -> None:
        folder = staged[len(self.root) + 1 :].rsplit("/", 1)[0]
        if folder.startswith("cache/plugin-update-"):
            self.shell(f"run-as {PACKAGE} rm -r {folder}")

    def installed_wasm(self, plugin: str) -> set[str]:
        out = self.shell(f"run-as {PACKAGE} sh -c 'find files/extensions/plugins/{plugin} \\( -name \"*.wasm\" -o -name \"*.so\" \\) -exec sha256sum {{}} \\;'")
        return {line.split()[0] for line in out.decode().splitlines() if line.strip()}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def ok(response: dict, what: str) -> None:
    result = response.get("result")
    if not (isinstance(result, dict) and "Ok" in result):
        raise RuntimeError(f"{what}: {result.get('Err') if isinstance(result, dict) else result}")


def plugin_source(name: str) -> tuple[Path, str]:
    """The plugin's wasm crate (holding plugin.json) and its id, which is its top folder's name
    (`control`, `clipboard`, ...): the id is given when packaging, not written in the manifest."""
    for manifest in sorted(REPO.glob("plugins/*/wasm/plugin.json")):
        plugin = manifest.parent.parent.name.removeprefix("peersus-")
        if name == plugin:
            return manifest.parent, plugin
    raise SystemExit(f"no plugin named {name!r} under plugins/*/wasm")


def build(source: Path) -> None:
    print(f"building {source.relative_to(REPO)}")
    subprocess.run(["cargo", "build", "--release", "--target", "wasm32-unknown-unknown", "--locked"],
                   cwd=source, check=True)


def package(source: Path, plugin: str, platform: str, out: Path) -> Path:
    done = subprocess.run([sys.executable, str(SDK / "scripts/package-native-extension.py"),
                           "--source", str(source), "--sdk", str(SDK), "--target-dir", str(source / "target"),
                           "--out", str(out), "--platform", platform, "--id", plugin],
                          capture_output=True, text=True)
    if done.returncode:
        raise SystemExit(f"packaging for {platform} failed:\n{done.stderr.strip()[-2000:]}")
    archives = sorted(out.glob(f"*{platform}*.tar.gz"))
    if len(archives) != 1:
        raise SystemExit(f"packaging for {platform} made {len(archives)} archives")
    return archives[0]


def archive_modules(archive: Path) -> set[str]:
    """The program modules an archive carries: its `.wasm` (the plugin, always there) and any
    `.so`, a secondary native binary some plugins ship as an extra."""
    with tarfile.open(archive) as tar:
        modules = {member.name: sha(tar.extractfile(member).read()) for member in tar
                   if member.isfile() and member.name.endswith(MODULES)}
    if not any(name.endswith(".wasm") for name in modules):
        raise SystemExit(f"{archive.name} holds no wasm module")
    return set(modules.values())


def grants_of(snapshot: dict, plugin: str) -> tuple[list, dict]:
    own = snapshot.get("grants", {}).get(plugin, [])
    peers = {p["id"]: p["grants"][plugin] for p in snapshot.get("peers", []) if p.get("grants", {}).get(plugin)}
    return own, peers


def install(device, archive: Path, plugin: str, agreement: str, risk: bool) -> None:
    before = device.snapshot()
    own, peers = grants_of(before, plugin)
    staged = device.stage(archive)
    try:
        ok(device.request({"operation": "Command", "command": {"command": "stage-install", "source": staged}}), "stage")
        ok(device.request({"operation": "Command", "command": {
            "command": "confirm-install", "plugin": plugin, "agreement": agreement, "risk_acknowledged": risk}}), "install")
    finally:
        device.unstage(staged)
    # Given back exactly as they were: installing made the app forget them.
    ok(device.request({"operation": "Command", "command": {"command": "set-grants", "plugin": plugin, "grants": own}}),
       "restoring its permissions")
    for peer, grants in peers.items():
        ok(device.request({"operation": "Command", "command": {"command": "set-grants", "plugin": plugin,
                                                               "peer": peer, "grants": grants}}),
           "restoring its permissions on a paired device")
    after = device.snapshot()
    if grants_of(after, plugin) != (own, peers):
        raise RuntimeError("its permissions did not come back as they were")
    wanted = archive_modules(archive)
    for _ in range(20):
        status = next((p.get("status") for p in device.snapshot().get("plugins", []) if p.get("id") == plugin), None)
        if status == "running":
            break
        time.sleep(0.5)
    if not wanted <= device.installed_wasm(plugin):
        raise RuntimeError("the installed modules are not the ones just built")
    print(f"  {device.name}: installed, {status}, {len(own)} own and {len(peers)} device permission set(s) kept")


def phones() -> list[str]:
    """One adb transport per physical phone. A transport's name may hold spaces (an mDNS route: `adb-… (2)._adb-tls-connect._tcp`),
    so it is everything before the tab. A phone reached twice (its address and an mDNS route) is installed on once, through
    the ordinary transport, as `dev.py` deploys."""
    out = subprocess.run(["adb", "devices"], capture_output=True, text=True).stdout.splitlines()[1:]
    transports = [line.rsplit("\t", 1)[0] for line in out if line.endswith("\tdevice")]
    by_phone: dict[str, str] = {}
    for transport in sorted(transports, key=lambda name: ("_adb-tls" in name, name)):
        serial = subprocess.run(["adb", "-s", transport, "shell", "getprop", "ro.serialno"],
                                capture_output=True, text=True, timeout=30).stdout.strip()
        by_phone.setdefault(serial or transport, transport)
    return list(by_phone.values())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("plugin", help="plugin folder (control, clipboard, ...) or id")
    parser.add_argument("--to", nargs="*", default=["all"], help="device names, `local`, or `all` (default)")
    parser.add_argument("--no-build", action="store_true", help="install the module built last")
    parser.add_argument("--accept", metavar="DIGEST",
                        help="the digest of the licence and notice you read and accept (printed by `peersus plugin-install`)")
    parser.add_argument("--accept-outside-store-risk", action="store_true",
                        help="you read the application's warning about a package from outside the store")
    args = parser.parse_args()
    wanted = {name.lower() for name in args.to}

    source, plugin = plugin_source(args.plugin)
    devices = []
    try:
        devices.append(Local())
    except Exception as error:
        print(f"this computer: skipped ({error})")
    for transport in phones():
        try:
            devices.append(Phone(transport))
        except Exception as error:
            print(f"a phone: skipped ({error})")

    def chosen(device) -> bool:
        return "all" in wanted or device.name.lower() in wanted or ("local" in wanted and isinstance(device, Local))

    targets = [d for d in devices if chosen(d)]
    missing = wanted - {"all", "local"} - {d.name.lower() for d in devices}
    if missing:
        print(f"not reachable: {', '.join(sorted(missing))}")
    if not targets:
        raise SystemExit("no device to install on")

    if not args.no_build:
        build(source)
    failed = 0
    with tempfile.TemporaryDirectory(prefix=f"{plugin}-deploy-") as scratch:
        archives = {}
        for platform in sorted({d.platform for d in targets}):
            out = Path(scratch) / platform
            out.mkdir()
            archives[platform] = package(source, plugin, platform, out)
        if not args.accept:
            raise SystemExit("built and packaged; nothing installed: the licence and notice were not accepted "
                             "(read them with `peersus plugin-install <unpacked package>` and pass --accept DIGEST)")
        for device in targets:
            try:
                install(device, archives[device.platform], plugin, args.accept, args.accept_outside_store_risk)
            except Exception as error:
                failed += 1
                print(f"  {device.name}: FAILED: {error}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
