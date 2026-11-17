#!/usr/bin/env python3
"""Deterministic development-tool regressions; never contact a device or run a build."""
import contextlib
import io
import os
from pathlib import Path
import shutil
import subprocess
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

    def test_signing_mismatch_on_second_phone_prevents_any_install(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / "candidate.apk"
            apk.write_bytes(b"new APK")
            previous = Path(directory) / "previous.apk"
            previous.write_bytes(b"previous APK")
            commands = []
            def capture(command):
                commands.append(command)
                return "package: name='ro.peersus'\napplication-debuggable"
            with patch("dev.android_tool", side_effect=lambda name: name), patch("dev.capture", side_effect=capture), \
                 patch("dev.discover", return_value=self.devices()), patch("dev.installed_apk", return_value=previous), \
                 patch("dev.signer", side_effect=[frozenset({"same"}), frozenset({"same"}), frozenset({"different"})]):
                with self.assertRaisesRegex(dev.Failure, "certificate differs"):
                    dev.deploy(apk, ["nx", "rog"])
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
                return "package: name='ro.peersus'\napplication-debuggable"
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
             patch("dev.signer", return_value=frozenset({"same"})), contextlib.redirect_stdout(io.StringIO()):
            if tamper:
                with self.assertRaisesRegex(dev.Failure, "installed APK differs"):
                    dev.deploy(apk, ["nx"])
            else:
                dev.deploy(apk, ["nx", "rog"])
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


class Desktops(unittest.TestCase):
    def stage(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        stage = Path(temporary.name)
        (stage / "bin").mkdir()
        for name in ("peersus", "peersus-ui", "peersus-wasm-host"):
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


def programs_of(names):
    import hashlib
    return {name: hashlib.sha256(name.encode()).digest() for name in names}


if __name__ == "__main__":
    unittest.main()
