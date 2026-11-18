#!/usr/bin/env python3
"""Build development artifacts or explicitly update selected phones and desktops, never uninstall."""
import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import shlex
import sys
import tempfile
from build_layout import environment as layout_environment

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "ro.peersus"
DEFAULT_APK = ROOT / "target/distribution/android/community/debug/app-community-debug.apk"
# What `scripts/stage-linux.py` writes: the tree a desktop is updated from.
DEFAULT_STAGE = ROOT / "target/ui/distribution/linux"
PHONES = ("nx", "rog")
# Each desktop and how it is reached: `None` is this workstation; otherwise the host name in the user's own ssh configuration.
DESKTOPS = {"msi": None, "hp": "hp"}
# Where the staged tree is copied on a remote desktop before it is installed there.
REMOTE_STAGE = "/tmp/peersus-stage"
# The two programs a staged tree must hold; every program it holds is installed and checked.
NEEDED_PROGRAMS = ("peersus", "peersus-ui")


class Failure(Exception):
    pass


def capture(command):
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=120)
    if result.returncode:
        # adb errors can contain device identifiers. Report the error category, not those identifiers.
        category = re.search(r"INSTALL_FAILED_[A-Z_]+", result.stdout + result.stderr)
        raise Failure(category.group() if category else "Device/tool operation failed; no uninstall was attempted")
    return result.stdout.strip()


def build_environment(target):
    # The layout and, for Android, the SDK pieces: one derivation, the same `./build.sh` evaluates.
    environment = layout_environment(target)
    if target == "android" and "ANDROID_HOME" not in environment:
        raise Failure("Set ANDROID_HOME to the installed Android SDK; prerequisites are never installed automatically")
    return environment


def xtask(arguments, environment, container=None, purpose="Development build"):
    command = ["cargo", "xtask", *arguments]
    if container:
        command = ["distrobox", "enter", "--name", container, "--", "sh", "-c",
                   'export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"; exec "$@"', "dev-build", *command]
    result = subprocess.run(command, cwd=ROOT, env=environment)
    if result.returncode:
        raise Failure(f"{purpose} failed (exit {result.returncode})")


def build(target, container=None, private_store=False):
    environment = build_environment(target)
    # This is a non-secret compile-time switch, never an application credential.
    # Set both sides explicitly so inherited flags cannot enable private mode.
    environment["PEERSUS_PRIVATE_STORE_BUILD"] = "1" if private_store else "0"
    def task(name, in_container=False):
        xtask([name], environment, container if in_container else None)
    if target == "desktop":
        task("dev", bool(container))
    elif container:
        task("android-dev-native", True)
        # Host Gradle uses its existing debug signer, not a container's unrelated debug identity.
        task("android-dev-apk")
    else:
        task("android-dev")


def bench(container, options):
    """Measure synthetic local workloads; compilation may use a container, the window runs on the host."""
    environment = build_environment("desktop")
    if container:
        xtask(["bench-build"], environment, container, "Benchmark build")
        options = ["--no-build", *options]
    xtask(["bench", *options], environment, purpose="Benchmark")


@dataclass(frozen=True)
class Device:
    alias: str
    transport: str
    identity: str


def choose_devices(devices, requested):
    chosen = []
    for alias in requested:
        matches = [device for device in devices if device.alias == alias]
        identities = {device.identity for device in matches}
        if not matches:
            raise Failure(f"{alias.upper()}: no authorized online device found")
        if len(identities) != 1 or not next(iter(identities)):
            raise Failure(f"{alias.upper()}: device identity is ambiguous; refusing deployment")
        # Prefer the ordinary transport over a duplicate mDNS route to the same physical phone.
        chosen.append(min(matches, key=lambda device: ("_adb-tls" in device.transport, device.transport)))
    return chosen


def discover():
    devices = []
    for line in capture(["adb", "devices", "-l"]).splitlines()[1:]:
        match = re.match(r"^(.*?)\s+device\s+(.*)$", line)
        if not match:
            continue
        transport, details = match.groups()
        model = re.search(r"\bmodel:(\S+)", details)
        if not model:
            continue
        model = model.group(1)
        alias = "nx" if model.startswith("NX") else "rog" if model.startswith("ASUS_AI") else None
        if alias:
            identity = capture(["adb", "-s", transport, "shell", "getprop", "ro.serialno"])
            devices.append(Device(alias, transport, identity))
    return devices


def android_tool(name):
    found = shutil.which(name)
    if found:
        return found
    environment = build_environment("android")
    candidate = Path(environment["ANDROID_HOME"]) / "build-tools/36.0.0" / name
    if not candidate.is_file():
        raise Failure(f"Android build-tools 36.0.0 must provide {name}")
    return str(candidate)


def signer(apk, apksigner):
    text = capture([apksigner, "verify", "--print-certs", str(apk)])
    digests = frozenset(re.findall(r"Signer #\d+ certificate SHA-256 digest:\s*([0-9a-fA-F]{64})", text))
    if not digests:
        raise Failure("APK signer could not be verified")
    return frozenset(digest.lower() for digest in digests)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").digest()


def installed_apk(device, destination):
    text = capture(["adb", "-s", device.transport, "shell", "pm", "path", PACKAGE])
    paths = [line.removeprefix("package:") for line in text.splitlines() if line.startswith("package:")]
    if not paths:
        return None
    base = next((path for path in paths if path.endswith("/base.apk")), None)
    if base is None:
        raise Failure(f"{device.alias.upper()}: installed base APK cannot be identified")
    capture(["adb", "-s", device.transport, "pull", base, str(destination)])
    return destination


def deploy(apk, requested):
    if len(requested) != len(set(requested)):
        raise Failure("Select each physical device only once")
    apk = apk.resolve(strict=True)
    apksigner = android_tool("apksigner")
    badging = capture([android_tool("aapt2"), "dump", "badging", str(apk)])
    if not re.search(r"^package: name='" + re.escape(PACKAGE) + r"'", badging, re.MULTILINE):
        raise Failure("The APK does not contain the expected application")
    if "application-debuggable" not in badging:
        raise Failure("Development deployment requires a debuggable APK")
    expected_signer = signer(apk, apksigner)
    expected_digest = digest(apk)
    devices = choose_devices(discover(), requested)
    with tempfile.TemporaryDirectory(prefix="dev-deploy-") as temporary:
        directory = Path(temporary)
        # Preflight ALL targets before installing anything. Never repair a mismatch by uninstalling.
        for device in devices:
            previous = installed_apk(device, directory / f"{device.alias}-before.apk")
            if previous is not None and signer(previous, apksigner) != expected_signer:
                raise Failure(f"{device.alias.upper()}: signing certificate differs; update refused, existing application and data untouched")
        for device in devices:
            actual_identity = capture(["adb", "-s", device.transport, "shell", "getprop", "ro.serialno"])
            if actual_identity != device.identity:
                raise Failure(f"{device.alias.upper()}: device changed after preflight; update refused")
            capture(["adb", "-s", device.transport, "install", "-r", str(apk)])
            installed = installed_apk(device, directory / f"{device.alias}-after.apk")
            if installed is None or digest(installed) != expected_digest:
                raise Failure(f"{device.alias.upper()}: installation returned, but installed APK differs; no automatic retry")
            print(f"{device.alias.upper()}: development APK installed and byte-for-byte verified; no uninstall or data clear")


def staged_programs(stage):
    """The programs of a staged tree and their digests, refused when the tree is incomplete."""
    programs = {path.name: digest(path) for path in sorted((stage / "bin").glob("*")) if path.is_file()}
    missing = [name for name in NEEDED_PROGRAMS if name not in programs]
    if missing:
        raise Failure("The staged desktop tree is incomplete; run ./build.sh desktop and scripts/stage-linux.py first")
    return programs


def ssh(host, script, stdin=None):
    """Run `script` with bash on `host` (whatever its login shell is), without asking for a password."""
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", host, "bash -c " + shlex.quote(script)],
                            cwd=ROOT, input=stdin, capture_output=True, text=True, timeout=300)
    if result.returncode:
        raise Failure(f"{host.upper()}: remote operation failed; nothing was removed")
    return result.stdout


def installed_programs(host, names):
    """The digests of the programs installed for the user on `host` (this workstation when `None`)."""
    if host is None:
        directory = Path.home() / ".local/bin"
        return {name: digest(directory / name) for name in names if (directory / name).is_file()}
    listing = ssh(host, " ".join(f'sha256sum "$HOME/.local/bin/{shlex.quote(name)}" 2>/dev/null;' for name in names))
    found = {}
    for line in listing.splitlines():
        value, _, path = line.partition("  ")
        found[Path(path).name] = bytes.fromhex(value)
    return found


def glibc_needed(stage):
    """The newest C library version any staged program asks for (its `GLIBC_x.y` version needs), or `None`."""
    newest = None
    for path in sorted((stage / "bin").glob("*")):
        if path.is_file():
            for major, minor in re.findall(rb"GLIBC_(\d+)\.(\d+)", path.read_bytes()):
                version = (int(major), int(minor))
                newest = version if newest is None else max(newest, version)
    return newest


def glibc_version(said):
    """The version in what `getconf GNU_LIBC_VERSION` says (`glibc 2.43`), or `None`."""
    found = re.search(r"(\d+)\.(\d+)", said or "")
    return (int(found[1]), int(found[2])) if found else None


def preflight_desktops(stage, requested):
    """Every selected desktop is checked before anything is installed anywhere: it is reached, and its C library is at
    least as new as the one the staged programs were linked against. A newer workstation links them against a newer
    one, which an older desktop cannot load: the window would not start there at all."""
    if len(requested) != len(set(requested)):
        raise Failure("Select each desktop only once")
    stage = stage.resolve(strict=True)
    programs = staged_programs(stage)
    needed = glibc_needed(stage)
    for alias in requested:
        host = DESKTOPS[alias]
        said = os.confstr("CS_GNU_LIBC_VERSION") if host is None else ssh(host, "getconf GNU_LIBC_VERSION")
        have = glibc_version(said)
        if needed and have and have < needed:
            raise Failure(
                f"{alias.upper()}: the staged programs need glibc {needed[0]}.{needed[1]} and it has {have[0]}.{have[1]}; "
                "build them where the C library is no newer (./dev.sh desktop --container NAME, or ./build.sh desktop "
                "inside that container), stage again; nothing was installed")
    return programs


def deploy_desktops(stage, requested, programs):
    """Install the staged tree for the user on each desktop, as `scripts/install-linux-user.py` does, and verify it.

    Programs are replaced in one step each, so a running application keeps the files it started from: like an
    installed phone application it is the new build from its next start. No data, setting or extension is touched.
    """
    stage = stage.resolve(strict=True)
    installer = (ROOT / "scripts/install-linux-user.py").read_text()
    for alias in requested:
        host = DESKTOPS[alias]
        if host is None:
            capture([sys.executable, str(ROOT / "scripts/install-linux-user.py"), str(stage)])
        else:
            capture(["rsync", "-a", "--delete", f"{stage}/", f"{host}:{REMOTE_STAGE}/"])
            ssh(host, f"python3 - {shlex.quote(REMOTE_STAGE)}", stdin=installer)
        if installed_programs(host, programs) != programs:
            raise Failure(f"{alias.upper()}: installation returned, but the installed programs differ; no automatic retry")
        print(f"{alias.upper()}: staged desktop build installed for this user and byte-for-byte verified; "
              "no data, setting or extension touched; the running application uses it from its next start")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for target in ("desktop", "android"):
        command = commands.add_parser(target, help=f"Build {target} development artifacts, without installation")
        command.add_argument("--container", help="Use this existing Distrobox for compilation; Android debug signing stays on the host")
        command.add_argument("--private-store", action="store_true", help="Build the authenticated private development catalog UI and transport; no token is read by the build")
    command = commands.add_parser("deploy", help="Update only the explicitly selected phones and desktops; preserve application data")
    command.add_argument("devices", nargs="+", choices=(*PHONES, *DESKTOPS))
    command.add_argument("--apk", type=Path, default=DEFAULT_APK, help="The debuggable APK the phones get")
    command.add_argument("--stage", type=Path, default=DEFAULT_STAGE, help="The staged Linux tree the desktops get (scripts/stage-linux.py)")
    command = commands.add_parser("bench", help="Measure synthetic local workloads in release; writes target/bench/latest.json, deploys nothing")
    command.add_argument("--container", help="Use this existing Distrobox for compilation; the programs run on the host")
    command.add_argument("--seconds", type=int, help="How long each window run measures (default 10)")
    command.add_argument("--save", metavar="NAME", help="Keep this run in benchmark-results/ under NAME")
    command.add_argument("--compare", metavar="NAME", help="Print the change against the kept run NAME")
    command.add_argument("--no-window", action="store_true", help="Leave the real-window runs out (no display needed)")
    command.add_argument("--on-screen", action="store_true", help="Use the session's display instead of a private in-memory one")
    args = parser.parse_args()
    try:
        if args.command == "deploy":
            phones = [device for device in args.devices if device in PHONES]
            desktops = [device for device in args.devices if device in DESKTOPS]
            # Every desktop is checked before any phone or desktop is changed; phones check themselves the same way.
            programs = preflight_desktops(args.stage, desktops) if desktops else None
            if phones:
                deploy(args.apk, phones)
            if desktops:
                deploy_desktops(args.stage, desktops, programs)
        elif args.command == "bench":
            options = [f"--{name}={value}" for name in ("seconds", "save", "compare") if (value := getattr(args, name)) is not None]
            options += [f"--{name.replace('_', '-')}" for name in ("no_window", "on_screen") if getattr(args, name)]
            bench(args.container, options)
        else:
            build(args.command, args.container, args.private_store)
    except (Failure, OSError, subprocess.TimeoutExpired):
        # Do not expose adb identifiers, infrastructure or local paths in generic errors.
        error = sys.exc_info()[1]
        print(str(error) if isinstance(error, Failure) else "Development operation failed or timed out; inspect actual state before retrying", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
