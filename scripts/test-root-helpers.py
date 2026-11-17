#!/usr/bin/env python3
"""Exercise root helper routing without compiling or accessing application data."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class RootHelpers(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="root-helper-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        cargo = self.directory / "cargo"
        cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "print(json.dumps({'args': sys.argv[1:], 'cwd': os.getcwd(), "
            "'target': os.environ.get('CARGO_TARGET_DIR'), 'build_target': os.environ.get('CARGO_BUILD_TARGET')}))\n"
            "sys.exit(int(os.environ.get('HELPER_TEST_EXIT', '0')))\n"
        )
        cargo.chmod(0o755)
        rustc = self.directory / "rustc"
        rustc.write_text("#!/bin/sh\nprintf 'rustc test\\nhost: x86_64-unknown-linux-gnu\\n'\n")
        rustc.chmod(0o755)
        self.environment = dict(os.environ)
        self.environment.pop("CARGO_TARGET_DIR", None)
        self.environment.pop("CARGO_BUILD_TARGET", None)
        self.environment.pop("HELPER_TEST_EXIT", None)
        self.environment["PATH"] = str(self.directory) + os.pathsep + self.environment["PATH"]

    def run_helper(self, helper, *args, **environment):
        return subprocess.run(
            [str(ROOT / helper), *args], cwd=self.directory,
            env=self.environment | environment, capture_output=True, text=True,
            timeout=10,
        )

    def test_build_routes_platform_and_keeps_output_inside_target(self):
        for args, task, target in (((), "build", "target/ui"), (("desktop",), "build", "target/ui"), (("android",), "android", "target/android")):
            with self.subTest(args=args):
                result = self.run_helper("build.sh", *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout), {"args": ["xtask", task], "cwd": str(ROOT), "target": target, "build_target": None if task == "android" else "x86_64-unknown-linux-gnu"})

    def test_explicit_target_override_is_not_replaced(self):
        for helper in ("build.sh", "check.sh"):
            for target in ("target/custom cache", str(self.directory / "absolute cache")):
                with self.subTest(helper=helper, target=target):
                    result = self.run_helper(helper, CARGO_TARGET_DIR=target)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout)["target"], target)

    def test_check_defaults_to_full_gate_and_routes_each_individual_gate(self):
        for args in ((), ("gate",), ("fmt",), ("metadata",), ("check",), ("clippy",), ("test",), ("audit",)):
            with self.subTest(args=args):
                result = self.run_helper("check.sh", *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout), {"args": ["xtask", args[0] if args else "gate"], "cwd": str(ROOT), "target": "target/ui", "build_target": "x86_64-unknown-linux-gnu"})

    def test_build_or_gate_failure_is_propagated(self):
        for helper in ("build.sh", "check.sh"):
            with self.subTest(helper=helper):
                result = self.run_helper(helper, HELPER_TEST_EXIT="17")
                self.assertEqual(result.returncode, 17)

    def test_explicit_build_target_is_preserved_without_a_layout_fallback(self):
        for helper in ("build.sh", "check.sh"):
            result = self.run_helper(helper, CARGO_BUILD_TARGET="aarch64-apple-darwin")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["build_target"], "aarch64-apple-darwin")

    def test_help_and_invalid_arguments_never_invoke_cargo(self):
        for helper in ("build.sh", "check.sh", "store-demo.sh", "website.sh"):
            for args, code in ((("--help",), 0), (("unsupported",), 2), (("--help", "extra"), 2)):
                with self.subTest(helper=helper, args=args):
                    result = self.run_helper(helper, *args, HELPER_TEST_EXIT="17")
                    self.assertEqual(result.returncode, code, result.stderr)
                    self.assertNotIn('"args":', result.stdout)


if __name__ == "__main__":
    unittest.main()
