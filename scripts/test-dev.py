#!/usr/bin/env python3
"""Deterministic development-tool regressions; never contact a device or run a build."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import build_layout
import dev


class Layout(unittest.TestCase):
    @patch("build_layout.host_triple", return_value="x86_64-unknown-linux-gnu")
    def test_cargo_supplies_the_target_group_and_profile(self, host):
        configured = build_layout.environment("desktop", {})
        self.assertEqual(configured["CARGO_TARGET_DIR"], "target/ui")
        self.assertEqual(configured["CARGO_BUILD_TARGET"], "x86_64-unknown-linux-gnu")
        self.assertEqual(build_layout.artifact_directory("desktop", "debug", {}), Path("target/ui/x86_64-unknown-linux-gnu/debug"))
        with tempfile.TemporaryDirectory() as root:
            self.assertEqual(build_layout.environment("android", {}, Path(root)), {"CARGO_TARGET_DIR": "target/android"})

    def test_android_takes_the_sdk_gradle_names_and_derives_the_pinned_ndk_without_replacing_a_given_one(self):
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "android").mkdir()
            (Path(root) / "android/local.properties").write_text("sdk.dir=/sdk\n")
            configured = build_layout.environment("android", {}, Path(root))
            self.assertEqual(configured["ANDROID_HOME"], "/sdk")
            self.assertEqual(configured["ANDROID_NDK"], "/sdk/ndk/27.2.12479018")
            self.assertEqual(configured["ANDROID_NDK_HOME"], configured["ANDROID_NDK"])
            given = build_layout.environment("android", {"ANDROID_HOME": "/other", "ANDROID_NDK": "/mine"}, Path(root))
            self.assertEqual((given["ANDROID_HOME"], given["ANDROID_NDK"]), ("/other", "/mine"))

    @patch("build_layout.host_triple", return_value="x86_64-unknown-linux-gnu")
    def test_explicit_roots_and_targets_are_not_silently_replaced(self, host):
        for target in ("x86_64-pc-windows-gnu", "aarch64-apple-darwin"):
            configured = {"CARGO_TARGET_DIR": "custom cache", "CARGO_BUILD_TARGET": target}
            self.assertEqual(build_layout.artifact_directory("desktop", "release", configured), Path("custom cache") / target / "release")


class Development(unittest.TestCase):
    def devices(self):
        return [dev.Device("nx", "ordinary-nx", "physical-nx"),
                dev.Device("nx", "duplicate-nx._adb-tls", "physical-nx"),
                dev.Device("rog", "rog._adb-tls", "physical-rog")]

    def test_multiple_transports_are_one_physical_phone_and_not_two_installs(self):
        chosen = dev.choose_devices(self.devices(), ["nx", "rog"])
        self.assertEqual([device.transport for device in chosen], ["ordinary-nx", "rog._adb-tls"])

    def test_two_physical_phones_with_the_same_alias_are_not_guessed(self):
        with self.assertRaisesRegex(dev.Failure, "ambiguous"):
            dev.choose_devices(self.devices() + [dev.Device("nx", "another-nx", "different-physical-nx")], ["nx"])
        with self.assertRaisesRegex(dev.Failure, "no authorized"):
            dev.choose_devices([], ["rog"])

    @patch("dev.build_environment", return_value={"CARGO_TARGET_DIR": "target/android"})
    @patch("dev.subprocess.run", return_value=subprocess.CompletedProcess([], 0))
    def test_container_native_build_never_moves_debug_signing_into_the_container(self, run, environment):
        dev.build("android", "test-builder")
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands[0][:4], ["distrobox", "enter", "--name", "test-builder"])
        self.assertEqual(commands[0][-3:], ["cargo", "xtask", "android-dev-native"])
        self.assertEqual(commands[1], ["cargo", "xtask", "android-dev-apk"])

    @patch("dev.build_environment", return_value={})
    @patch("dev.subprocess.run", return_value=subprocess.CompletedProcess([], 7))
    def test_failed_native_build_never_packages_stale_libraries(self, run, environment):
        with self.assertRaises(dev.Failure):
            dev.build("android", "test-builder")
        self.assertEqual(run.call_count, 1)

    @patch("dev.build_environment", return_value={"PEERSUS_PRIVATE_STORE_BUILD": "1"})
    @patch("dev.subprocess.run", return_value=subprocess.CompletedProcess([], 0))
    def test_normal_build_cannot_inherit_private_catalog_mode(self, run, environment):
        dev.build("desktop", "test-builder")
        self.assertEqual(run.call_args.kwargs["env"]["PEERSUS_PRIVATE_STORE_BUILD"], "0")

    @patch("dev.build_environment", return_value={})
    @patch("dev.subprocess.run", return_value=subprocess.CompletedProcess([], 0))
    def test_private_android_mode_reaches_native_and_host_packaging_together(self, run, environment):
        dev.build("android", "test-builder", private_store=True)
        self.assertEqual(run.call_count, 2)
        for call in run.call_args_list:
            self.assertEqual(call.kwargs["env"]["PEERSUS_PRIVATE_STORE_BUILD"], "1")

    def test_stale_apk_is_refused_even_when_the_build_returns_success(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / "candidate.apk"
            apk.write_bytes(b"old APK")
            os.utime(apk, ns=(1, 1))
            with patch("dev.DEFAULT_APK", apk), patch("dev.build"):
                with self.assertRaisesRegex(dev.Failure, "fresh APK"):
                    dev.build_apk(0, Path(directory))

    def test_phone_build_version_respects_highest_installed_version_and_explicit_number(self):
        with patch("dev.DEFAULT_APK") as apk, patch("dev.build") as build, \
             patch("dev.DEFAULT_APK.name", "app-community-debug.apk"), \
             patch("dev.shutil.copyfile"), patch.dict(os.environ, {"PEERSUS_BUILD_NUMBER": "17"}):
            apk.stat.return_value.st_mtime_ns = 2**63 - 1
            result = dev.build_apk(25, Path("snapshot"), container="builder")
        self.assertEqual(result, Path("snapshot/app-community-debug.apk"))
        self.assertEqual(build.call_args.kwargs["build_number"], 24)
        self.assertEqual(build.call_args.kwargs["container"], "builder")

    def test_cargo_artifact_older_than_its_input_is_refused(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            source, output = root / "source file.rs", root / "application"
            source.touch()
            output.touch()
            output.with_suffix(".d").write_text("application: source\\ file.rs\n")
            os.utime(output, ns=(1, 1))
            with self.assertRaisesRegex(RuntimeError, "older than a build input"):
                dev.extensions.verify_cargo_output(output, root)
            os.utime(output, ns=(source.stat().st_mtime_ns, source.stat().st_mtime_ns))
            dev.extensions.verify_cargo_output(output, root)

    def test_desktop_staging_uses_the_release_build_environment_and_a_new_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            target = root / "target"
            release = target / "example-linux/release"
            release.mkdir(parents=True)
            (release / "peersus-native-host").touch()
            environment = {"CARGO_TARGET_DIR": str(target), "CARGO_BUILD_TARGET": "example-linux"}
            with patch("dev.build_environment", return_value=environment), patch("dev.xtask") as build, \
                 patch.object(dev.extensions, "verify_cargo_output"), \
                 patch("dev.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as stager:
                stage = dev.build_desktop(root, "builder")
            build.assert_called_once_with(["build"], environment, "builder", "Desktop release build")
            self.assertEqual(stager.call_args.args[0][-2:], ["--out", str(stage)])
            self.assertEqual(stager.call_args.kwargs["env"], environment)
            self.assertEqual(stage, root / "linux")

    def test_container_apk_build_verifies_native_output_before_host_packaging(self):
        with patch("dev.build_environment", return_value={"CARGO_TARGET_DIR": "target/android"}), \
             patch("dev.xtask") as xtask, patch("dev.verify_android_native", side_effect=dev.Failure("stale native build")):
            with self.assertRaisesRegex(dev.Failure, "stale native build"):
                dev.build("android", container="builder", build_number=8)
        self.assertEqual([call.args[0] for call in xtask.call_args_list], [["android-dev-native"]])

    def test_signing_mismatch_on_second_phone_prevents_any_install(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / "candidate.apk"
            apk.write_bytes(b"new APK")
            previous = Path(directory) / "previous.apk"
            previous.write_bytes(b"previous APK")
            commands = []
            def capture(command):
                commands.append(command)
                return "package: name='ro.peersus' versionCode='1'\napplication-debuggable"
            with patch("dev.android_tool", side_effect=lambda name: name), patch("dev.capture", side_effect=capture), \
                 patch("dev.discover", return_value=self.devices()), patch("dev.installed_apk", return_value=previous), \
                 patch("dev.build_apk", return_value=apk), \
                 patch("dev.signer", side_effect=[frozenset({"same"}), frozenset({"same"}), frozenset({"different"})]):
                with self.assertRaisesRegex(dev.Failure, "certificate differs"):
                    dev.deploy_selected(["nx", "rog"])
            self.assertFalse(any("install" in command or "uninstall" in command for command in commands))

    def run_deploy(self, tamper=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        apk = Path(temporary.name) / "candidate.apk"
        apk.write_bytes(b"new APK")
        commands = []
        def capture(command):
            commands.append(command)
            if command[0] == "aapt2":
                return "package: name='ro.peersus' versionCode='1'\napplication-debuggable"
            if "getprop" in command:
                return "physical-nx" if command[2] == "ordinary-nx" else "physical-rog"
            return "Success"
        def installed(device, destination):
            shutil.copyfile(apk, destination)
            if tamper and destination.name.endswith("after.apk"):
                destination.write_bytes(b"wrong installed APK")
            return destination
        with patch("dev.android_tool", side_effect=lambda name: name), patch("dev.capture", side_effect=capture), \
             patch("dev.discover", return_value=self.devices()), patch("dev.installed_apk", side_effect=installed), \
             patch("dev.build_apk", return_value=apk), \
             patch("dev.signer", return_value=frozenset({"same"})), patch("dev.restart_phone"), \
             patch("dev.wait_for_owner"), contextlib.redirect_stdout(io.StringIO()) as said:
            if tamper:
                self.assertEqual(dev.deploy_selected(["nx"], app_only=True), 1)
                self.assertIn("installed APK differs", said.getvalue())
            else:
                self.assertEqual(dev.deploy_selected(["nx", "rog"], app_only=True), 0)
        return commands

    def test_only_selected_deduplicated_transports_receive_replace_install(self):
        commands = self.run_deploy()
        installs = [command for command in commands if "install" in command]
        self.assertEqual([command[2] for command in installs], ["ordinary-nx", "rog._adb-tls"])
        self.assertTrue(all(command[3:5] == ["install", "-r"] for command in installs))
        self.assertFalse(any("uninstall" in command or "clear" in command for command in commands))

    def test_an_uncertain_install_is_not_blindly_retried(self):
        commands = self.run_deploy(tamper=True)
        self.assertEqual(sum("install" in command for command in commands), 1)

    def test_restart_phone_refuses_unconfirmed_launch(self):
        nx = self.devices()[0]
        with patch("dev.capture", side_effect=["", "Status: timeout"]) as capture:
            with self.assertRaisesRegex(dev.Failure, "did not confirm"):
                dev.restart_phone(nx)
        self.assertEqual(capture.call_args_list[0].args[0][-2:], ["force-stop", dev.PACKAGE])
        self.assertEqual(capture.call_args_list[1].args[0][-2:], ["-p", dev.PACKAGE])


class Desktops(unittest.TestCase):
    def stage(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        stage = Path(temporary.name)
        (stage / "bin").mkdir()
        for name in (*dev.NEEDED_PROGRAMS, "peersus-wasm-host", "peersus-wasm-canvas-host"):
            (stage / "bin" / name).write_bytes(name.encode())
        return stage

    def test_an_incomplete_stage_is_refused_before_any_desktop_is_reached(self):
        stage = self.stage()
        (stage / "bin/peersus-ui").unlink()
        with patch("dev.ssh") as ssh, self.assertRaisesRegex(dev.Failure, "incomplete"):
            dev.preflight_desktops(stage, ["msi", "hp"])
        ssh.assert_not_called()

    def test_every_remote_desktop_is_reached_before_anything_is_installed(self):
        stage = self.stage()
        with patch("dev.ssh", side_effect=dev.Failure("HP: remote operation failed; nothing was removed")) as ssh:
            with self.assertRaisesRegex(dev.Failure, "HP"):
                dev.preflight_desktops(stage, ["msi", "hp"])
        self.assertEqual(ssh.call_args.args, ("hp", "getconf GNU_LIBC_VERSION"))
        with self.assertRaisesRegex(dev.Failure, "only once"):
            dev.preflight_desktops(stage, ["hp", "hp"])

    def test_missing_ssh_desktop_is_recorded_without_blocking_local_preflight(self):
        stage = self.stage()
        offline = set()
        with patch("dev.ssh", side_effect=dev.Offline("unreachable")), \
             patch("dev.os.confstr", return_value="glibc 2.44") as local:
            programs = dev.preflight_desktops(stage, ["hp", "msi"], offline=offline)
        self.assertEqual(offline, {"hp"})
        self.assertIn("peersus-ui", programs)
        local.assert_called_once_with("CS_GNU_LIBC_VERSION")

    def test_a_desktop_with_an_older_c_library_than_the_programs_need_is_refused_before_anything_is_installed(self):
        stage = self.stage()
        # What a newer workstation's linker writes into a program: the versions of the C library it needs.
        (stage / "bin/peersus-ui").write_bytes(b"\x7fELF...GLIBC_2.34\x00GLIBC_2.44\x00GLIBC_2.2.5")
        with patch("dev.ssh", return_value="glibc 2.43\n"), patch("dev.os.confstr", return_value="glibc 2.44"):
            with self.assertRaisesRegex(dev.Failure, "HP: the staged programs need glibc 2.44 and it has 2.43"):
                dev.preflight_desktops(stage, ["msi", "hp"])
        # A desktop as new as the programs need passes.
        with patch("dev.ssh", return_value="glibc 2.44\n"), patch("dev.os.confstr", return_value="glibc 2.44"):
            self.assertIn("peersus-ui", dev.preflight_desktops(stage, ["msi", "hp"]))

    def test_the_version_a_c_library_says_is_read_or_left_unknown(self):
        self.assertEqual(dev.glibc_version("glibc 2.43\n"), (2, 43))
        self.assertIsNone(dev.glibc_version(""))

    def run_desktops(self, installed):
        stage = self.stage()
        programs = dev.staged_programs(stage)
        commands, remote = [], []
        with patch("dev.capture", side_effect=lambda command: commands.append(command) or ""), \
             patch("dev.ssh", side_effect=lambda host, script, stdin=None: remote.append((host, script, stdin)) or ""), \
             patch("dev.installed_programs", side_effect=installed), contextlib.redirect_stdout(io.StringIO()) as out:
            dev.deploy_desktops(stage, ["msi", "hp"], programs)
        return stage, programs, commands, remote, out.getvalue()

    def test_both_desktops_get_the_staged_programs_and_each_is_verified(self):
        stage, programs, commands, remote, said = self.run_desktops(lambda host, names: dict(programs_of(names)))
        # This workstation: the installer, run here on the staged tree.
        self.assertEqual(commands[0][1:], [str(dev.ROOT / "scripts/install-linux-user.py"), str(stage.resolve())])
        # HP: the tree copied, then the same installer run there, read from standard input; nothing removed on either.
        self.assertEqual(commands[1][:3], ["rsync", "-a", "--delete"])
        self.assertTrue(commands[1][-1].startswith("hp:"))
        self.assertEqual(remote[0][:2], ("hp", f"python3 - {dev.REMOTE_STAGE}"))
        self.assertEqual(remote[0][2], (dev.ROOT / "scripts/install-linux-user.py").read_text())
        self.assertFalse(any("rm " in script or "uninstall" in script for _, script, _ in remote))
        self.assertIn("MSI: staged desktop build installed", said)
        self.assertIn("HP: staged desktop build installed", said)

    def test_a_desktop_whose_installed_programs_differ_stops_the_deploy_without_a_retry(self):
        with self.assertRaisesRegex(dev.Failure, "MSI: installation returned, but the installed programs differ"):
            self.run_desktops(lambda host, names: {name: b"other" for name in names})

    def test_desktop_restart_refuses_to_stop_when_its_graphical_session_is_unavailable(self):
        for alias, passes_local_display in (("msi", True), ("hp", False)):
            with self.subTest(alias=alias):
                calls = []

                def run(command, *, check, **_kwargs):
                    calls.append(command)
                    if command[:3] == ("systemctl", "--user", "is-active"):
                        self.assertFalse(check)
                        return subprocess.CompletedProcess(command, 3)
                    if command[0] == "systemd-run" and "--wait" in command:
                        raise subprocess.CalledProcessError(1, command)
                    self.fail(f"restart tried to modify the session before its preflight: {command[0]}")

                def execute(source, mode):
                    with patch.object(sys, "argv", ["desktop-restart", mode]):
                        exec(compile(source, "desktop-restart", "exec"), {})

                def local(command):
                    self.assertEqual(command[:2], [sys.executable, "-c"])
                    return execute(*command[2:])

                def remote(host, command, *, stdin):
                    self.assertEqual(host, dev.DESKTOPS["hp"])
                    program, input_mode, mode = shlex.split(command)
                    self.assertEqual((program, input_mode), ("python3", "-"))
                    return execute(stdin, mode)

                with patch("dev.capture", side_effect=local), patch("dev.ssh", side_effect=remote), \
                     patch("subprocess.run", side_effect=run), patch.object(Path, "iterdir", return_value=iter(())), \
                     patch("socket.socket") as socket, patch.dict(os.environ, {"DISPLAY": ":42"}, clear=True):
                    socket.return_value.__enter__.return_value.connect.side_effect = ConnectionRefusedError
                    with self.assertRaises(subprocess.CalledProcessError):
                        dev.restart_desktop(alias)

                self.assertEqual(len(calls), 2)
                self.assertEqual("--setenv=DISPLAY=:42" in calls[-1], passes_local_display)


class DeploySelected(unittest.TestCase):
    def run_selected(self, *, app_only=False, no_phone=False, bad_desktop=False, fail_restart=False, fail_extension=False):
        events = []
        nx = dev.Device("nx", "authorized", "physical")

        def record(event, result=None):
            def call(*args, **kwargs):
                events.append((event, args, kwargs))
                return result
            return call

        def restart(device):
            events.append(("restart phone", (device,), {}))
            if fail_restart:
                raise dev.Failure("launch failed")

        def prepare(inventories, root, *, launcher):
            events.append(("build extensions", (inventories, root, launcher), {}))
            return [{name: root / name for name in names} for _, _, names in inventories], ({1} if fail_extension else set())

        with contextlib.ExitStack() as mocks, contextlib.redirect_stdout(io.StringIO()) as output:
            for name, action in {
                "discover": record("discover", [] if no_phone else [nx]),
                "ssh": record("check desktop reachability"),
                "android_tool": lambda name: name,
                "installed_apk": record("check installed APK"),
                "build_apk": record("build APK", Path("new.apk")),
                "build_desktop": record("build and stage desktop", Path("stage")),
                "preflight_phones": record("phone preflight", b"apk digest"),
                "preflight_desktops": (dev.Failure("incompatible desktop") if bad_desktop
                                       else record("desktop preflight", {"peersus": b"program digest"})),
                "install_phone": record("install phone"),
                "deploy_desktops": record("install desktop"),
                "restart_phone": restart,
                "restart_desktop": record("restart desktop"),
                "wait_for_owner": lambda alias, phone: record("control socket", alias)(alias, phone),
                "extension_inventory": lambda alias, *_: record(
                    "read extension inventory", ("android" if alias == "nx" else "linux", {alias + "-guest"}))(),
            }.items():
                mocks.enter_context(patch.object(dev, name, side_effect=action))
            mocks.enter_context(patch.object(dev.extensions, "prepare_packages", side_effect=prepare))
            mocks.enter_context(patch.object(dev.extensions, "deploy_prepared", side_effect=record("update extensions", 0)))
            if bad_desktop:
                with self.assertRaisesRegex(dev.Failure, "incompatible desktop"):
                    dev.deploy_selected(["hp", "nx"])
                result = None
            else:
                result = dev.deploy_selected(["hp", "nx"], container="builder", app_only=app_only,
                                             accept_current_agreement=True, accept_outside_store_risk=True)
        return result, events, output.getvalue()

    def test_both_preflights_precede_app_writes_and_extensions_follow_verified_restarts(self):
        result, events, _ = self.run_selected()
        self.assertEqual(result, 0)
        names = [name for name, _, _ in events]
        install = names.index("install desktop")
        self.assertLess(names.index("build and stage desktop"), names.index("desktop preflight"))
        self.assertLess(names.index("build APK"), names.index("phone preflight"))
        for step in ("desktop preflight", "phone preflight", "build extensions"):
            self.assertLess(names.index(step), install)
        self.assertEqual(names[install:], ["install desktop", "restart desktop", "control socket", "update extensions",
                                          "install phone", "restart phone", "control socket", "update extensions"])
        updates = [(args, flags) for event, args, flags in events if event == "update extensions"]
        self.assertEqual([(alias, owner, set(archives)) for (alias, owner, archives), _ in updates],
                         [("hp", "hp", {"hp-guest"}), ("nx", "nx", {"nx-guest"})])
        self.assertTrue(all(flags["accept_current_agreement"] and flags["accept_outside_store_risk"] for _, flags in updates))
        _, (_, _, launcher), _ = next(event for event in events if event[0] == "build extensions")
        self.assertEqual(launcher[:4], ["distrobox", "enter", "--name", "builder"])

    def test_an_offline_phone_does_not_block_an_online_desktop(self):
        result, events, said = self.run_selected(no_phone=True)
        self.assertNotEqual(result, 0)
        self.assertIn("NX: offline; not updated", said)
        self.assertNotIn("install phone", [name for name, _, _ in events])
        self.assertNotIn("build APK", [name for name, _, _ in events])
        self.assertEqual(events[-1][1][:2], ("hp", "hp"))

    def test_incompatible_online_desktop_blocks_all_app_writes(self):
        _, events, _ = self.run_selected(bad_desktop=True)
        self.assertFalse(any(name.startswith(("install", "restart")) for name, _, _ in events))

    def test_failed_restart_reports_failure_and_does_not_send_extensions_to_that_device(self):
        result, events, said = self.run_selected(fail_restart=True)
        self.assertNotEqual(result, 0)
        self.assertIn("NX: FAILED (restart)", said)
        self.assertEqual([args[0] for event, args, _ in events if event == "update extensions"], ["hp"])

    def test_extension_build_failure_blocks_app_install_only_on_the_affected_target(self):
        result, events, said = self.run_selected(fail_extension=True)
        self.assertNotEqual(result, 0)
        self.assertIn("NX: application not changed because an extension build failed", said)
        self.assertNotIn("install phone", [name for name, _, _ in events])
        self.assertEqual(events[-1][1][:2], ("hp", "hp"))

    def test_app_only_still_restarts_but_does_not_update_extensions(self):
        result, events, _ = self.run_selected(app_only=True)
        self.assertEqual(result, 0)
        self.assertEqual([name for name, _, _ in events][-3:], ["install phone", "restart phone", "control socket"])
        self.assertNotIn("build extensions", [name for name, _, _ in events])


class InstalledExtensions(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.archive = Path(scratch.name) / "guest-linux.tar.gz"
        self.manifest = json.dumps({"program": "guest.wasm"}).encode()
        self.module = b"new module"
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, data in [("plugin.json", self.manifest), ("guest.wasm", self.module)]:
                member = tarfile.TarInfo(f"guest/{name}")
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        self.ext = dev.extensions
        digest = patch.object(self.ext, "agreement_digest", return_value="current-package-agreement")
        self.addCleanup(digest.stop)
        digest.start()

    def device(self, *, installed=True, unchanged=False, disabled=False, agreement_changed=False):
        ext, manifest, module = self.ext, self.manifest, self.module
        owned = {"owner": "guest", "name": "read"}
        offered = {"owner": "guest", "name": "receive"}
        original = {
            "id": "guest", "standing": "disabled" if disabled else "ready",
            "status": "stopped" if disabled else "running", "agreed": True,
            "manifest": {"capabilities": {"read": "Read", "receive": "Receive"},
                         "agreements": {"pages": ["old" if agreement_changed else "current"]}},
            "grantable": {"outright": [owned], "per_device": [offered]},
        }
        replacement = copy.deepcopy(original)
        replacement["manifest"]["agreements"] = {"pages": ["current"]}

        class Device:
            platform = "linux"
            name = "desktop"

            def __init__(self):
                self.commands = []
                self.updated = unchanged
                self.state = {"plugins": [original] if installed else [], "staged": [],
                              "settings": {"outside_store_risk_accepted": True},
                              "grants": {"guest": [owned]},
                              "peers": [{"id": "peer", "grants": {"guest": [offered]}}]}

            def snapshot(self):
                return copy.deepcopy(self.state)

            def stage(self, _archive):
                return "/temporary/archive"

            def unstage(self, _path):
                pass

            def installed_modules(self, _plugin):
                return {"guest.wasm": ext.sha(module if self.updated else b"old module")}

            def manifest_digest(self, _plugin):
                return ext.sha(manifest if self.updated else b"old manifest")

            def request(self, request):
                command = request["command"]
                operation = command["command"]
                self.commands.append(command)
                if operation == "stage-install":
                    self.state["staged"] = [replacement]
                elif operation == "confirm-install":
                    self.state["plugins"] = [replacement]
                    self.state["grants"] = {}
                    self.state["peers"][0]["grants"] = {}
                    self.state["staged"] = []
                    self.updated = True
                elif operation == "cancel-install":
                    self.state["staged"] = []
                elif operation == "set-grants":
                    grants = self.state["peers"][0]["grants"] if command.get("peer") else self.state["grants"]
                    grants["guest"] = command["grants"]
                else:
                    raise AssertionError(operation)
                return {"result": {"Ok": None}}

        return Device()

    def test_equal_package_is_not_reinstalled_even_when_disabled(self):
        device = self.device(unchanged=True, disabled=True)
        self.assertTrue(self.ext.install(device, self.archive, "guest", None, False, installed_only=True)
                        .startswith("up to date"))
        self.assertEqual(device.commands, [])

    def test_update_keeps_exact_own_and_per_device_grants_and_disabled_state(self):
        device = self.device(disabled=True)
        before = self.ext.grants_of(device.snapshot(), "guest")
        result = self.ext.install(device, self.archive, "guest", None, False, installed_only=True)
        self.assertIn("updated (disabled; grants unchanged", result)
        self.assertEqual(self.ext.grants_of(device.snapshot(), "guest"), before)
        self.assertEqual(device.commands[1]["agreement"], "current-package-agreement")

    def test_changed_agreement_is_not_accepted_without_explicit_opt_in(self):
        device = self.device(agreement_changed=True)
        with self.assertRaisesRegex(RuntimeError, "licence/notice needs acceptance"):
            self.ext.install(device, self.archive, "guest", None, False, installed_only=True)
        self.assertEqual([c["command"] for c in device.commands], ["stage-install", "cancel-install"])
        self.assertFalse(device.updated)

    def test_module_checksum_is_bound_to_its_filename(self):
        device = self.device(unchanged=True)
        expected = device.installed_modules("guest")
        device.updated = False
        with patch.object(device, "manifest_digest", return_value=self.ext.sha(self.manifest)), \
             patch.object(device, "installed_modules", side_effect=[{"other.wasm": expected["guest.wasm"]}, expected]):
            result = self.ext.install(device, self.archive, "guest", None, False, installed_only=True)
        self.assertTrue(result.startswith("updated"))
        self.assertEqual(device.commands[1]["command"], "confirm-install")

    def test_outside_store_warning_is_not_implicitly_accepted_by_agreement_flag(self):
        device = self.device(agreement_changed=True)
        device.state["settings"]["outside_store_risk_accepted"] = False
        with self.assertRaisesRegex(RuntimeError, "outside-store warning needs acknowledgment"):
            self.ext.install(device, self.archive, "guest", None, False, installed_only=True,
                             accept_current_agreement=True)
        self.assertEqual(device.commands, [])

    def test_changed_agreement_opt_in_identifies_package_digest_and_target(self):
        device = self.device(agreement_changed=True)
        with contextlib.redirect_stdout(io.StringIO()) as said:
            self.ext.install(device, self.archive, "guest", None, False, installed_only=True,
                             accept_current_agreement=True, label="HP: guest")
        self.assertIn("HP: guest: accepted licence/notice current-package-agreement", said.getvalue())

    def test_removed_extension_is_not_reintroduced(self):
        device = self.device(installed=False)
        with self.assertRaisesRegex(RuntimeError, "no longer installed"):
            self.ext.install(device, self.archive, "guest", None, False, installed_only=True)
        self.assertEqual(device.commands, [])

    def test_shared_extension_is_built_once_and_packaged_once_per_platform(self):
        inventories = [("msi", "linux", {"guest"}), ("hp", "linux", {"guest"}), ("nx", "android", {"guest"})]
        with patch.object(self.ext, "plugin_source", return_value=(Path("guest"), "guest")), \
             patch.object(self.ext, "build") as build, \
             patch.object(self.ext, "package", side_effect=lambda source, name, platform, out: out / "archive") as package:
            archives, failed = self.ext.prepare_packages(inventories, Path("output"), launcher=["builder"])
        self.assertEqual(failed, set())
        build.assert_called_once_with(Path("guest"), ["builder"])
        self.assertEqual(package.call_count, 2)
        self.assertEqual(archives[0], archives[1])
        self.assertNotEqual(archives[1], archives[2])

    def test_inventory_packages_only_installed_extensions_and_reports_unknown_source(self):
        desktop = self.device(unchanged=True)
        desktop.state["plugins"].append({"id": "external", "standing": "ready"})
        phone = self.device(installed=False)
        phone.platform = "android"
        with patch.object(self.ext, "plugin_source", side_effect=[RuntimeError("no matching local source"),
                                                                 (Path("guest"), "guest")]), \
             patch.object(self.ext, "package", return_value=self.archive) as package, \
             patch.object(self.ext, "build") as build, contextlib.redirect_stdout(io.StringIO()) as said:
            failures = self.ext.deploy_packages([("msi", desktop), ("nx", phone)], no_build=True)
        self.assertEqual(failures, 1)
        self.assertIn("MSI: external: FAILED:", said.getvalue())
        self.assertIn("MSI: guest: up to date", said.getvalue())
        self.assertIn("NX: no extensions installed", said.getvalue())
        self.assertEqual(package.call_args.args[:3], (Path("guest"), "guest", "linux"))
        self.assertEqual(package.call_count, 1)
        build.assert_not_called()
        self.assertEqual(phone.commands, [])


def programs_of(names):
    import hashlib
    return {name: hashlib.sha256(name.encode()).digest() for name in names}


if __name__ == "__main__":
    unittest.main()
