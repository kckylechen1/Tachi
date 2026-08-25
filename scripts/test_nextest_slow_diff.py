"""Discrimination tests for the fail-closed slow-test roster gate."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "nextest-slow-diff.sh"
ROSTERED = "memory-server-runtime tests::intentional_slow"


def run_gate(capture: str) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as temp_dir:
        temp = Path(temp_dir)
        input_path = temp / "nextest.txt"
        roster_path = temp / "roster.txt"
        input_path.write_text(capture, encoding="utf-8")
        roster_path.write_text(ROSTERED + "\n", encoding="utf-8")
        env = os.environ.copy()
        env["NEXTEST_SLOW_ROSTER"] = str(roster_path)
        return subprocess.run(
            ["bash", str(SCRIPT), str(input_path)],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )


class NextestSlowDiffTests(unittest.TestCase):
    def test_complete_green_run_without_slows_is_ok(self) -> None:
        result = run_gate(
            "Starting 1 test across 1 binary\n"
            "PASS [ 0.010s] crate tests::fast\n"
            "Summary [ 0.020s] 1 test run: 1 passed\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no slow tests", result.stdout)

    def test_failed_run_is_refused_instead_of_hollow_green(self) -> None:
        result = run_gate(
            "Starting 2 tests across 1 binary\n"
            "FAIL [ 0.010s] crate tests::fails\n"
            "Summary [ 0.020s] 1 test run: 0 passed, 1 failed, 1 cancelled\n"
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("INCOMPLETE_RUN", result.stderr)

    def test_interrupted_run_without_summary_is_refused(self) -> None:
        result = run_gate(
            "Starting 2 tests across 1 binary\n"
            "PASS [ 0.010s] crate tests::first\n"
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Summary missing", result.stderr)

    def test_truncated_capture_with_slow_line_is_refused(self) -> None:
        result = run_gate(
            f"SLOW [ 35.000s] (1/2) {ROSTERED}\n"
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Summary missing", result.stderr)

    def test_complete_green_run_accepts_rostered_slow(self) -> None:
        result = run_gate(
            f"SLOW [ 35.000s] (1/1) {ROSTERED}\n"
            "     Summary [ 35.001s] 1 test run: 1 passed, 1 slow\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("all slow tests are in the roster", result.stdout)

    def test_complete_green_flaky_run_is_still_a_valid_slow_measurement(self) -> None:
        result = run_gate(
            "TRY 1 FAIL [ 0.010s] crate tests::flaky\n"
            "     FLAKY 2/2 [ 0.020s] crate tests::flaky\n"
            "     Summary [ 0.030s] 1 test run: 1 passed (1 flaky)\n"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no slow tests", result.stdout)

    def test_complete_green_run_rejects_new_slow(self) -> None:
        result = run_gate(
            "SLOW [ 21.000s] (1/1) tachi-server tests::new_slow\n"
            "Summary [ 21.001s] 1 test run: 1 passed, 1 slow\n"
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("tachi-server tests::new_slow", result.stdout)


if __name__ == "__main__":
    unittest.main()
