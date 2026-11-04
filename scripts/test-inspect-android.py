#!/usr/bin/env python3
"""Security regressions for the artifact boundary, without executing artifact code."""
import importlib.util
import io
from pathlib import Path
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("inspector", Path(__file__).with_name("inspect-android.py"))
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)


class BoundaryTest(unittest.TestCase):
    def archive(self, extra=None, manifest=b"manifest", dex=b"dex\n"):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            archive.writestr("base/manifest/AndroidManifest.xml", manifest)
            archive.writestr("base/dex/classes.dex", dex)
            for name, data in (extra or {}).items():
                archive.writestr(name, data)
        return zipfile.ZipFile(stream)

    def test_play_refuses_loader_class_or_manifest_even_if_the_other_layers_are_clean(self):
        native = b"Ldev/peersus/app/NativePluginService;"
        cases = [
            ({"base/lib/arm64-v8a/" + inspector.LOADER: b"ELF"}, b"manifest", b"dex\n"),
            ({}, b"manifest", native),
            ({}, b"dev.peersus.app.NativeSlots$Slot0", b"dex\n"),
            ({}, "dev.peersus.app.NativeSlots$Slot0".encode("utf-16le"), b"dex\n"),
        ]
        for extra, manifest, dex in cases:
            with self.subTest(extra=list(extra), manifest=manifest), self.archive(extra, manifest, dex) as archive:
                with self.assertRaises(ValueError):
                    inspector.boundary(archive, "play", 2)

    def test_play_refuses_elevated_clipboard_manifest_and_dex_independently(self):
        for manifest, dex in [
            (b"rikka.shizuku.ShizukuProvider", b"dex\n"),
            ("moe.shizuku.manager.permission.API_V23".encode("utf-16le"), b"dex\n"),
            (b"manifest", b"Ldev/peersus/app/ClipboardUserService;"),
            (b"manifest", b"Lrikka/shizuku/Shizuku;"),
        ]:
            with self.subTest(manifest=manifest, dex=dex), self.archive(manifest=manifest, dex=dex) as archive:
                with self.assertRaisesRegex(ValueError, "elevated clipboard"):
                    inspector.boundary(archive, "play", 2)

    def test_community_requires_every_service_in_manifest_and_dex(self):
        extra = {"base/lib/arm64-v8a/" + inspector.LOADER: b"ELF"}
        dex = b"Ldev/peersus/app/NativePluginService;Ldev/peersus/app/NativeSlots$Slot0;Ldev/peersus/app/NativeSlots$Slot1;"
        manifest = b"dev.peersus.app.NativeSlots$Slot0 dev.peersus.app.NativeSlots$Slot1"
        with self.archive(extra, manifest, dex) as archive:
            self.assertEqual(len(inspector.boundary(archive, "community", 2)), 1)
        for changed_manifest, changed_dex in [(manifest.replace(b"Slot1", b"Slot10"), dex), (manifest, dex.replace(b"Slot1;", b"Slot10;"))]:
            with self.archive(extra, changed_manifest, changed_dex) as archive:
                with self.assertRaises(ValueError):
                    inspector.boundary(archive, "community", 2)

    def test_missing_dex_or_manifest_cannot_be_claimed_as_clean(self):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            archive.writestr("base/manifest/AndroidManifest.xml", b"manifest")
        with zipfile.ZipFile(stream) as archive:
            with self.assertRaises(ValueError):
                inspector.boundary(archive, "play", 2)


if __name__ == "__main__":
    unittest.main()
