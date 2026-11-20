#!/usr/bin/env python3
"""Build the external Android worker only; no credentials, signing, or execution of modules."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ABIS = {"arm64-v8a": "aarch64-linux-android", "x86_64": "x86_64-linux-android"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--abi", choices=ABIS, action="append")
    parser.add_argument("--test-assets", type=Path)
    parser.add_argument("--fixtures-only", action="store_true", help="Do not modify application libraries when generating test assets")
    args = parser.parse_args()
    if args.fixtures_only and not args.test_assets:
        raise ValueError("--fixtures-only requires --test-assets")
    ndk = os.environ.get("ANDROID_NDK_HOME") or os.environ.get("ANDROID_NDK")
    if not ndk:
        raise ValueError("ANDROID_NDK_HOME is required for the declared Android toolchain")
    hosts = sorted((Path(ndk) / "toolchains/llvm/prebuilt").glob("*"))
    if len(hosts) != 1:
        raise ValueError("Expected exactly one NDK host toolchain")
    source = ROOT / "crates/peersus-plugin-runtime"
    for abi in args.abi or ABIS:
        compiler = hosts[0] / "bin" / (ABIS[abi] + "31-clang")
        common = [str(compiler), "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fPIC", "-shared",
                  "-fstack-protector-strong", "-fvisibility=hidden", "-Wl,-z,relro,-z,now,-z,max-page-size=16384,-z,common-page-size=16384"]
        if not args.fixtures_only:
            destination = args.output / abi
            destination.mkdir(parents=True, exist_ok=True)
            subprocess.run(common + [str(source / "native/android.c"), "-ldl", "-o", str(destination / "libpeersus_native_android.so")], check=True)
        if args.test_assets:
            assets = args.test_assets / abi
            assets.mkdir(parents=True, exist_ok=True)
            for fixture, path in {"echo.so": source / "sdk/echo.c", "data.so": source / "tests/android_data_fixture.c"}.items():
                subprocess.run(common + [str(path), "-o", str(assets / fixture)], check=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
