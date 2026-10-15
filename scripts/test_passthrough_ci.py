"""Validation of the fixed fresh-host vote and untrusted report rejection."""
import copy
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import subprocess

import passthrough_ci as gate
import capture_ci


def report(operating_system, attempt, exceeded=False):
    baseline, limit = gate.baselines()[operating_system]
    ceiling = round(min(limit, baseline * 1.5 + 1) * 1_000_000)
    difference = ceiling + 1 if exceeded else ceiling - 1
    samples = [{"git_ns": 5_000_000, "paradox_ns": 5_000_000 + difference,
                "difference_ns": difference} for _ in range(201)]
    measurements = [{"args": list(command), "baseline_ms": baseline, "limit_ms": limit,
                     "ceiling_ns": ceiling, "git_median_ns": 5_000_000,
                     "paradox_median_ns": 5_000_000 + difference,
                     "overhead_ns": difference, "exceeded": exceeded,
                     "samples": copy.deepcopy(samples)} for command in sorted(gate.COMMANDS)]
    return dict(schema=1, os=operating_system, arch=gate.ARCHES[operating_system],
                source="source", attempt=attempt, samples_each=201, warmups_each=16,
                binary_sha256="a" * 64, test_sha256="b" * 64,
                runner_os=operating_system, runner_arch=gate.ARCHES[operating_system],
                image_os="fixed-image", image_version="fixed-version", git_version="git 1",
                workflow_run="123", workflow_attempt="1", measurements=measurements)


class AggregationTests(unittest.TestCase):
    def assess(self, reports):
        with tempfile.TemporaryDirectory() as directory:
            for number, item in enumerate(reports):
                path = Path(directory) / str(number) / "report.json"
                path.parent.mkdir()
                path.write_text(json.dumps(item), encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                return gate.aggregate(directory, "source")

    def matrix(self, noisy_attempts=()):
        return [report(name, attempt, attempt in noisy_attempts)
                for name in gate.ARCHES for attempt in (1, 2, 3)]

    def test_one_bad_host_at_any_position_does_not_fail(self):
        for attempt in (1, 2, 3):
            with self.subTest(attempt=attempt):
                self.assess(self.matrix((attempt,)))

    def test_two_bad_hosts_at_any_positions_fail(self):
        for attempts in ((1, 2), (1, 3), (2, 3)):
            with self.subTest(attempts=attempts), self.assertRaisesRegex(ValueError, "regressions"):
                self.assess(self.matrix(attempts))

    def test_each_command_votes_independently(self):
        reports = self.matrix()
        # Two hosts have a bad measurement, but for different commands.
        reports[0]["measurements"][0] = report("linux", 1, True)["measurements"][0]
        reports[1]["measurements"][1] = report("linux", 2, True)["measurements"][1]
        self.assess(reports)

    def test_missing_and_duplicate_assessments_fail(self):
        with self.assertRaisesRegex(ValueError, "nine"):
            self.assess(self.matrix()[:-1])
        reports = self.matrix()
        reports[1] = copy.deepcopy(reports[0])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.assess(reports)

    def test_changed_provenance_and_thresholds_fail(self):
        for field, value in (("source", "another-source"), ("binary_sha256", "c" * 64),
                             ("test_sha256", "d" * 64), ("image_os", "other-image")):
            reports = self.matrix()
            reports[1][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.assess(reports)
        reports = self.matrix()
        reports[0]["measurements"][0]["ceiling_ns"] += 1
        with self.assertRaisesRegex(ValueError, "gate changed"):
            self.assess(reports)

    def test_patch_image_and_native_git_versions_remain_per_host_observations(self):
        reports = self.matrix()
        reports[1]["image_version"] = "next-patch-image"
        reports[1]["git_version"] = "git-next"
        self.assess(reports)
        reports[1]["git_version"] = ""
        with self.assertRaisesRegex(ValueError, "missing image/Git"):
            self.assess(reports)

    def test_raw_pairs_and_computed_medians_are_checked(self):
        for mutation in ("signed_difference", "median", "vote", "count", "commands"):
            reports = self.matrix()
            item = reports[0]["measurements"][0]
            if mutation == "signed_difference":
                item["samples"][0]["difference_ns"] += 1
            elif mutation == "median":
                item["overhead_ns"] += 1
            elif mutation == "vote":
                item["exceeded"] = not item["exceeded"]
            elif mutation == "count":
                item["samples"].pop()
            else:
                item["args"] = ["unexpected"]
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.assess(reports)

    def test_negative_paired_median_is_clamped_for_final_overhead(self):
        reports = self.matrix()
        for measurement in reports[0]["measurements"]:
            differences = [-2] * 101 + [4] * 100
            measurement["samples"] = [{"git_ns": 10, "paradox_ns": 10 + difference,
                                       "difference_ns": difference} for difference in differences]
            measurement.update(git_median_ns=10, paradox_median_ns=8, overhead_ns=0)
        self.assess(reports)


class SelectionTests(unittest.TestCase):
    def exercise(self, targets, selected="performance"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = []
            for number, name in enumerate(targets):
                path = root / f"harness-{number}"
                path.write_bytes(f"{name}-{number}".encode())
                rows.append(dict(reason="compiler-artifact", target=dict(name=name),
                                 profile=dict(test=True), executable=str(path)))
            # Non-test binaries and artifacts without executables are ineligible.
            rows.extend([dict(reason="compiler-artifact", target=dict(name=selected),
                              profile=dict(test=False), executable="must-not-copy"),
                         dict(reason="compiler-artifact", target=dict(name=selected),
                              profile=dict(test=True), executable=None)])
            log = root / "build.jsonl"
            log.write_text("\n".join(json.dumps(row) for row in rows), encoding="utf-8")
            destination = root / "packaged" / "harness"
            if selected == "performance":
                gate.select(log, destination)
            else:
                gate.select(log, destination, selected)
            return destination.read_bytes()

    def test_default_preserves_performance_selection(self):
        self.assertEqual(self.exercise(["pending", "performance", "capture_performance"]),
                         b"performance-1")

    def test_specific_targets_select_only_the_matching_test_executable(self):
        targets = ["performance", "pending", "capture_performance"]
        for number, target in enumerate(targets):
            with self.subTest(target=target):
                self.assertEqual(self.exercise(targets, target), f"{target}-{number}".encode())

    def test_missing_target_refuses_selection(self):
        with self.assertRaisesRegex(ValueError, "one pending"):
            self.exercise(["performance", "capture_performance"], "pending")

    def test_duplicate_target_refuses_ambiguous_selection(self):
        with self.assertRaisesRegex(ValueError, "one capture_performance"):
            self.exercise(["capture_performance", "performance", "capture_performance"],
                          "capture_performance")


class CaptureRunnerTests(unittest.TestCase):
    def exercise(self, first_failure=None, mutate=False, empty=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = [root / name for name in ("pending", "capture", "paradox")]
            for path in paths:
                path.write_bytes(path.name.encode())
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                self.assertEqual(kwargs["env"]["PARADOX_BENCH_BINARY"], str(paths[2].resolve()))
                if len(calls) == 1 and first_failure == "timeout":
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                if len(calls) == 1 and first_failure == "spawn":
                    raise OSError("cannot launch harness")
                code = 101 if len(calls) == 1 and first_failure == "exit" else 0
                kwargs["stdout"].write("test result: ok. 0 passed;\n" if empty else
                                       "test result: ok. 1 passed;\n")
                if mutate and len(calls) == 1:
                    paths[2].write_bytes(b"different executable")
                return subprocess.CompletedProcess(command, code)

            environment = {name: "fixture" for name in ("GITHUB_SHA", "RUNNER_OS", "RUNNER_ARCH",
                            "ImageOS", "ImageVersion", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT")}
            with mock.patch.dict(capture_ci.os.environ, environment), \
                    mock.patch.object(capture_ci.subprocess, "run", side_effect=run), \
                    mock.patch.object(capture_ci.subprocess, "check_output", return_value="git fixture\n"), \
                    contextlib.redirect_stdout(io.StringIO()):
                code = capture_ci.run(*paths, root / "reports")
            result = json.loads((root / "reports/report.json").read_text(encoding="utf-8"))
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0][-1], "twenty_contended_savers_queue_paths_and_holder_drains_exact_final_content")
            self.assertIn("--ignored", calls[1])
            self.assertEqual(result["source"], "fixture")
            self.assertTrue((root / "reports/pending.stderr.log").exists())
            self.assertTrue((root / "reports/capture.stdout.log").exists())
            return code, result

    def test_both_gates_run_once_and_success_retains_hashes(self):
        code, report = self.exercise()
        self.assertEqual(code, 0)
        self.assertTrue(report["passed"])
        for item in report["executables"].values():
            self.assertEqual(item["sha256_before"], item["sha256_after"])

    def test_capture_still_runs_after_pending_exit_timeout_or_spawn_failure(self):
        for failure in ("exit", "timeout", "spawn"):
            with self.subTest(failure=failure):
                code, report = self.exercise(first_failure=failure)
                self.assertEqual(code, 1)
                self.assertFalse(report["passed"])
                self.assertEqual(report["assessments"][1]["returncode"], 0)

    def test_mutated_executable_and_empty_test_filter_cannot_pass(self):
        code, report = self.exercise(mutate=True)
        self.assertEqual(code, 1)
        self.assertTrue(any("changed during" in error for error in report["errors"]))
        code, report = self.exercise(empty=True)
        self.assertEqual(code, 1)
        self.assertTrue(any("one passing test" in error for error in report["errors"]))


if __name__ == "__main__":
    unittest.main()
