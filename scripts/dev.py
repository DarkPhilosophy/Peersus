#!/usr/bin/env python3
"""Build development artifacts or explicitly update selected phones and desktops, never uninstall."""
import argparse
from dataclasses import dataclass
import hashlib
import json
from importlib import import_module
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from build_layout import cargo_artifact_directory, environment as layout_environment

# The existing CLI is also the extension deploy implementation; its filename is not a Python identifier.
extensions = import_module("deploy-plugin")

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "ro.peersus"
DEFAULT_APK = ROOT / "target/distribution/android/community/debug/app-community-debug.apk"
RELEASE_PROGRAMS = ("peersus", "peersus-ui", "peersus-wasm-host", "peersus-wasm-canvas-host", "peersus-store-fetch", "peersus-diag")
PHONES = ("nx", "rog")
# Each desktop and how it is reached: `None` is this workstation; otherwise the host name in the user's own ssh configuration.
DESKTOPS = {"msi": None, "hp": "hp"}
# Where the staged tree is copied on a remote desktop before it is installed there.
REMOTE_STAGE = "/tmp/peersus-stage"
NEEDED_PROGRAMS = (*RELEASE_PROGRAMS, "peersus-native-host")


class Failure(Exception):
    pass


class Offline(Failure):
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


def build_launcher(container):
    return (["distrobox", "enter", "--name", container, "--", "sh", "-c",
             'export PATH="$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"; exec "$@"', "dev-build"]
            if container else [])


def xtask(arguments, environment, container=None, purpose="Development build"):
    command = [*build_launcher(container), "cargo", "xtask", *arguments]
    result = subprocess.run(command, cwd=ROOT, env=environment)
    if result.returncode:
        raise Failure(f"{purpose} failed (exit {result.returncode})")


def build(target, container=None, private_store=False, *, build_number=None):
    environment = build_environment(target)
    # This is a non-secret compile-time switch, never an application credential.
    # Set both sides explicitly so inherited flags cannot enable private mode.
    environment["PEERSUS_PRIVATE_STORE_BUILD"] = "1" if private_store else "0"
    if build_number is not None:
        environment["PEERSUS_BUILD_NUMBER"] = str(build_number)
    def task(name, in_container=False):
        xtask([name], environment, container if in_container else None)
    if target == "desktop":
        task("dev", bool(container))
    elif container:
        task("android-dev-native", True)
        if build_number is not None:
            verify_android_native(environment)
        # Host Gradle uses its existing debug signer, not a container's unrelated debug identity.
        task("android-dev-apk")
    else:
        task("android-dev")
        if build_number is not None:
            verify_android_native(environment)


def verify_android_native(environment):
    target = ROOT / environment["CARGO_TARGET_DIR"]
    try:
        for triple in ("aarch64-linux-android", "x86_64-linux-android"):
            release = target / triple / "release"
            for name in ("libpeersus_android.so", "libpeersus_plugin_android.so"):
                extensions.verify_cargo_output(release / name, ROOT)
    except RuntimeError as error:
        raise Failure(str(error)) from error


def build_desktop(directory, container=None, private_store=False):
    environment = build_environment("desktop")
    # A non-secret compile-time switch; both sides are set so an inherited flag cannot enable it.
    environment["PEERSUS_PRIVATE_STORE_BUILD"] = "1" if private_store else "0"
    # Never leave a diagnostics-enabled artifact where release staging looks for a product build.
    environment["CARGO_TARGET_DIR"] = str(Path(environment["CARGO_TARGET_DIR"]) / "dev-deploy")
    xtask(["dev-deploy"], environment, container, "Optimized desktop development build")
    release = ROOT / cargo_artifact_directory("release", environment)
    try:
        for name in RELEASE_PROGRAMS:
            extensions.verify_cargo_output(release / name, ROOT)
        native = release / "peersus-native-host"
        native_source = ROOT / "crates/peersus-plugin-runtime/native/host.c"
        if native.stat().st_mtime_ns < native_source.stat().st_mtime_ns:
            raise RuntimeError("peersus-native-host is older than its build input")
    except RuntimeError as error:
        raise Failure(str(error)) from error
    stage = directory / "linux"
    result = subprocess.run([sys.executable, str(ROOT / "scripts/stage-linux.py"), "--out", str(stage)],
                            cwd=ROOT, env=environment)
    if result.returncode:
        raise Failure("Desktop development staging failed")
    return stage


def build_apk(minimum_version, directory, container=None, private_store=False):
    try:
        build_number = max(int(os.environ.get("PEERSUS_BUILD_NUMBER", "0")), minimum_version - 1, 0)
    except ValueError as error:
        raise Failure("PEERSUS_BUILD_NUMBER must be an integer") from error
    started = time.time_ns()
    build("android", container=container, private_store=private_store, build_number=build_number)
    if not DEFAULT_APK.is_file() or DEFAULT_APK.stat().st_mtime_ns < started:
        raise Failure("Android build did not produce a fresh APK; nothing installed")
    candidate = directory / DEFAULT_APK.name
    shutil.copyfile(DEFAULT_APK, candidate)
    return candidate


def bench(container, options):
    """Measure synthetic local workloads; compilation may use a container, the window runs on the host."""
    environment = build_environment("desktop")
    if container:
        xtask(["bench-build"], environment, container, "Benchmark build")
        if "--sizes" in options:
            # The programs whose sizes are reported are the product's, built the same way.
            xtask(["build"], environment, container, "Product build for sizes")
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
            raise Offline(f"{alias.upper()}: no authorized online device found")
        if len(identities) != 1 or not next(iter(identities)):
            raise Failure(f"{alias.upper()}: device identity is ambiguous; refusing deployment")
        # Prefer the ordinary transport over a duplicate mDNS route to the same physical phone.
        chosen.append(min(matches, key=lambda device: ("_adb-tls" in device.transport, device.transport)))
    return chosen


def discover(requested=PHONES):
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
        if alias in requested:
            try:
                identity = capture(["adb", "-s", transport, "shell", "getprop", "ro.serialno"])
            except (Failure, subprocess.TimeoutExpired):
                continue  # A wireless route can disappear while adb is listing it.
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


def apk_badging(apk, aapt):
    text = capture([aapt, "dump", "badging", str(apk)])
    package = re.search(r"^package: name='" + re.escape(PACKAGE) + r"' versionCode='([0-9]+)'", text, re.MULTILINE)
    if not package:
        raise Failure("APK identity or versionCode could not be verified")
    return int(package[1]), "application-debuggable" in text


def preflight_phones(apk, previous, tools):
    apksigner, aapt = tools
    version, debuggable = apk_badging(apk, aapt)
    if not debuggable:
        raise Failure("Development deployment requires a debuggable APK")
    expected_signer = signer(apk, apksigner)
    for alias, (installed, installed_version) in previous.items():
        if installed is not None and signer(installed, apksigner) != expected_signer:
            raise Failure(f"{alias.upper()}: signing certificate differs; update refused, existing application and data untouched")
        if installed_version > version:
            raise Failure(f"{alias.upper()}: built APK versionCode is below the installed version; nothing installed")
    return digest(apk)


def install_phone(apk, device, directory, expected_digest):
    actual_identity = capture(["adb", "-s", device.transport, "shell", "getprop", "ro.serialno"])
    if actual_identity != device.identity:
        raise Failure(f"{device.alias.upper()}: device changed after preflight; update refused")
    capture(["adb", "-s", device.transport, "install", "-r", str(apk)])
    installed = installed_apk(device, directory / f"{device.alias}-after.apk")
    if installed is None or digest(installed) != expected_digest:
        raise Failure(f"{device.alias.upper()}: installation returned, but installed APK differs; no automatic retry")
    print(f"{device.alias.upper()}: development APK installed and byte-for-byte verified; no uninstall or data clear")


def restart_phone(device):
    shell = ["adb", "-s", device.transport, "shell"]
    capture([*shell, "am", "force-stop", PACKAGE])
    # `am start` with only an action, category and package resolves as an implicit intent, which
    # also requires CATEGORY_DEFAULT; a launcher entry does not declare it, so Android refuses
    # ("unable to resolve"). Resolve the launcher activity the way a home screen does, then start
    # that exact component.
    resolved = capture([*shell, "cmd", "package", "resolve-activity", "--brief", "--user", "0",
                        "-a", "android.intent.action.MAIN", "-c", "android.intent.category.LAUNCHER", PACKAGE])
    component = resolved.strip().splitlines()[-1].strip() if resolved.strip() else ""
    if not component.startswith(f"{PACKAGE}/"):
        raise Failure("Android reported no launcher activity for Peersus")
    result = capture([*shell, "am", "start", "-W", "--user", "0", "-n", component])
    if not re.search(r"^Status: ok\s*$", result, re.MULTILINE) or re.search(r"^Error", result, re.MULTILINE):
        raise Failure("Android did not confirm the launcher activity start")


def staged_programs(stage):
    """The programs of a staged tree and their digests, refused when the tree is incomplete."""
    bin_dir = stage / "bin"
    files = {path.name: path for path in bin_dir.iterdir()} if bin_dir.is_dir() and not bin_dir.is_symlink() else {}
    if files.keys() != set(NEEDED_PROGRAMS) or any(path.is_symlink() or not path.is_file() for path in files.values()):
        raise Failure("The staged desktop tree is incomplete or has unexpected programs; stage a fresh build")
    return {name: digest(files[name]) for name in NEEDED_PROGRAMS}


def ssh(host, script, stdin=None):
    """Run `script` with bash on `host` (whatever its login shell is), without asking for a password."""
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host,
                             "bash -c " + shlex.quote(script)], cwd=ROOT, input=stdin,
                            capture_output=True, text=True, timeout=300)
    if result.returncode == 255:
        raise Offline(f"{host.upper()}: SSH is not reachable")
    if result.returncode:
        raise Failure(f"{host.upper()}: remote operation failed; nothing was removed")
    return result.stdout


def installed_programs(host, names):
    """The digests of the programs installed for the user on `host` (this workstation when `None`)."""
    if host is None:
        directory = Path.home() / ".local/lib/peersus"
        launcher = Path.home() / ".local/bin/peersus"
        if not launcher.is_symlink() or launcher.resolve() != directory.joinpath("peersus").resolve():
            return {}
        return {name: digest(directory / name) for name in names
                if (directory / name).is_file() and not (directory / name).is_symlink()}
    listing = ssh(host, 'dir=$(realpath -e -- "$HOME/.local/lib/peersus") || exit 1; launcher="$HOME/.local/bin/peersus"; '
                  'test -L "$launcher" && test "$(readlink -f -- "$launcher")" = "$dir/peersus" || exit 1; ' +
                  " ".join(f'test ! -L "$dir"/{shlex.quote(name)} && sha256sum -- "$dir"/{shlex.quote(name)} || exit 1;' for name in names))
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


def preflight_desktops(stage, requested, *, offline=None):
    """Check every reachable desktop, including whether the staged binaries can load there."""
    if len(requested) != len(set(requested)):
        raise Failure("Select each desktop only once")
    stage = stage.resolve(strict=True)
    programs = staged_programs(stage)
    needed = glibc_needed(stage)
    for alias in requested:
        host = DESKTOPS[alias]
        try:
            said = os.confstr("CS_GNU_LIBC_VERSION") if host is None else ssh(host, "getconf GNU_LIBC_VERSION")
        except Offline:
            if offline is None:
                raise
            offline.add(alias)
            continue
        have = glibc_version(said)
        if needed and (not have or have < needed):
            available = ".".join(map(str, have)) if have else "an unknown version"
            raise Failure(
                f"{alias.upper()}: the staged programs need glibc {needed[0]}.{needed[1]} and it has {available}; "
                "build them where the C library is no newer (./dev.sh desktop --container NAME, or ./build.sh desktop "
                "inside that container), stage again; nothing was installed")
    return programs


# No command or owner IPC exposes the window's login-service switch. Inspect the
# service before publishing a new launcher, but never edit or reload the unit here.
DESKTOP_LOGIN_CHECK = r"""
from pathlib import Path
import subprocess

result = subprocess.run(
    ["systemctl", "--user", "show", "peersus.service",
     "--property=LoadState,ActiveState,UnitFileState,NeedDaemonReload,DropInPaths,ExecStart,FragmentPath"],
    capture_output=True, text=True, timeout=15,
)
unit = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
prefix = (Path.home() / ".local").resolve()
command = unit.get("ExecStart", "")

def launches(program, arguments):
    executable, separator, _ = command.removeprefix("{ path=").partition(" ; argv[]=")
    if not separator or not Path(executable).is_absolute() or Path(executable).resolve() != program.resolve():
        return False
    expected = f"{{ path={executable} ; argv[]={executable} {arguments} ; ignore_errors=no ; "
    return command.startswith(expected) and command.endswith("}") and "{" not in command[len(expected):]

idle = unit.get("ActiveState") in {"inactive", "failed"}
if unit.get("LoadState") == "not-found" and idle and unit.get("UnitFileState") == "":
    print("absent")
elif result.returncode or not unit.get("ActiveState") or not unit.get("LoadState"):
    print("unavailable")
elif idle and unit.get("UnitFileState") in {"disabled", "masked", "masked-runtime"}:
    fragment = Path(unit.get("FragmentPath", ""))
    if (unit["UnitFileState"] == "disabled" and launches(prefix / "bin/peersus", "--service")
            and fragment.is_absolute() and fragment.name == "peersus.service"):
        print(f"inactive-legacy\t{fragment}")
    else:
        print("inactive")
else:
    # Distribution-wide service.d defaults apply to every user service, not this unit alone.
    unit_dropins = any(Path(path).parent.name == "peersus.service.d"
                       for path in unit.get("DropInPaths", "").split())
    current = (unit["LoadState"] == "loaded" and unit.get("NeedDaemonReload") == "no"
               and not unit_dropins and launches(prefix / "lib/peersus/peersus", "owner --service"))
    print("current" if current else "blocked")
"""


def check_desktop_login(alias):
    host = DESKTOPS[alias]
    if host is None:
        state = capture([sys.executable, "-c", DESKTOP_LOGIN_CHECK])
    else:
        state = ssh(host, "python3 -", stdin=DESKTOP_LOGIN_CHECK).strip()
    state, _, fragment = state.partition("\t")
    if state == "inactive-legacy":
        print(f"{alias.upper()}: disabled login unit {fragment} left unchanged; it still uses the old command. "
              "Start when you sign in will be refused until that file is removed.")
    elif state == "inactive":
        print(f"{alias.upper()}: inactive Peersus login unit left unchanged")
    elif state == "blocked":
        raise Failure(f"{alias.upper()}: active or enabled Peersus login unit needs review; "
                      "nothing was installed and the unit was left unchanged")
    elif state not in ("absent", "current"):
        raise Failure(f"{alias.upper()}: could not verify the Peersus login unit; nothing was installed")


def deploy_desktops(stage, requested, programs):
    """Install the staged tree for the user with its own `peersus install`, and verify it before any restart."""
    stage = stage.resolve(strict=True)
    reports = {}
    for alias in requested:
        check_desktop_login(alias)
        plan = preflight_desktop_session(alias)
        host = DESKTOPS[alias]
        if host is None:
            receipt = capture([str(stage / "bin/peersus"), "install"])
        else:
            capture(["rsync", "-a", "--delete", f"{stage}/", f"{host}:{REMOTE_STAGE}/"])
            receipt = ssh(host, f"{shlex.quote(REMOTE_STAGE + '/bin/peersus')} install")
        if installed_programs(host, programs) != programs:
            raise Failure(f"{alias.upper()}: installation returned, but the installed programs differ; no automatic retry")
        print(f"{alias.upper()}: staged desktop build installed for this user and byte-for-byte verified")
        reports[alias] = plan, [line for line in receipt.splitlines()
                                if line.startswith("The launcher of an older layout")]
    return reports


# The same restart runs as the deployment user locally and over SSH. Use the installed login
# service when it owns the socket; otherwise launch in the graphical user's systemd manager.
DESKTOP_RESTART = (Path(__file__).parent / "desktop-session.py").read_text()


def desktop_session(alias, *arguments):
    host = DESKTOPS[alias]
    if host is None:
        return capture([sys.executable, "-c", DESKTOP_RESTART, "local", *arguments])
    return ssh(host, shlex.join(["python3", "-", "ssh", *arguments]), stdin=DESKTOP_RESTART)


def preflight_desktop_session(alias):
    try:
        return json.loads(desktop_session(alias))
    except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
        raise Failure(f"{alias.upper()}: desktop session preflight failed; nothing was installed") from error


def restart_desktop(alias, plan):
    desktop_session(alias, json.dumps(plan))


def wait_for_owner(alias, phone):
    if phone is not None:
        connect = lambda: extensions.Phone(phone.transport, ready_timeout=3)
    elif DESKTOPS[alias] is None:
        connect = lambda: extensions.Local(ready_timeout=3)
    else:
        connect = lambda: extensions.Remote(DESKTOPS[alias], ready_timeout=3)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            return connect()
        except (RuntimeError, OSError, subprocess.TimeoutExpired, KeyError, ValueError):
            time.sleep(.5)
    raise Failure("the restarted Peersus owner did not answer on its control socket")


def extension_inventory(alias, phone, previous):
    if phone is not None:
        device = extensions.Phone(phone.transport, connect=False)
        names = device.installed_names() if previous else set()
    else:
        host = DESKTOPS[alias]
        device = extensions.Local(connect=False) if host is None else extensions.Remote(host, connect=False)
        names = device.installed_names()
    return device.platform, names


def deploy_selected(requested, *, container=None, private_store=False, app_only=False,
                    accept_current_agreement=False, accept_outside_store_risk=False):
    if len(requested) != len(set(requested)):
        raise Failure("Select each physical device only once")
    phones = [alias for alias in requested if alias in PHONES]
    desktops = [alias for alias in requested if alias in DESKTOPS]
    offline = set()
    discovered = discover(phones) if phones else []
    devices = {}
    for alias in phones:
        try:
            devices[alias], = choose_devices(discovered, [alias])
        except Offline:
            offline.add(alias)
    for alias in desktops:
        host = DESKTOPS[alias]
        if host is not None:
            try:
                ssh(host, "true")
            except Offline:
                offline.add(alias)
    for alias in requested:
        if alias in offline:
            print(f"{alias.upper()}: offline; not updated")
    desktops = [alias for alias in desktops if alias not in offline]
    if not desktops and not devices:
        return 1

    with tempfile.TemporaryDirectory(prefix="dev-deploy-") as temporary:
        directory = Path(temporary)
        previous = {}
        if devices:
            tools = android_tool("apksigner"), android_tool("aapt2")
            for alias, device in devices.items():
                installed = installed_apk(device, directory / f"{alias}-before.apk")
                previous[alias] = installed, apk_badging(installed, tools[1])[0] if installed is not None else 0
        # Compatibility and all local builds complete before the first device is modified.
        if desktops:
            stage = build_desktop(directory, container, private_store)
            online = offline.copy()
            programs = preflight_desktops(stage, desktops, offline=offline)
            for alias in requested:
                if alias in offline - online:
                    print(f"{alias.upper()}: offline; not updated")
        if devices:
            apk = build_apk(max(version for _, version in previous.values()), directory, container, private_store)
            expected_digest = preflight_phones(apk, previous, tools)
        eligible = [alias for alias in requested if alias not in offline]
        failed = bool(offline)
        archives = {}
        if not app_only:
            inventories = []
            for alias in eligible:
                try:
                    platform, names = extension_inventory(
                        alias, devices.get(alias), previous.get(alias, (None, 0))[0])
                    inventories.append((alias, platform, names))
                except (OSError, RuntimeError, subprocess.SubprocessError, ValueError):
                    print(f"{alias.upper()}: FAILED to read installed extensions; application not changed")
                    failed = True
            prepared, build_failures = extensions.prepare_packages(inventories, directory / "extensions",
                                                                  launcher=build_launcher(container))
            for index, ((alias, _, _), packages) in enumerate(zip(inventories, prepared, strict=True)):
                if index in build_failures:
                    print(f"{alias.upper()}: application not changed because an extension build failed")
                    failed = True
                else:
                    archives[alias] = packages
            eligible = [alias for alias in eligible if alias in archives]

        for alias in eligible:
            phone = devices.get(alias)
            phase = "install and verify application"
            try:
                if phone is None:
                    migration = deploy_desktops(stage, [alias], programs)
                else:
                    install_phone(apk, phone, directory, expected_digest)
                phase = "restart"
                if phone is None:
                    restart_desktop(alias, migration[alias][0])
                else:
                    restart_phone(phone)
                owner = wait_for_owner(alias, phone)
                print(f"{alias.upper()}: app restarted; control socket answered")
                if phone is None:
                    if installed_programs(DESKTOPS[alias], programs) != programs:
                        raise Failure("The running installation no longer matches the staged programs")
                    for line in migration[alias][1]:
                        print(f"{alias.upper()}: {line}")
                if app_only:
                    print(f"{alias.upper()}: extension update explicitly skipped (--app-only)")
                else:
                    failed = bool(extensions.deploy_prepared(
                        alias, owner, archives[alias], installed_only=True,
                        accept_current_agreement=accept_current_agreement,
                        accept_outside_store_risk=accept_outside_store_risk,
                    )) or failed
            except (Failure, OSError, subprocess.TimeoutExpired) as error:
                cause = str(error) if isinstance(error, Failure) else "operation failed or timed out; inspect actual state before retrying"
                print(f"{alias.upper()}: FAILED ({phase}): {cause}")
                failed = True
    return int(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for target in ("desktop", "android"):
        command = commands.add_parser(target, help=f"Build {target} development artifacts, without installation")
        command.add_argument("--container", help="Use this existing Distrobox for compilation; Android debug signing stays on the host")
        command.add_argument("--private-store", action="store_true", help="Build the authenticated private development catalog UI and transport; no token is read by the build")
    deploy_summary = "Build, install and restart Peersus, then update its already-installed extensions"
    command = commands.add_parser("deploy", help=deploy_summary, description=deploy_summary)
    command.add_argument("devices", nargs="+", choices=(*PHONES, *DESKTOPS))
    command.add_argument("--container", help="Existing Distrobox for compilation; Android debug signing stays on the host")
    command.add_argument("--private-store", action="store_true", help="Also build the authenticated private development catalog for the desktop and the Android APK; every deploy follows the development update channel either way")
    command.add_argument("--app-only", action="store_true",
                         help="Explicitly skip checking and updating installed extensions; still restart the application")
    command.add_argument("--accept-current-agreement", action="store_true",
                         help="Accept the current licence and notice for each changed package in this run")
    command.add_argument("--accept-outside-store-risk", action="store_true",
                         help="Acknowledge the application's warning if not previously accepted on the device")
    command = commands.add_parser("bench", help="Measure synthetic local workloads in release; writes target/bench/latest.json, deploys nothing")
    command.add_argument("--container", help="Use this existing Distrobox for compilation; the programs run on the host")
    command.add_argument("--seconds", type=int, help="How long each window run measures (default 10)")
    command.add_argument("--save", metavar="NAME", help="Keep this run in benchmark-results/ under NAME")
    command.add_argument("--compare", metavar="NAME", help="Print the change against the kept run NAME, with a verdict")
    command.add_argument("--sizes", action="store_true", help="Also build the product and report the size of each program")
    command.add_argument("--no-window", action="store_true", help="Leave the real-window runs out (no display needed)")
    command.add_argument("--on-screen", action="store_true", help="Use the session's display instead of a private in-memory one")
    args = parser.parse_args()
    try:
        if args.command == "deploy":
            return deploy_selected(args.devices, container=args.container, private_store=args.private_store, app_only=args.app_only,
                                   accept_current_agreement=args.accept_current_agreement,
                                   accept_outside_store_risk=args.accept_outside_store_risk)
        elif args.command == "bench":
            options = [f"--{name}={value}" for name in ("seconds", "save", "compare") if (value := getattr(args, name)) is not None]
            options += [f"--{name.replace('_', '-')}" for name in ("sizes", "no_window", "on_screen") if getattr(args, name)]
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
