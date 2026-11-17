#!/usr/bin/env python3
"""Reject misaligned release native artifacts; emit compressed/uncompressed sizes as JSON.
AAB ZIP alignment is not APK alignment. ELF is inspected in both; APK additionally uses zipalign.
This does not validate Play signing, delivery-generated APKs or runtime behavior.
"""
import argparse
import json
import re
from pathlib import Path
import shutil
import struct
import subprocess
import zipfile
import xml.etree.ElementTree as ET

FLAVORS = {"play", "community"}
LOADER = "libpeersus_native_android.so"

def distribution(artifact):
    found = [flavor for flavor in FLAVORS if re.search(r"(?:^|-)" + flavor + r"(?:-|\.)", artifact.name)]
    return found[0] if len(found) == 1 else None

def boundary(archive, flavor, slot_count):
    names = archive.namelist()
    loaders = [name for name in names if name.endswith("/" + LOADER)]
    dex = [archive.read(name) for name in names if re.search(r"(?:^|/)classes[0-9]*\.dex$", name)]
    if not dex:
        raise ValueError("No packaged DEX code to inspect")
    classes = b"Ldev/peersus/app/NativePluginService;"
    slots = {int(value) for data in dex for value in re.findall(rb"Ldev/peersus/app/NativeSlots\$Slot([0-9]+);", data)}
    manifests = [archive.read(name) for name in names if name == "AndroidManifest.xml" or name.endswith("/manifest/AndroidManifest.xml")]
    if not manifests:
        raise ValueError("No packaged manifest to inspect")
    native_manifest = any(any(marker in data for marker in (b"NativeSlots", b"NativePluginService", "NativeSlots".encode("utf-16le"), "NativePluginService".encode("utf-16le"))) for data in manifests)
    if flavor == "play":
        if loaders or slots or native_manifest or any(classes in data for data in dex):
            raise ValueError("Play artifact contains an external-native loader or service")
        privileged = (b"rikka.shizuku", b"moe.shizuku", b"ClipboardUserService", b"Lrikka/shizuku/", b"Lmoe/shizuku/")
        if any(marker in data or marker.decode().encode("utf-16le") in data
               for data in manifests + dex for marker in privileged):
            raise ValueError("Play artifact contains an elevated clipboard provider or API")
    else:
        if not loaders or slots != set(range(slot_count)) or not native_manifest or not any(classes in data for data in dex):
            raise ValueError("Community artifact is missing its complete native worker boundary")
        # AAB manifests are protobuf: component names remain exact UTF-8 strings.
        for slot in range(slot_count):
            marker = ("dev.peersus.app.NativeSlots$Slot" + str(slot)).encode()
            if not any(marker in data or marker.decode().encode("utf-16le") in data for data in manifests):
                raise ValueError("Community manifest is missing a native service slot")
        for name in names:
            if name.endswith("/manifest/AndroidManifest.xml"):
                declared = {int(value) for value in re.findall(rb"dev\.peersus\.app\.NativeSlots\$Slot([0-9]+)", archive.read(name))}
                if declared != set(range(slot_count)):
                    raise ValueError("Community bundle manifest has an incomplete native service slot set")
    return loaders

def elf(data, name):
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError(f"unsupported native ELF format: {name}")
    phoff = struct.unpack_from("<Q", data, 32)[0]
    entry, count = struct.unpack_from("<HH", data, 54)
    loads = 0
    for index in range(count):
        values = struct.unpack_from("<IIQQQQQQ", data, phoff + index * entry)
        kind, _, offset, address, _, _, _, alignment = values
        if kind == 1:
            loads += 1
            if alignment < 16384 or (address - offset) % 16384:
                raise ValueError(f"PT_LOAD is not 16 KiB compatible: {name}")
    if not loads:
        raise ValueError(f"no load segments: {name}")

def inspect(root, report):
    artifacts = sorted(path for suffix in ("*.apk", "*.aab") for path in root.rglob(suffix)
                       if "release" in path.name.lower() and distribution(path))
    declaration = (Path(__file__).resolve().parents[1] / "crates/peersus-app/src/discovery.rs").read_text()
    slot_count = int(re.search(r"pub const MAX_PLUGINS: usize = ([0-9]+);", declaration).group(1))
    if not artifacts:
        raise ValueError("no release APK/AAB artifacts found")
    zipalign = shutil.which("zipalign")
    if not zipalign:
        import os
        sdk = os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME")
        candidates = sorted((Path(sdk) / "build-tools").glob("*/zipalign")) if sdk else []
        zipalign = str(candidates[-1]) if candidates else None
    if not zipalign:
        raise ValueError("zipalign missing: add Android SDK build-tools to PATH")
    analyzer = shutil.which("apkanalyzer")
    if not analyzer:
        import os
        sdk = os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME")
        candidate = Path(sdk) / "cmdline-tools/latest/bin/apkanalyzer" if sdk else None
        analyzer = str(candidate) if candidate and candidate.is_file() else None
    if not analyzer:
        raise ValueError("apkanalyzer missing: required to verify release JNI callbacks survive R8")
    output = []
    kinds = set()
    abis = set()
    coverage = {flavor: {"kinds": set(), "abis": set()} for flavor in FLAVORS}
    for artifact in artifacts:
        kinds.add(artifact.suffix)
        flavor = distribution(artifact)
        coverage[flavor]["kinds"].add(artifact.suffix)
        libraries = []
        artifact_abis = set()
        with zipfile.ZipFile(artifact) as archive:
            loaders = boundary(archive, flavor, slot_count)
            for info in archive.infolist():
                if info.filename.endswith(".so"):
                    elf(archive.read(info), info.filename)
                    libraries.append({"path": info.filename, "compressed": info.compress_size, "uncompressed": info.file_size})
                    abis.add(info.filename.split("/")[-2])
                    artifact_abis.add(info.filename.split("/")[-2])
                    coverage[flavor]["abis"].add(info.filename.split("/")[-2])
            if not libraries:
                raise ValueError(f"no native libraries: {artifact}")
            for abi in artifact_abis:
                expected = {"libpeersus_android.so", "libpeersus_plugin_android.so"}
                if flavor == "community": expected.add(LOADER)
                present = {item["path"].split("/")[-1] for item in libraries if item["path"].split("/")[-2] == abi}
                if not expected.issubset(present):
                    raise ValueError("Distribution is missing a required application/runtime library")
        if artifact.suffix == ".aab" and artifact_abis != {"arm64-v8a", "x86_64"}:
            raise ValueError(f"AAB does not contain exactly both supported ABIs: {artifact}")
        if artifact.suffix == ".apk" and len(artifact_abis) != 1:
            raise ValueError(f"release APK is not ABI-split: {artifact}")
        if artifact.suffix == ".apk":
            subprocess.run([zipalign, "-c", "-P", "16", "-v", "4", str(artifact)], check=True)
            manifest = ET.fromstring(subprocess.check_output([analyzer, "manifest", "print", str(artifact)], text=True))
            android = "{http://schemas.android.com/apk/res/android}"
            native_services = [item for item in manifest.iter("service") if "NativeSlots$Slot" in item.get(android + "name", "")]
            if len(native_services) != (slot_count if flavor == "community" else 0):
                raise ValueError("Unexpected native service count in APK manifest")
            if any(item.get(android + "exported") != "false" or not re.fullmatch(r":native_[0-9]+", item.get(android + "process", "")) for item in native_services):
                raise ValueError("Native service is exported or shares an inappropriate process")
            providers = [item for item in manifest.iter("provider") if item.get(android + "name") == "rikka.shizuku.ShizukuProvider"]
            if flavor == "community" and (len(providers) != 1 or providers[0].get(android + "permission") != "android.permission.INTERACT_ACROSS_USERS_FULL"):
                raise ValueError("Community clipboard provider is missing or not protected")
            common = ["setBackgroundState", "setTransferNotifications", "setOfferAppearance", "setAcceptWithoutWindow", "bringWindowForward", "setLanguage", "deliver"]
            callbacks = {
                "MainActivity": common + ["chooseFolder", "chooseSavedFolder", "forgetSavedFolder", "chooseDocuments", "choosePlugin", "openCamera", "closeCamera", "chooseCamera", "notificationsAllowed", "overlayAllowed", "openOverlayPermission", "openNotifications", "openExternalUrl", "backgroundClipboardSupported", "backgroundClipboardStatus", "openBackgroundClipboard", "toBackground", "backComesAsKey"],
                "TransferService": common,
                "PluginBridge": ["nativeAttach", "nativeExited", "nativeTargets", "nativeShare", "launchWasm", "stop", "shareFailed"],
            }
            callbacks["WasmPluginService"] = ["nativeRun", "openData", "isWorkerProcess"]
            callbacks["ModuleBridge"] = ["nativeConnect"]
            if flavor == "community": callbacks["NativePluginService"] = ["nativeRun"]
            for cls, methods in callbacks.items():
                dex = subprocess.check_output([analyzer, "dex", "code", "--class", "dev.peersus.app." + cls, str(artifact)], text=True)
                for method in methods:
                    if not re.search(r"\b" + re.escape(method) + r"\(", dex):
                        raise ValueError(f"R8 removed/renamed JNI callback {cls}.{method}")
        output.append({"artifact": str(artifact), "distribution": flavor, "bytes": artifact.stat().st_size, "external_native_loaders": loaders, "native": libraries})
    if kinds != {".apk", ".aab"} or not {"arm64-v8a", "x86_64"}.issubset(abis):
        raise ValueError("expected release APKs and AAB with both configured ABIs")
    for flavor, found in coverage.items():
        if found["kinds"] != {".apk", ".aab"} or found["abis"] != {"arm64-v8a", "x86_64"}:
            raise ValueError("Expected both APK/AAB formats and ABIs for " + flavor)
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps({"artifacts": output, "validation": "ELF 16KiB; APK zipalign; unsigned; no device/Play validation"}, indent=2) + "\n")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    try:
        inspect(args.root, args.report)
    except (OSError, ValueError, subprocess.CalledProcessError, zipfile.BadZipFile, struct.error) as error:
        raise SystemExit(str(error)) from error
