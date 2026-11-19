"""Regression checks for the dogfood release-comparison exit status."""
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dogfood_e2e as dogfood


class DogfoodComparisonTests(unittest.TestCase):
    def compare(self, baseline, candidate, scenarios=("first",)):
        selected = [dogfood.Scenario(name, (), lambda ctx: None) for name in scenarios]
        argv = ["dogfood", "--tachi", sys.executable, "--baseline", sys.executable]
        with patch.object(sys, "argv", argv), \
             patch.object(dogfood, "SCENARIOS", selected), \
             patch.object(dogfood, "run_binary", side_effect=[baseline, candidate]), \
             contextlib.redirect_stdout(io.StringIO()) as output:
            return dogfood.main(), output.getvalue()

    def test_two_binaries_that_cannot_start_do_not_pass_comparison(self):
        # Python is executable, but cannot act as a Tachi daemon. Exercise the
        # real CLI and harness failure path with isolated temporary homes.
        result = subprocess.run(
            [sys.executable, str(Path(dogfood.__file__)), "--tachi", sys.executable,
             "--baseline", sys.executable, "--only", "save_get_and_miss"],
            capture_output=True, text=True, timeout=20,
        )
        self.assertEqual(result.stdout.count("FAIL  harness"), 2, result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_early_return_after_matching_checks_is_incomplete(self):
        passed = dogfood.Check("first", "row returned", "PASS", "")
        complete = dogfood.Run(checks=[passed], completed=["first"])
        interrupted = dogfood.Run(checks=[
            passed, dogfood.Check("first", "first reached ctx.done()", "FAIL", "early return"),
        ])
        for baseline, candidate in ((complete, interrupted), (interrupted, complete)):
            with self.subTest(interrupted="candidate" if candidate is interrupted else "baseline"):
                result, output = self.compare(baseline, candidate)
                self.assertNotEqual(result, 0, output)
                self.assertIn("incomplete", output.lower())

    def test_same_check_name_in_another_scenario_cannot_hide_a_regression(self):
        name = "markdown search returns the row"
        baseline = dogfood.Run(checks=[
            dogfood.Check("first", name, "PASS", ""),
            dogfood.Check("second", name, "PASS", ""),
        ], completed=["first", "second"])
        candidate = dogfood.Run(checks=[
            dogfood.Check("first", name, "FAIL", "row absent"),
            dogfood.Check("second", name, "PASS", ""),
        ], completed=["first", "second"])
        result, output = self.compare(baseline, candidate, ("first", "second"))
        self.assertNotEqual(result, 0, output)
        self.assertIn(f"REGRESSION  first / {name}", output)

    def test_later_pass_cannot_erase_failure_in_the_same_check(self):
        baseline = dogfood.Run(checks=[dogfood.Check("first", "same", "PASS", "")],
                               completed=["first"])
        candidate = dogfood.Run(checks=[
            dogfood.Check("first", "same", "FAIL", "first observation failed"),
            dogfood.Check("first", "same", "PASS", "later observation passed"),
        ], completed=["first"])
        result, output = self.compare(baseline, candidate)
        self.assertNotEqual(result, 0, output)

    def test_new_candidate_failure_is_a_regression(self):
        passed = dogfood.Check("first", "existing check", "PASS", "")
        baseline = dogfood.Run(checks=[passed], completed=["first"])
        candidate = dogfood.Run(checks=[
            passed, dogfood.Check("first", "new check", "FAIL", "new failing branch"),
        ], completed=["first"])
        result, output = self.compare(baseline, candidate)
        self.assertNotEqual(result, 0, output)
        self.assertIn("REGRESSION  first / new check", output)

    def test_completed_baseline_failures_remain_valid_comparison_data(self):
        baseline = dogfood.Run(checks=[dogfood.Check("first", "known defect", "FAIL", "")],
                               completed=["first"])
        for status in ("FAIL", "PASS"):
            with self.subTest(candidate_status=status):
                candidate = dogfood.Run(
                    checks=[dogfood.Check("first", "known defect", status, "")], completed=["first"],
                )
                result, output = self.compare(baseline, candidate)
                self.assertEqual(result, 0, output)
                self.assertEqual("FIXED       first / known defect" in output, status == "PASS")


if __name__ == "__main__":
    unittest.main()
