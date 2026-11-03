#!/usr/bin/env python3
"""Build development artifacts or explicitly update selected phones, never uninstall."""
import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from build_layout import environment as layout_environment

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "ro.peersus"
DEFAULT_APK = ROOT / "target/distribution/android/community/debug/app-community-debug.apk"


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
    environment = layout_environment(target)
    environment.setdefault("CARGO_BUILD_JOBS", "2")
    if target == "android":
        sdk = environment.get("ANDROID_HOME") or environment.get("ANDROID_SDK_ROOT")
        if not sdk:
            properties = ROOT / "android/local.properties"
            if properties.is_file():
                for line in properties.read_text().splitlines():
                    if line.startswith("sdk.dir="):
                        sdk = line.split("=", 1)[1].strip()
        if not sdk:
            raise Failure("Set ANDROID_HOME to the installed Android SDK; prerequisites are never installed automatically")
        environment.setdefault("ANDROID_HOME", sdk)
        environment.setdefault("ANDROID_NDK_HOME", str(Path(sdk) / "ndk/27.2.12479018"))
        environment.setdefault("ANDROID_NDK", environment["ANDROID_NDK_HOME"])
    return environment


def build(target, container=None):
    environment = build_environment(target)
    def task(name, in_container=False):
        command = ["cargo", "xtask", name]
        if in_container:
            command = ["distrobox", "enter", "--name", container, "--", "sh", "-c",
                       'export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"; exec "$@"', "dev-build", *command]
        result = subprocess.run(command, cwd=ROOT, env=environment)
        if result.returncode:
            raise Failure(f"Development build failed (exit {result.returncode})")
    if target == "desktop":
        task("dev", bool(container))
    elif container:
        task("android-dev-native", True)
        # Host Gradle uses its existing debug signer, not a container's unrelated debug identity.
        task("android-dev-apk")
    else:
        task("android-dev")


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for target in ("desktop", "android"):
        command = commands.add_parser(target, help=f"Build {target} development artifacts, without installation")
        command.add_argument("--container", help="Use this existing Distrobox for compilation; Android debug signing stays on the host")
    command = commands.add_parser("deploy", help="Update only the explicitly selected phones; preserve application data")
    command.add_argument("devices", nargs="+", choices=("nx", "rog"))
    command.add_argument("--apk", type=Path, default=DEFAULT_APK)
    args = parser.parse_args()
    try:
        if args.command == "deploy":
            deploy(args.apk, args.devices)
        else:
            build(args.command, args.container)
    except (Failure, OSError, subprocess.TimeoutExpired):
        # Do not expose adb identifiers, infrastructure or local paths in generic errors.
        error = sys.exc_info()[1]
        print(str(error) if isinstance(error, Failure) else "Development operation failed or timed out; inspect actual state before retrying", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
