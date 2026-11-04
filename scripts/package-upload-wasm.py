#!/usr/bin/env python3
"""Build the upload WASM package for Android APKs and manual development installs.

This does not sign, publish, install, access credentials or perform an upload.
Store submission metadata and maintainer signing remain separate operations.
"""
import argparse
import gzip
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/peersus-upload/wasm"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-only", action="store_true",
                        help="Build the APK payload without running the separate quality gate")
    parser.add_argument("--output", type=Path,
                        help="Write a new platform archive here instead of the Android build output")
    parser.add_argument("--target-dir", type=Path,
                        help="Keep all Cargo products in this disposable SDK build directory")
    args = parser.parse_args()
    if args.output is not None and (args.output.exists() or args.output.is_symlink()):
        raise ValueError("The submission output archive must not already exist")
    if args.target_dir is not None:
        os.environ["CARGO_TARGET_DIR"] = str(args.target_dir.resolve())
    manifest_path = SOURCE / "Cargo.toml"
    if not args.build_only:
        for arguments in [
            ["fmt", "--manifest-path", str(manifest_path), "--", "--check"],
            ["clippy", "--manifest-path", str(manifest_path), "--locked", "--all-targets", "--", "-D", "warnings"],
            ["test", "--manifest-path", str(manifest_path), "--locked"],
        ]:
            subprocess.run(["cargo", *arguments], cwd=ROOT, check=True)
        subprocess.run([
            "cargo", "audit", "--file", str(SOURCE / "Cargo.lock"), "--deny", "warnings",
        ], cwd=ROOT, check=True)
    subprocess.run([
        "cargo", "build", "--manifest-path", str(manifest_path), "--locked",
        "--release", "--target", "wasm32-unknown-unknown",
    ], cwd=ROOT, check=True)
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--manifest-path", str(manifest_path),
        "--locked", "--offline", "--format-version", "1",
    ], cwd=ROOT))
    target = Path(metadata["target_directory"])
    module = target / "wasm32-unknown-unknown/release/peersus_upload_wasm.wasm"
    binary = module.read_bytes()
    if not binary.startswith(b"\0asm\x01\0\0\0"):
        raise ValueError("The built extension is not a WebAssembly module")
    binary = subprocess.check_output([
        "python3", str(ROOT / "scripts/embed-metadata.py"),
        "wasm", str(SOURCE / "plugin.json"), str(module),
    ], cwd=ROOT)
    if not args.build_only:
        with tempfile.TemporaryDirectory(prefix="peersus-upload-guest-") as temporary:
            packaged_module = Path(temporary) / module.name
            packaged_module.write_bytes(binary)
            environment = os.environ.copy()
            environment["PEERSUS_UPLOAD_WASM"] = str(packaged_module)
            worker_test = "real_upload_guest_uses_host_services_and_returns_the_verified_link"
            discovered = subprocess.check_output([
                "cargo", "test", "-p", "peersus-plugin-runtime", "--test", "process",
                "--locked", worker_test, "--", "--ignored", "--exact", "--list",
            ], cwd=ROOT, env=environment, text=True)
            if f"{worker_test}: test" not in discovered.splitlines():
                raise RuntimeError("The isolated upload worker regression test was not discovered")
            subprocess.run([
                "cargo", "test", "-p", "peersus-plugin-runtime", "--test", "process",
                "--locked", worker_test,
                "--", "--ignored", "--exact",
            ], cwd=ROOT, env=environment, check=True)
        subprocess.run([
            "cargo", "test", "-p", "peersus-ui-android", "--test", "bundled_upload", "--locked",
        ], cwd=ROOT, check=True)
    manifest = json.loads((SOURCE / "plugin.json").read_text())
    files = {
        manifest["program"]: binary,
        "LICENSE": (ROOT / "LICENSE").read_bytes(),
        "NOTICE": (ROOT / "NOTICE").read_bytes(),
    }
    notices = ["Third-party source licenses for the upload WASM development package.", ""]
    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
        if package["source"] is None:
            continue
        root = Path(package["manifest_path"]).resolve().parent
        licenses = sorted({
            path for pattern in ("LICENSE*", "COPYING*", "NOTICE*")
            for path in root.glob(pattern)
            if not path.is_symlink() and path.is_file()
        })
        if not licenses or not package["license"]:
            raise ValueError(f"Missing upstream license attribution for {package['name']}")
        notices.append(f"{package['name']} {package['version']}: {package['license']}")
        for license_path in licenses:
            name = f"licenses/{package['name']}-{package['version']}/{license_path.name}"
            if name in files:
                raise ValueError("Duplicate third-party license archive entry")
            files[name] = license_path.read_bytes()
    files["THIRD_PARTY_NOTICES.txt"] = ("\n".join(notices) + "\n").encode()
    manifest["files"] = sorted(name for name in files if name != manifest["program"])
    files["plugin.json"] = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
    output = args.output if args.output is not None else ROOT / "target/extensions/upload-android.tar.gz"
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=".upload-android-", suffix=".pending", dir=output.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                    for name, data in sorted(files.items()):
                        entry = tarfile.TarInfo(f"upload/{name}")
                        entry.size = len(data)
                        entry.mode = 0o700 if name == manifest["program"] else 0o600
                        entry.mtime = 0
                        archive.addfile(entry, io.BytesIO(data))
        # The archive must satisfy the same data-only check as a submission.
        # Refuse before replacing the last successfully validated package.
        subprocess.run([
            "python3", str(ROOT / "scripts/validate-extension.py"), str(temporary),
        ], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
        if not output.is_file() or temporary.read_bytes() != output.read_bytes():
            os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    print("Built the Android upload development archive; no installation or publication performed.")


if __name__ == "__main__":
    main()
