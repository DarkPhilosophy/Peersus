#!/usr/bin/env python3
"""Exercise the memory gate against synthetic Linux procfs samples, never a real session."""
import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest.mock import patch

GATE = runpy.run_path(str(Path(__file__).with_name("memory-gate.py")))


class MemoryGate(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.proc = self.root / "proc"
        self.proc.mkdir()
        self.bin = self.root / ".local/lib/peersus"
        self.bin.mkdir(parents=True)
        launcher = self.root / ".local/bin/peersus"
        launcher.parent.mkdir()
        launcher.symlink_to("../lib/peersus/peersus")
        self.data = self.root / "profile"
        self.data.mkdir()
        (self.data / "instance.lock").touch()
        self.baseline = self.root / "baseline.json"
        self.conditions = ["--page", "send", "--extension-pages", "0", "--canvas", "opened-and-closed",
                           "--paired", "4", "--online", "3", "--renderer", "software", "--window-size", "1280x800"]
        self.programs = {}
        for name in ("peersus", "peersus-ui", "peersus-wasm-host"):
            path = self.bin / name
            path.write_text(f"build artifact: {name}")
            path.chmod(0o755)
            self.programs[name] = path
        (self.proc / "uptime").write_text("900.00 0.00\n")
        module = self.fixture_module()
        module.parent.mkdir(parents=True)
        module.write_bytes(b"\0asm\1\0\0\0")
        self.process(10, "peersus-ui", 1)
        self.process(11, "peersus", 10, holder=True)
        self.process(12, "peersus-wasm-host", 11, module=self.fixture_module())

    def fixture_module(self):
        return self.data / "plugins" / "private-module-name" / "extension.wasm"

    def process(self, pid, name, parent, *, exe=None, holder=False, module=None, start=100):
        entry = self.proc / str(pid)
        entry.mkdir()
        (entry / "exe").symlink_to(exe or self.programs[name])
        (entry / "cmdline").write_bytes(os.fsencode(name) + b"\0" + (os.fsencode(module) + b"\0" if module else b""))
        fields = ["S", str(parent), *(["0"] * 17), str(start * os.sysconf("SC_CLK_TCK"))]
        (entry / "stat").write_text(f"{pid} (synthetic (comm)) {' '.join(fields)}\n")
        if holder:
            (entry / "fd").mkdir()
            (entry / "fdinfo").mkdir()
            (entry / "fd" / "4").symlink_to(self.data / "instance.lock")
            (entry / "fdinfo" / "4").write_text(f"lock:\t1: FLOCK ADVISORY WRITE {pid} 00:01:2 0 EOF\n")
        self.environment(pid)
        self.sample(pid, anon=2400)

    def environment(self, pid, *entries):
        (self.proc / str(pid) / "environ").write_bytes(b"\0".join(entries) + b"\0")

    def sample(self, pid=12, *, anon, rss=5000, pss=3000, swap=0, huge=0):
        keys = ("Rss", "Pss", "Pss_Anon", "SwapPss", "AnonHugePages")
        (self.proc / str(pid) / "smaps_rollup").write_text(
            "0-0 ---p 0 00:00 0 [rollup]\n" + "".join(
                f"{k}: {v} kB\n" for k, v in zip(keys, (rss, pss, anon, swap, huge))
            )
        )

    def gate(self, action, conditions=None, pause=lambda _: None):
        args = [action, str(self.baseline), "--data-dir", str(self.data),
                *(self.conditions if conditions is None else conditions)]
        with patch.object(Path, "home", return_value=self.root):
            return self.invoke(lambda: GATE["main"](args, proc=self.proc, pause=pause))

    def invoke(self, call):
        stream = io.StringIO()
        with contextlib.redirect_stdout(stream), contextlib.redirect_stderr(stream):
            try:
                result = call()
            except SystemExit as error:
                result = error.code
        return result, stream.getvalue()

    def record(self):
        status, output = self.gate("--record")
        self.assertEqual(status, 0, output)
        self.assertIn("RESULT=RECORDED", output)

    def series(self, anon, readings):
        remaining = iter(readings)

        def advance(_):
            self.sample(anon=anon, huge=next(remaining))

        advance(None)
        return advance

    def test_old_baseline_requires_explicit_record_before_sampling(self):
        original = '{"owner":{"rss":40.4,"pss":37.6,"private_dirty":11.0}}'
        self.baseline.write_text(original)
        # Supply the old script with a harmless synthetic sample too: its failure should be its
        # incorrect decision, not an empty workstation or missing permission on real /proc.
        with patch.object(sys, "argv", ["memory-gate.py", "--check", str(self.baseline)]), patch.dict(
            GATE["main"].__globals__, {"roles": lambda: {"owner": 1},
                                       "measure": lambda _: {"rss": 40.4, "pss": 37.6}}
        ):
            status, output = self.invoke(GATE["main"])
        self.assertEqual(status, 3, output)
        self.assertIn("RESULT=LEGACY_BASELINE", output)
        self.assertIn("--record", output)
        self.assertEqual(self.baseline.read_text(), original)

    def test_one_huge_page_in_either_direction_does_not_flip_pass(self):
        self.record()
        self.sample(anon=4448, pss=5048, rss=7048, huge=2048)
        status, output = self.gate("--check")
        self.assertEqual(status, 0, output)
        self.assertIn("anon_huge_kib=2048", output)
        self.record()
        self.sample(anon=2400)
        status, output = self.gate("--check")
        self.assertEqual(status, 0, output)

    def test_a_transient_unmatched_huge_page_does_not_supply_the_median(self):
        self.record()
        visits = iter((2400, 2400, 2400, 2400))
        self.sample(anon=6600, pss=7200, rss=9200, huge=4096)
        status, output = self.gate("--check", pause=lambda _: self.sample(anon=next(visits)))
        self.assertEqual(status, 0, output)
        self.assertIn("anon_swap_kib=2400", output)

    def test_persistent_huge_page_uncertainty_is_not_reported_as_growth(self):
        self.record()
        self.sample(anon=4480, pss=5080, rss=7080, huge=2048)
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("THP residency", output)
        self.assertNotIn("GREW", output)

    def test_current_and_reference_median_ties_are_order_independent(self):
        orders = ((0, 0, 2048, 2048, 2048), (2048, 2048, 0, 2048, 0))
        self.sample(anon=2400, huge=2048)
        self.record()
        for order in orders:
            with self.subTest(tied="current", order=order):
                status, output = self.gate("--check", pause=self.series(4300, order))
                self.assertEqual(status, 2, output)
                self.assertIn("THP residency", output)

        for order in orders:
            with self.subTest(tied="reference", order=order):
                status, output = self.gate("--record", pause=self.series(2400, order))
                self.assertEqual(status, 0, output)
                self.sample(anon=4300)
                status, output = self.gate("--check")
                self.assertEqual(status, 2, output)
                self.assertIn("THP residency", output)

    def test_median_ties_are_accepted_when_every_thp_reading_agrees(self):
        self.record()
        status, output = self.gate("--check", pause=self.series(2400, (0, 2048, 0, 2048, 0)))
        self.assertEqual(status, 0, output)
        self.assertIn("RESULT=PASS", output)

    def test_swapping_anonymous_pages_does_not_flip_the_verdict(self):
        self.record()
        self.sample(anon=1000, pss=1600, rss=3600, swap=1400)
        status, output = self.gate("--check")
        self.assertEqual(status, 0, output)
        self.assertIn("anon_swap_kib=2400", output)
        self.assertIn("rss_kib=3600", output)

    def test_real_anon_growth_one_kib_over_limit_fails(self):
        self.record()
        self.sample(anon=4449, pss=5200, rss=7000)
        status, output = self.gate("--check")
        self.assertEqual(status, 1, output)
        self.assertIn("limit_kib=4448", output)
        self.assertIn("RESULT=GREW", output)

    def test_two_scoped_workers_with_the_same_role_are_ambiguous(self):
        self.record()
        self.process(13, "peersus-wasm-host", 11, module=self.fixture_module())
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("ambiguous role=extension-1", output)
        self.assertNotIn("private-module-name", output)
        self.assertNotIn(str(self.root), output)

    def test_second_scoped_window_is_ambiguous(self):
        self.record()
        self.process(13, "peersus-ui", 11)
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("ambiguous role=window", output)

    def test_external_probe_and_unrelated_tree_cannot_shadow_the_owner(self):
        self.record()
        probe = self.root / "other-peersus"
        probe.mkdir()
        other = probe / "peersus-wasm-host"
        other.write_bytes(self.programs[other.name].read_bytes())
        self.process(13, other.name, 11, exe=other, module=self.fixture_module())
        self.process(14, other.name, 1, module=self.fixture_module())
        self.process(15, "peersus", 1)  # Same executable, no lock in the chosen profile.
        self.sample(13, anon=14000, pss=15000, rss=17000)
        self.sample(14, anon=14000, pss=15000, rss=17000)
        self.sample(15, anon=14000, pss=15000, rss=17000)
        status, output = self.gate("--check")
        self.assertEqual(status, 0, output)
        self.assertNotIn("14000", output)

    def test_missing_worker_does_not_create_a_pass(self):
        self.record()
        (self.proc / "12" / "exe").unlink()
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("role set", output)

    def test_opened_page_and_peer_count_are_reported_as_different_conditions(self):
        self.record()
        self.sample(anon=9000, pss=9600, rss=11000)
        conditions = ["--page", "extension", "--extension-pages", "1", "--canvas", "open",
                      "--paired", "4", "--online", "2", "--renderer", "software", "--window-size", "1280x800"]
        status, output = self.gate("--check", conditions)
        self.assertEqual(status, 2, output)
        for field in ("page", "extension_pages", "canvas", "online"):
            self.assertIn(f"condition={field}", output)
        self.assertNotIn("GREW", output)

    def test_renderer_and_window_size_must_be_attested_and_match(self):
        self.record()
        for flag, changed in (("--renderer", "vulkan"), ("--window-size", "1280x801")):
            with self.subTest(flag=flag):
                words = self.conditions.copy()
                index = words.index(flag)
                words[index + 1] = changed
                status, output = self.gate("--check", words)
                self.assertEqual(status, 2, output)
                self.assertIn(f"condition={flag[2:].replace('-', '_')}", output)
                del words[index:index + 2]
                status, output = self.gate("--check", words)
                self.assertEqual(status, 2, output)
                self.assertIn(flag, output)

    def test_modified_installed_module_is_reported_without_its_name(self):
        self.record()
        self.fixture_module().write_bytes(b"\0asm\1\0\0\0new guest")
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("condition=module role=extension-1", output)
        self.assertNotIn("private-module-name", output)
        self.assertNotIn(str(self.root), output)

    def test_module_replaced_during_sampling_is_not_comparable(self):
        self.record()
        changed = self.fixture_module().with_suffix(".new")
        changed.write_bytes(self.fixture_module().read_bytes())

        status, output = self.gate("--check", pause=lambda _: changed.replace(self.fixture_module()))
        self.assertEqual(status, 2, output)
        self.assertIn("changed during sampling", output)

    def test_modified_installed_helper_is_not_a_comparable_workload(self):
        self.record()
        self.programs["peersus-wasm-host"].write_text("change only the helper build")
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("condition=helper_binary role=extension-1", output)

    def test_allocator_difference_is_taken_from_each_process(self):
        self.record()
        for pid, role in ((10, "window"), (11, "owner"), (12, "extension-1")):
            with self.subTest(role=role):
                self.environment(pid, b"MALLOC_MMAP_THRESHOLD_=4194304", b"OTHER=private-fixture")
                status, output = self.gate("--check")
                self.assertEqual(status, 2, output)
                self.assertIn(f"condition=allocator role={role}", output)
                for sensitive in ("4194304", "private-fixture"):
                    self.assertNotIn(sensitive, output)
                    self.assertNotIn(sensitive, self.baseline.read_text())
                self.environment(pid)

    def test_allocator_measurement_stores_only_a_boolean_and_matches_whole_keys(self):
        # Split an environment item across read blocks and include a false match in its
        # value. Neither that value nor the real threshold should appear in the baseline.
        self.environment(11, b"OTHER=" + b"x" * 4080 + b"MALLOC_MMAP_THRESHOLD_=unrelated",
                         b"MALLOC_MMAP_THRESHOLD_=4194304")
        self.record()
        raw = self.baseline.read_text()
        data = json.loads(raw)
        self.assertIs(data["roles"]["owner"]["fixed_mmap"], True)
        self.assertNotIn("4194304", raw)
        self.assertNotIn("unrelated", raw)

        self.environment(11, b"OTHER=MALLOC_MMAP_THRESHOLD_=4194304")
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("condition=allocator role=owner", output)

    def test_explicit_nonpackaged_thresholds_never_compare_as_unset(self):
        self.record()
        for value in (b"", b"8388608"):
            with self.subTest(value=value):
                self.environment(11, b"MALLOC_MMAP_THRESHOLD_=" + value)
                status, output = self.gate("--check")
                self.assertEqual(status, 2, output)
                self.assertIn("condition=allocator role=owner", output)
                self.assertIn("custom mmap threshold", output)
                self.assertNotIn("8388608", output)

    def test_material_age_difference_is_inconclusive(self):
        self.record()
        (self.proc / "uptime").write_text("1021.00 0.00\n")
        status, output = self.gate("--check")
        self.assertEqual(status, 2, output)
        self.assertIn("condition=process_age", output)

    def test_executable_identity_changes_across_comparable_builds(self):
        self.record()
        saved = json.loads(self.baseline.read_text())["build"]
        self.programs["peersus"].write_text("different release bytes")
        status, output = self.gate("--check")
        self.assertEqual(status, 0, output)
        self.assertIn(f"baseline_build={saved}", output)
        self.assertNotIn(f"\nbuild={saved}\n", output)


if __name__ == "__main__":
    unittest.main()
