#!/usr/bin/env python3
"""Restart the desktop session onto the installed build, as the deployment user: `local` on this machine or `ssh`
on a remote desktop, where `./dev.sh deploy` sends this file to `python3 -`. It uses the installed login service
when that owns the control socket, and otherwise launches in the graphical user's systemd manager."""
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import time

prefix = (Path.home() / ".local").resolve()
bin_dir = (prefix / "bin").resolve()
library = (prefix / "lib/peersus").resolve()
legacy_owner = bin_dir / "peersus"
owner, ui = library / "peersus", library / "peersus-ui"
unit = "peersus.service"
ctl = ["systemctl", "--user"]

def run(*args, check=True):
    return subprocess.run(args, check=check, capture_output=True, text=True, timeout=15)

def executable(pid):
    try:
        target = Path(os.readlink(f"/proc/{pid}/exe").removesuffix(" (deleted)"))
        # Keep the name of a deleted executable even if that name now points to the new launcher.
        return target.parent.resolve() / target.name
    except (FileNotFoundError, PermissionError):
        return None

def matching_processes(paths):
    for path in Path("/proc").iterdir():
        if path.name.isdecimal() and executable(path.name) in paths:
            try:
                if path.stat().st_uid == os.getuid():
                    yield int(path.name)
            except FileNotFoundError:
                continue

data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
control = data / "peersus/control.sock"

def current_owner():
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(1)
        try:
            client.connect(str(control))
        except (FileNotFoundError, ConnectionRefusedError):
            return None
        pid, uid, _ = struct.unpack("3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        if uid != os.getuid():
            raise RuntimeError("Control socket does not belong to the deployment user")
        return pid

def wait_for(test):
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if test():
            return
        time.sleep(.25)
    raise RuntimeError("Desktop did not stop or start in time")

def same_binary(pid, binary):
    try:
        return pid is not None and os.path.samefile(f"/proc/{pid}/exe", binary)
    except FileNotFoundError:
        return False

env = {}
if sys.argv[1] == "local":
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP"):
        if os.environ.get(key):
            env[key] = os.environ[key]
# Use the same directory as the control client, including if the user manager has a different override.
env["XDG_DATA_HOME"] = str(data)
paths = {ui, bin_dir / "peersus-ui"}
owner_paths = {owner, legacy_owner}
launcher = ["systemd-run", "--user", "--quiet", "--collect",
            *(f"--setenv={key}={value}" for key, value in env.items())]

if len(sys.argv) == 2:
    previous = current_owner()
    old_owner = executable(previous) if previous is not None else None
    if previous is not None and old_owner not in owner_paths:
        raise RuntimeError("Control socket owner is not the Peersus executable")
    windows = list(matching_processes(paths))
    service = run(*ctl, "is-active", "--quiet", unit, check=False).returncode == 0
    if service:
        main_pid = int(run(*ctl, "show", "-p", "MainPID", "--value", unit).stdout)
        if previous is not None and main_pid != previous:
            raise RuntimeError("The login service is not the owner of the control socket")
        if executable(main_pid) not in owner_paths:
            raise RuntimeError("The login service is configured for a different installation")
    launch_window = bool(windows) or not service
    if launch_window:
        # Use the actual launch environment without reading the manager's full environment.
        run(*launcher, "--wait", "--pipe", "/bin/sh", "-c", 'test -n "$DISPLAY" || test -n "$WAYLAND_DISPLAY"')
    print(json.dumps([previous, str(old_owner) if old_owner is not None else None, windows, service]))
    sys.exit(0)

previous, old_owner, windows, service = json.loads(sys.argv[2])
old_owner = Path(old_owner) if old_owner is not None else None
launch_window = bool(windows) or not service


if service:
    run(*ctl, "stop", unit)
elif previous is not None and current_owner() == previous and executable(previous) == old_owner:
    run(str(owner), "stop")
for pid in windows:
    try:
        fd = os.pidfd_open(pid)
    except ProcessLookupError:
        continue
    try:
        if executable(pid) in paths:
            signal.pidfd_send_signal(fd, signal.SIGTERM)
    except ProcessLookupError:
        pass
    finally:
        os.close(fd)
wait_for(lambda: current_owner() is None and (previous is None or executable(previous) != old_owner)
         and not any(matching_processes(paths)))
if service:
    run(*ctl, "start", unit)
if launch_window:
    # The owner started by the window may be configured to keep running after the window closes.
    run(*launcher, "--property=ExitType=cgroup", str(legacy_owner))
wait_for(lambda: same_binary(current_owner(), owner) and (not launch_window or any(
    same_binary(pid, ui) for pid in matching_processes({ui}))))
