"""Validation of the fixed fresh-host vote and untrusted report rejection."""
import copy
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import passthrough_ci as gate


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
                             ("test_sha256", "d" * 64), ("image_version", "other-image")):
            reports = self.matrix()
            reports[1][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.assess(reports)
        reports = self.matrix()
        reports[0]["measurements"][0]["ceiling_ns"] += 1
        with self.assertRaisesRegex(ValueError, "gate changed"):
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


if __name__ == "__main__":
    unittest.main()
