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
        self.assertEqual(build_layout.environment("android", {}), {"CARGO_TARGET_DIR": "target/android"})

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


if __name__ == "__main__":
    unittest.main()
