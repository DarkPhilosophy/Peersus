#!/usr/bin/env python3
"""Package and deploy extensions on local and SSH desktops and debuggable Android devices.

    scripts/deploy-plugin.py control                 # build, package, and say what must be accepted
    scripts/deploy-plugin.py control --accept DIGEST --accept-outside-store-risk
                                                     # install on every reachable device
    scripts/deploy-plugin.py control --to msi rog ...   # only these devices (their Peersus device names)
    scripts/deploy-plugin.py clipboard --no-build ...   # install what was built last

A device is named by its Peersus device name, case-insensitively; `local` is this computer.
Matching packages are left alone. An update reuses the exact agreement already accepted on its
device. For new or changed text, run `peersus plugin-install <unpacked package>` to review it and
pass `--accept DIGEST` or explicitly use `--accept-current-agreement` for this run. A package from
outside the store also needs the application's warning accepted. Compatible own and per-device
grants are restored and verified after an update.
Nothing private is printed: no device ids, addresses or paths outside this repository.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
import shlex
import shutil
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

    def __init__(self, ready_timeout=60, *, connect=True) -> None:
        data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
        self.socket = data / "peersus" / "control.sock"
        if connect:
            self.name = self.request({"operation": "Snapshot"}, timeout=ready_timeout)["snapshot"]["settings"]["device_name"]

    def request(self, request: dict, timeout=60) -> dict:
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(timeout)
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

    def installed_modules(self, plugin: str) -> dict[str, str]:
        root = self.socket.parent / "plugins" / plugin
        return {p.relative_to(root).as_posix(): sha(p.read_bytes())
                for suffix in MODULES for p in root.rglob(f"*{suffix}") if p.is_file()}

    def manifest_digest(self, plugin: str) -> str:
        path = self.socket.parent / "plugins" / plugin / "plugin.json"
        return sha(path.read_bytes()) if path.is_file() else ""

    def installed_names(self) -> set[str]:
        store = self.socket.parent / "plugins"
        try:
            return {path.name for path in store.iterdir() if path.is_dir() and installed_name(path.name)}
        except FileNotFoundError:
            return set()


# Run on the remote desktop: relay one framed request from stdin to its owner's control socket and the framed answer back.
RELAY = r"""
import os, socket, struct, sys
data = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
request = sys.stdin.buffer.read()
with socket.socket(socket.AF_UNIX) as s:
    s.settimeout(60)
    s.connect(os.path.join(data, "peersus", "control.sock"))
    s.sendall(request)
    head = b""
    while len(head) < 4:
        chunk = s.recv(4 - len(head))
        if not chunk:
            break
        head += chunk
    body = b""
    if len(head) == 4:
        size = struct.unpack(">I", head)[0]
        while len(body) < size:
            chunk = s.recv(min(1 << 20, size - len(body)))
            if not chunk:
                break
            body += chunk
sys.stdout.buffer.write(head + body)
"""


class Remote:
    """A desktop over ssh (as `./dev.sh deploy hp` reaches it), through its owner's control socket."""

    platform = "linux"

    def __init__(self, host: str, ready_timeout=90, *, connect=True) -> None:
        self.host = host
        if connect:
            self.name = self.request({"operation": "Snapshot"}, timeout=ready_timeout)["snapshot"]["settings"]["device_name"]

    def ssh(self, command: list[str], data: bytes | None = None, timeout: int = 90) -> bytes:
        done = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", self.host, shlex.join(command)],
                              input=data, capture_output=True, timeout=timeout)
        if done.returncode != 0:
            raise RuntimeError("remote extension operation failed")
        return done.stdout

    def request(self, request: dict, timeout=90) -> dict:
        # The relay travels as base64, the request as bytes on stdin.
        code = base64.b64encode(RELAY.encode()).decode()
        return unframe(self.ssh(["python3", "-c", f"exec(__import__('base64').b64decode('{code}'))"],
                                frame(request), timeout=timeout))

    def snapshot(self) -> dict:
        return self.request({"operation": "Snapshot"})["snapshot"]

    def stage(self, archive: Path) -> str:
        folder = f".cache/peersus-plugin-update-{uuid.uuid4().hex}"
        self.ssh(["mkdir", "-p", folder])
        if subprocess.run(["scp", "-q", "-o", "BatchMode=yes", str(archive), f"{self.host}:{folder}/{archive.name}"],
                          timeout=120).returncode != 0:
            raise RuntimeError("the archive could not be copied")
        home = self.ssh(["sh", "-c", "printf %s \"$HOME\""]).decode()
        remote = f"{home}/{folder}/{archive.name}"
        copied = self.ssh(["sha256sum", remote]).decode().split()
        if not copied or copied[0] != sha(archive.read_bytes()):
            raise RuntimeError("the archive did not arrive whole")
        return remote

    def unstage(self, staged: str) -> None:
        folder = Path(staged).parent
        if folder.parent.name == ".cache" and folder.name.startswith("peersus-plugin-update-"):
            self.ssh(["rm", "--", staged])
            self.ssh(["rmdir", "--", str(folder)])

    def installed_modules(self, plugin: str) -> dict[str, str]:
        out = self.ssh(["sh", "-c", f'folder="${{XDG_DATA_HOME:-$HOME/.local/share}}/peersus/plugins/{plugin}"; '
                                    'if [ -d "$folder" ]; then cd "$folder" && ' + MODULE_HASHES + '; fi'])
        return read_module_hashes(out)

    def manifest_digest(self, plugin: str) -> str:
        out = self.ssh(["sh", "-c", f'path="${{XDG_DATA_HOME:-$HOME/.local/share}}/peersus/plugins/{plugin}/plugin.json"; '
                                    'if [ -f "$path" ]; then sha256sum "$path"; fi']).split()
        return out[0].decode() if out else ""

    def installed_names(self) -> set[str]:
        return parse_installed_names(self.ssh(["sh", "-c", INSTALLED_NAMES, "list-extensions", "desktop"]))


class Phone:
    """A phone over adb, through the app's own control socket (run-as, so only the app's files)."""

    platform = "android"

    def __init__(self, transport: str, ready_timeout=90, *, connect=True) -> None:
        self.transport = transport
        if connect:
            self.root = self.shell(f"run-as {PACKAGE} pwd", timeout=ready_timeout).decode().strip()
            if not self.root:
                raise RuntimeError("the app is not installed or not debuggable")
            self.name = self.request({"operation": "Snapshot"}, timeout=ready_timeout)["snapshot"]["settings"]["device_name"]

    def shell(self, command: str, data: bytes | None = None, timeout: int = 60, *, check=True) -> bytes:
        done = subprocess.run(["adb", "-s", self.transport, "shell", "-T", command],
                              input=data, capture_output=True, timeout=timeout)
        if check and done.returncode:
            raise RuntimeError("Android extension operation failed")
        return done.stdout

    def request(self, request: dict, timeout=90) -> dict:
        # `shell -T` and not `exec-out`: exec-out does not pass stdin on, and the request is never read.
        return unframe(self.shell(f"run-as {PACKAGE} toybox nc -U -q 10 -w5 -W60 files/control.sock",
                                  frame(request), timeout=timeout, check=False))

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
        path = staged[len(self.root) + 1 :]
        folder = path.rsplit("/", 1)[0]
        if folder.startswith("cache/plugin-update-"):
            self.shell(f"run-as {PACKAGE} rm -- {shlex.quote(path)}")
            self.shell(f"run-as {PACKAGE} rmdir -- {shlex.quote(folder)}")

    def installed_modules(self, plugin: str) -> dict[str, str]:
        folder = f"files/extensions/plugins/{plugin}"
        out = self.shell(f"run-as {PACKAGE} sh -c " +
                         shlex.quote(f'if [ -d "{folder}" ]; then cd "{folder}" && {MODULE_HASHES}; fi'))
        return read_module_hashes(out)

    def manifest_digest(self, plugin: str) -> str:
        path = f"files/extensions/plugins/{plugin}/plugin.json"
        out = self.shell(f"run-as {PACKAGE} sh -c " +
                         shlex.quote(f'if [ -f "{path}" ]; then sha256sum "{path}"; fi')).split()
        return out[0].decode() if out else ""

    def installed_names(self) -> set[str]:
        return parse_installed_names(self.shell(f"run-as {PACKAGE} sh -c " + shlex.quote(INSTALLED_NAMES)))


MODULE_HASHES = "find . -type f \\( -name '*.wasm' -o -name '*.so' \\) -exec sha256sum {} \\;"
INSTALLED_NAMES = r"""
store=files/extensions/plugins
if [ "${1:-}" = desktop ]; then store=${XDG_DATA_HOME:-$HOME/.local/share}/peersus/plugins; fi
if [ -d "$store" ]; then
    test -r "$store" && test -x "$store" || exit 1
    for path in "$store"/*; do
        if [ -d "$path" ]; then printf '%s\n' "${path##*/}"; fi
    done
fi
"""


def installed_name(name):
    return not name.startswith(".") and re.fullmatch(r"[a-zA-Z0-9._-]{1,128}", name) is not None


def parse_installed_names(data):
    return {name for name in data.decode().splitlines() if installed_name(name)}


def read_module_hashes(data: bytes) -> dict[str, str]:
    hashes = {}
    for line in data.decode().splitlines():
        digest, path = line.split(maxsplit=1)
        if not path.startswith("./") or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise RuntimeError("installed module checksum could not be read")
        hashes[path[2:]] = digest
    return hashes


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
    raise RuntimeError("no matching local source under plugins/*/wasm")


def build(source: Path, launcher=()) -> None:
    manifest = json.loads((source / "plugin.json").read_text())
    if manifest["execution"]["kind"] != "wasm":
        raise RuntimeError("deployment requires a WASM source")
    print(f"building {source.relative_to(REPO)}")
    subprocess.run([*launcher, "cargo", "build", "--release", "--target", "wasm32-unknown-unknown", "--locked",
                    "--target-dir", str(source / "target")], cwd=source, check=True)
    program = source / "target/wasm32-unknown-unknown/release" / manifest["program"]
    verify_cargo_output(program, source)


def verify_cargo_output(output: Path, source_root: Path):
    """Cargo's dep-info names the source inputs of this exact output, including path dependencies."""
    try:
        newest = output.stat().st_mtime_ns
        rule = re.sub(r"\\\n", " ", output.with_suffix(".d").read_text()).splitlines()[0]
        _, separator, dependencies = rule.partition(": ")
        if not separator or not dependencies:
            raise ValueError("no dependencies")
        for item in re.findall(r"(?:\\.|[^\s\\])+", dependencies):
            source = source_root / re.sub(r"\\(.)", r"\1", item).replace("$$", "$")
            if source.stat().st_mtime_ns > newest:
                raise RuntimeError(f"{output.name} is older than a build input")
    except (OSError, ValueError, IndexError) as error:
        raise RuntimeError(f"could not verify the Cargo inputs of {output.name}") from error


def package(source: Path, plugin: str, platform: str, out: Path) -> Path:
    done = subprocess.run([sys.executable, str(SDK / "scripts/package-native-extension.py"),
                           "--source", str(source), "--sdk", str(SDK), "--target-dir", str(source / "target"),
                           "--out", str(out), "--platform", platform, "--id", plugin],
                          capture_output=True, text=True)
    if done.returncode:
        raise RuntimeError(f"packaging for {platform} failed (build the WASM and check the package inputs before deploying)")
    archives = sorted(out.glob(f"*{platform}*.tar.gz"))
    if len(archives) != 1:
        raise RuntimeError(f"packaging for {platform} made {len(archives)} archives")
    return archives[0]


def archive_modules(archive: Path) -> dict[str, str]:
    """The checksums of the WASM program and any secondary native library, by relative filename."""
    with tarfile.open(archive) as tar:
        modules = {member.name.split("/", 1)[1]: sha(tar.extractfile(member).read()) for member in tar
                   if member.isfile() and "/" in member.name and member.name.endswith(MODULES)}
    if not any(name.endswith(".wasm") for name in modules):
        raise RuntimeError("the package holds no WASM module")
    return modules


def grants_of(snapshot: dict, plugin: str) -> tuple[list, dict]:
    own = snapshot.get("grants", {}).get(plugin, [])
    peers = {p["id"]: p["grants"][plugin] for p in snapshot.get("peers", []) if p.get("grants", {}).get(plugin)}
    return own, peers


def retained_grants(before, after, plugin):
    """Intersect old grants with the installed contract. Never translate a renamed capability."""
    old, peers = grants_of(before, plugin)
    info = next(p for p in after["plugins"] if p["id"] == plugin)
    allowed = info["grantable"]
    previous = next((p for p in before["plugins"] if p["id"] == plugin), {})
    def compatible(g, scope):
        if g not in allowed[scope]:
            return False
        if g["owner"] != plugin:
            return True
        key = g["name"]
        descriptions = previous.get("manifest", {}).get("capabilities", {})
        return key in descriptions and descriptions[key] == info["manifest"]["capabilities"].get(key)
    paired = {p["id"] for p in after.get("peers", [])}
    return ([g for g in old if compatible(g, "outright")],
            {key: kept for key, saved in peers.items() if key in paired
             and (kept := [g for g in saved if compatible(g, "per_device")])})

def install(device, archive: Path, plugin: str, agreement: str | None, risk: bool, *, installed_only=False,
            accept_current_agreement=False, review_in_app=False, label=None) -> str:
    label = label or f"{device.name}: {plugin}"
    def eligible(info):
        return info is not None and (info.get("standing") != "needs-approval" or info.get("agreed"))

    before = device.snapshot()
    previous = next((p for p in before["plugins"] if p["id"] == plugin), None)
    if installed_only and not eligible(previous):
        raise RuntimeError("extension is no longer installed or was never approved; not installed")
    wanted = archive_modules(archive)
    with tarfile.open(archive) as tar:
        manifest = next(m for m in tar if m.isfile() and m.name == f"{plugin}/plugin.json")
        digest = sha(tar.extractfile(manifest).read())

    def matches():
        return device.manifest_digest(plugin) == digest and wanted == device.installed_modules(plugin)

    def command(name, **fields):
        return device.request({"operation": "Command", "command": {"command": name, **fields}})

    if previous is not None and matches():
        return "up to date (installed manifest and module bytes match; no grants changed)"
    if any(p["id"] == plugin for p in before["staged"]):
        raise RuntimeError("another installation is already awaiting review; left untouched")
    if not review_in_app and not (risk or before["settings"].get("outside_store_risk_accepted")):
        raise RuntimeError("outside-store warning needs acknowledgment; review it and use --accept-outside-store-risk")
    candidate = agreement_digest(archive) if not review_in_app else None
    if agreement is not None and agreement != candidate:
        raise RuntimeError(f"provided agreement does not match the package ({candidate})")

    staged = device.stage(archive)
    try:
        ok(command("stage-install", source=staged), "stage")
        if not review_in_app:
            # Compare the owner's canonical manifest values, not optional fields from the raw JSON.
            before = device.snapshot()
            previous = next((p for p in before["plugins"] if p["id"] == plugin), None)
            offered = next((p for p in before["staged"] if p["id"] == plugin), None)
            if offered is None:
                raise RuntimeError("the staged package is no longer awaiting confirmation")
            if installed_only and not eligible(previous):
                ok(command("cancel-install", plugin=plugin), "cancelling review")
                raise RuntimeError("extension is no longer installed or was never approved; not installed")
            unchanged = (previous is not None and previous.get("agreed") and
                         previous["manifest"].get("agreements") == offered["manifest"].get("agreements"))
            if not (unchanged or agreement is not None or accept_current_agreement):
                ok(command("cancel-install", plugin=plugin), "cancelling review")
                raise RuntimeError(f"licence/notice needs acceptance ({candidate}); review it and use --accept-current-agreement")
            ok(command("confirm-install", plugin=plugin, agreement=candidate, risk_acknowledged=risk), "install")
            if not unchanged:
                print(f"{label}: accepted licence/notice {candidate}")
        else:
            deadline = time.monotonic() + 600
            while time.monotonic() < deadline:
                state = device.snapshot()
                if not any(p["id"] == plugin for p in state["staged"]):
                    if not matches():
                        raise RuntimeError("Review ended without installing the requested package")
                    break
                # Honor changes made while the old package is still installed.
                before = state
                time.sleep(2)
            else:
                # Removing the source archive does not dismiss the owner's review.
                # If acceptance won the race, finish the normal restoration below.
                reply = command("cancel-install", plugin=plugin)
                if not matches():
                    if any(p["id"] == plugin for p in device.snapshot()["staged"]):
                        ok(reply, "cancelling expired review")
                        raise RuntimeError("Review is still open; do not confirm it after the installer exits")
                    raise RuntimeError("Review closed without installation; no permissions were written")
        if not matches():
            raise RuntimeError("installed manifest or module differs from the reviewed package; no grants were written")
        return restore_grants(device, plugin, before, command)
    finally:
        device.unstage(staged)


def restore_grants(device, plugin, before, command):
    """Restore only still-declared, unchanged grants and verify the resulting state."""
    after = device.snapshot()
    own, peers = retained_grants(before, after, plugin)
    if grants_of(after, plugin)[0] != own:
        ok(command("set-grants", plugin=plugin, grants=own), "restoring its grants")
    actual_peers = grants_of(after, plugin)[1]
    for peer, grants in peers.items():
        if actual_peers.get(peer) != grants:
            ok(command("set-grants", plugin=plugin, peer=peer, grants=grants), "restoring grants on a paired device")
    after = device.snapshot()
    if grants_of(after, plugin) != (own, peers):
        raise RuntimeError("grant restoration could not be verified")
    was_disabled = any(p["id"] == plugin and p.get("standing") == "disabled" for p in before["plugins"])
    for _ in range(21):
        info = next((p for p in after["plugins"] if p["id"] == plugin), {})
        expected_state = info.get("standing") == "disabled" if was_disabled else info.get("status") == "running"
        if expected_state:
            break
        time.sleep(.5)
        after = device.snapshot()
    else:
        raise RuntimeError("installed package did not reach its expected running/disabled state")
    previous_own, previous_peers = grants_of(before, plugin)
    grant_state = ("grants unchanged" if (own, peers) == (previous_own, previous_peers) else
                   "compatible grants kept; incompatible grants not restored")
    state = "disabled" if was_disabled else "running"
    return f"updated ({state}; {grant_state}: {len(own)} own, {len(peers)} device sets)"


def phones() -> list[str]:
    """One adb transport per physical phone. A transport's name may hold spaces (an mDNS route: `adb-… (2)._adb-tls-connect._tcp`),
    so it is everything before the tab. A phone reached twice (its address and an mDNS route) is installed on once, through
    the ordinary transport, as `dev.py` deploys."""
    out = subprocess.run(["adb", "devices"], capture_output=True, text=True).stdout.splitlines()[1:]
    transports = [line.rsplit("\t", 1)[0] for line in out if line.endswith("\tdevice")]
    by_phone: dict[str, str] = {}
    for transport in sorted(transports, key=lambda name: ("_adb-tls" in name, name)):
        try:
            result = subprocess.run(["adb", "-s", transport, "shell", "getprop", "ro.serialno"],
                                    capture_output=True, text=True, timeout=5)
        except subprocess.TimeoutExpired:
            continue
        if result.returncode == 0 and result.stdout.strip():
            by_phone.setdefault(result.stdout.strip(), transport)
    return list(by_phone.values())


def package_failure(alias, name, error):
    cause = str(error) if isinstance(error, RuntimeError) else "operation failed or timed out; inspect actual state before retrying"
    print(f"{alias.upper()}: {name}: FAILED: {cause}")


def prepare_packages(inventories, output, *, no_build=False, launcher=()):
    """Build once per extension and package once per platform, without writing to devices."""
    by_name = {}
    prepared = [{} for _ in inventories]
    failed = set()
    for index, (_, platform, names) in enumerate(inventories):
        for name in names:
            by_name.setdefault(name, {}).setdefault(platform, []).append(index)
    for name, platforms in sorted(by_name.items()):
        try:
            source, _ = plugin_source(name)
            if not no_build:
                build(source, launcher)
        except (OSError, RuntimeError, subprocess.SubprocessError, KeyError, ValueError) as error:
            for indexes in platforms.values():
                for index in indexes:
                    package_failure(inventories[index][0], name, error)
                    failed.add(index)
            continue
        for platform, indexes in sorted(platforms.items()):
            try:
                archive = package(source, name, platform, output / name / platform)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                for index in indexes:
                    package_failure(inventories[index][0], name, error)
                    failed.add(index)
                continue
            for index in indexes:
                prepared[index][name] = archive
    return prepared, failed


def deploy_prepared(alias, device, archives, *, installed_only, agreement=None, review_in_app=False,
                    accept_current_agreement=False, accept_outside_store_risk=False, check_inventory=True):
    failed = 0
    if installed_only and check_inventory:
        try:
            current = {row["id"] for row in device.snapshot()["plugins"]}
        except (OSError, RuntimeError, subprocess.SubprocessError, KeyError, ValueError):
            print(f"{alias.upper()}: FAILED to read installed extensions")
            return 1
        for name in sorted(current - archives.keys()):
            package_failure(alias, name, RuntimeError("no build for this installed extension; rerun deployment"))
            failed += 1
        if not current and not archives:
            print(f"{alias.upper()}: no extensions installed")
    for name, archive in sorted(archives.items()):
        label = f"{alias.upper()}: {name}"
        try:
            result = install(device, archive, name, agreement, accept_outside_store_risk,
                             installed_only=installed_only, accept_current_agreement=accept_current_agreement,
                             review_in_app=review_in_app, label=label)
            print(f"{label}: {result}")
        except (OSError, RuntimeError, subprocess.SubprocessError, tarfile.TarError,
                KeyError, ValueError, StopIteration) as error:
            package_failure(alias, name, error)
            failed += 1
    return failed


def deploy_packages(targets, *, plugin="all", no_build=False, **options) -> int:
    installed_only = plugin == "all"
    inventories, owners = [], []
    failed = 0
    for alias, device in targets:
        try:
            names = {p["id"] for p in device.snapshot()["plugins"]} if installed_only else {plugin}
        except (OSError, RuntimeError, subprocess.SubprocessError, KeyError, ValueError):
            print(f"{alias.upper()}: FAILED to read installed extensions")
            failed += 1
            continue
        if not names:
            print(f"{alias.upper()}: no extensions installed")
            continue
        inventories.append((alias, device.platform, names))
        owners.append((alias, device))
    with tempfile.TemporaryDirectory(prefix="extension-deploy-") as scratch:
        prepared, preparation_failures = prepare_packages(inventories, Path(scratch), no_build=no_build)
        failed += len(preparation_failures)
        for (alias, device), archives in zip(owners, prepared, strict=True):
            failed += deploy_prepared(alias, device, archives, installed_only=installed_only,
                                      check_inventory=False, **options)
    return failed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("plugin", help="plugin folder (control, clipboard, ...) or id; `all`: every plugin already installed on each device")
    parser.add_argument("--to", nargs="*", default=["all"], help="device names, `local`, or `all` (default)")
    parser.add_argument("--no-build", action="store_true", help="install the module built last")
    consent = parser.add_mutually_exclusive_group()
    consent.add_argument("--review-in-app", action="store_true",
                         help="Wait for the person's decision in the app and restore compatible grants after installation")
    consent.add_argument("--accept", metavar="DIGEST",
                         help="the digest of the licence and notice you read and accept (printed by `peersus plugin-install`)")
    consent.add_argument("--accept-current-agreement", action="store_true",
                         help="accept each package's own licence and notice as it is now (the person said so for this run)")
    parser.add_argument("--accept-outside-store-risk", action="store_true",
                        help="you read the application's warning about a package from outside the store")
    parser.add_argument("--ssh", nargs="*", default=["hp"],
                        help="ssh hosts of other desktops, as `./dev.sh deploy` reaches them (default: hp); an unreachable one is skipped")
    args = parser.parse_args()
    wanted = {name.lower() for name in args.to}

    devices = []
    try:
        devices.append(Local())
    except Exception as error:
        print(f"this computer: skipped ({error})")
    for transport in ([] if wanted == {"local"} else phones()):
        try:
            devices.append(Phone(transport))
        except Exception as error:
            print(f"a phone: skipped ({error})")
    for host in ([] if wanted == {"local"} else args.ssh):
        try:
            devices.append(Remote(host))
        except Exception as error:
            print(f"{host}: skipped ({error})")

    def chosen(device) -> bool:
        return "all" in wanted or device.name.lower() in wanted or ("local" in wanted and isinstance(device, Local))

    targets = [d for d in devices if chosen(d)]
    available = {d.name.lower() for d in devices}
    if any(isinstance(d, Local) for d in devices):
        available.add("local")
    missing = wanted - {"all"} - available
    if missing:
        raise SystemExit(f"not reachable: {', '.join(sorted(missing))}; nothing installed")
    if not targets:
        raise SystemExit("no device to install on")

    failed = deploy_packages(((d.name, d) for d in targets), plugin=args.plugin, no_build=args.no_build,
                             agreement=args.accept, review_in_app=args.review_in_app,
                             accept_current_agreement=args.accept_current_agreement,
                             accept_outside_store_risk=args.accept_outside_store_risk)
    sys.exit(1 if failed else 0)


def agreement_digest(archive: Path) -> str:
    """The digest of the licence and notice this package carries, as the application computes it (`peersus plugin-install`
    prints it without installing anything)."""
    with tempfile.TemporaryDirectory() as unpacked:
        with tarfile.open(archive) as tar:
            tar.extractall(unpacked, filter="data")
        root = next(p for p in Path(unpacked).iterdir() if p.is_dir())
        installed_cli = Path.home() / ".local/bin/peersus"
        cli = str(installed_cli) if installed_cli.is_file() else shutil.which("peersus")
        if cli is None:
            raise RuntimeError("the Peersus CLI is required to inspect the package's agreement")
        shown = subprocess.run([cli, "plugin-install", str(root)], capture_output=True, text=True, timeout=30)
        found = re.search(r"^  peersus plugin-install .* --accept ([0-9a-f]{64}) --accept-outside-store-risk$",
                          shown.stdout, re.MULTILINE)
        if not found:
            raise RuntimeError("the application did not print an agreement digest for the reviewed package")
        return found[1]


if __name__ == "__main__":
    main()
