"""Regression tests for scripts/nextest-known-reds-diff.sh (#1413 concern 5).

These tests exercise the gate's NEW behavior without a Rust toolchain:

  * exact known-red membership (the .config/nextest.toml filter is anchored to
    the full test paths, so no short fragment / prefix can be absorbed);
  * the completeness gate — a valid-but-incomplete (truncated) JUnit is refused
    with exit 4 before the script declares OK;
  * the existing outsider detection (exit 1) and malformed/missing-JUnit
    handling (exit 2).

The script's two `cargo nextest list` resolutions are bypassed via the
NEXTEST_KNOWN_REDS_KNOWN_LIST / NEXTEST_KNOWN_REDS_EXPECTED_LIST seams, so the
full bash + inline-python control flow runs against synthetic inputs.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "nextest-known-reds-diff.sh"
NEXTEST_TOML = REPO_ROOT / ".config" / "nextest.toml"

# Synthetic known-red paths used to exercise the gate's non-empty override seam.
# The live nextest configuration intentionally has no known-red members.
KNOWN_REDS = [
    "tests::dispatch_tests::board_first::successful_dispatch_seeds_status_and_kanban_before_plan_completes",
    "tests::dispatch_tests::board_first::plan_review_pending_response_projects_input_required_kanban_state",
    "tests::dispatch_tests::workflow_artifacts::ux_dispatch_gates::dispatch_confirmation::tachi_task_dispatch_requires_leader_confirmation_for_blocked_issue_flow",
]
PASSING = "tests::dispatch_tests::board_first::benign_passing_test"
OUTSIDER = "tests::dispatch_tests::board_first::a_genuinely_new_red"


def junit_xml(cases: list[tuple[str, bool]]) -> str:
    """Build a minimal nextest-style JUnit document.

    Each entry is (test_name, failed?). nextest puts the full rust path in the
    `name` attr (matching what scripts/nextest-known-reds-diff.sh extracts).
    """
    body = []
    for name, failed in cases:
        if failed:
            body.append(
                f'<testcase classname="tachi-server" name="{name}" time="0.01">'
                f'<failure message="boom" type="assert">panicked</failure></testcase>'
            )
        else:
            body.append(
                f'<testcase classname="tachi-server" name="{name}" time="0.01"/>'
            )
    inner = "".join(body)
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        f'<testsuites><testsuite name="tachi-server" tests="{len(cases)}">'
        f"{inner}</testsuite></testsuites>"
    )


def write_list(path: Path, names: list[str]) -> None:
    path.write_text("\n".join(sorted(set(names))) + "\n", encoding="utf-8")


def write_list_verbatim(path: Path, names: list[str]) -> None:
    """Write a list WITHOUT sorting or de-duplicating.

    `write_list` passes its input through `sorted(set(...))`, which would erase
    the very duplicate the AMBIGUOUS_TEST_NAME guard exists to catch.
    """
    path.write_text("\n".join(names) + "\n", encoding="utf-8")


def run_gate(
    junit_path: Path,
    known: list[str],
    expected: list[str],
    *,
    dedupe: bool = True,
) -> subprocess.CompletedProcess:
    """Invoke the gate with both cargo resolutions bypassed via the seams."""
    with tempfile.TemporaryDirectory() as tmp:
        tmpd = Path(tmp)
        known_file = tmpd / "known.txt"
        expected_file = tmpd / "expected.txt"
        writer = write_list if dedupe else write_list_verbatim
        writer(known_file, known)
        writer(expected_file, expected)
        env = {
            **os.environ,
            "NEXTEST_KNOWN_REDS_KNOWN_LIST": str(known_file),
            "NEXTEST_KNOWN_REDS_EXPECTED_LIST": str(expected_file),
            # Defensive: if a real toolchain were somehow invoked, force it off
            # the shared warm cache so the test never touches real state.
            "CARGO_TARGET_DIR": str(tmpd / "target"),
        }
        return subprocess.run(
            ["bash", str(SCRIPT), str(junit_path)],
            cwd=REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
        )


class KnownRedsGateTests(unittest.TestCase):
    def _write_junit(self, tmp: Path, cases: list[tuple[str, bool]]) -> Path:
        p = tmp / "junit.xml"
        p.write_text(junit_xml(cases), encoding="utf-8")
        return p

    def test_complete_junit_with_only_known_reds_is_ok(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], True), (KNOWN_REDS[1], True), (KNOWN_REDS[2], True), (PASSING, False)],
            )
            expected = KNOWN_REDS + [PASSING]
            res = run_gate(junit, KNOWN_REDS, expected)
            self.assertEqual(0, res.returncode, res.stdout + res.stderr)
            self.assertIn("OK", res.stdout)

    def test_clean_complete_run_with_no_failures_is_ok(self) -> None:
        # No failures at all, but the JUnit still covers the full expected set.
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], False), (PASSING, False)],
            )
            res = run_gate(junit, KNOWN_REDS[:1], [KNOWN_REDS[0], PASSING])
            self.assertEqual(0, res.returncode, res.stdout + res.stderr)

    def test_superset_junit_is_still_ok(self) -> None:
        # A workspace-wide JUnit carries MORE tests than expected; that is a
        # strict superset, not a truncation, and must not be flagged incomplete.
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], True), (PASSING, False), ("other_crate::extra::test", False)],
            )
            res = run_gate(junit, KNOWN_REDS[:1], [KNOWN_REDS[0], PASSING])
            self.assertEqual(0, res.returncode, res.stdout + res.stderr)

    def test_incomplete_junit_is_refused_before_ok(self) -> None:
        # Truncated run: one expected test (a known red) never produced a
        # testcase. Even though no outsider is present, the gate must refuse.
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], True), (PASSING, False)],  # KNOWN_REDS[1] and [2] absent
            )
            expected = list(KNOWN_REDS) + [PASSING]
            res = run_gate(junit, KNOWN_REDS, expected)
            self.assertEqual(4, res.returncode, res.stdout + res.stderr)
            self.assertIn("INCOMPLETE_JUNIT", res.stderr)

    def test_outsider_failure_is_exit1_even_when_complete(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], True), (OUTSIDER, True), (PASSING, False)],
            )
            expected = [KNOWN_REDS[0], OUTSIDER, PASSING]
            res = run_gate(junit, KNOWN_REDS, expected)
            self.assertEqual(1, res.returncode, res.stdout + res.stderr)
            self.assertIn("OUTSIDERS", res.stdout)
            self.assertIn(OUTSIDER, res.stdout)

    def test_malformed_junit_is_exit2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "junit.xml"
            bad.write_text("not xml <<", encoding="utf-8")
            res = run_gate(bad, KNOWN_REDS, KNOWN_REDS)
            self.assertEqual(2, res.returncode, res.stdout + res.stderr)

    def test_missing_junit_file_is_exit2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            res = run_gate(Path(tmp) / "does-not-exist.xml", KNOWN_REDS, KNOWN_REDS)
            self.assertEqual(2, res.returncode, res.stdout + res.stderr)

    def test_duplicate_expected_name_is_ambiguous_exit2(self) -> None:
        # #1610 Track T: the gate now spans more than one test binary, and it
        # strips the binary-id prefix so names match JUnit <testcase name>.
        # A test path present in TWO binaries therefore collapses under
        # `sort -u` into one key, letting a pass in one binary mask a failure in
        # the other. That must be a loud refusal (exit 2), not a silent merge.
        with tempfile.TemporaryDirectory() as tmp:
            junit = self._write_junit(
                Path(tmp),
                [(KNOWN_REDS[0], True), (PASSING, False)],
            )
            expected = [KNOWN_REDS[0], PASSING, PASSING]
            res = run_gate(junit, KNOWN_REDS[:1], expected, dedupe=False)
            self.assertEqual(2, res.returncode, res.stdout + res.stderr)
            self.assertIn("AMBIGUOUS_TEST_NAME", res.stderr)
            self.assertIn(PASSING, res.stderr)


class NextestTomlFilterTests(unittest.TestCase):
    """Freeze the intentionally empty known-red roster in nextest config."""

    def test_known_red_roster_is_intentionally_empty(self) -> None:
        config = tomllib.loads(NEXTEST_TOML.read_text(encoding="utf-8"))
        self.assertIn(
            "known-deterministic-reds",
            config.get("test-groups", {}),
            "known-red group definition must remain present even with an empty roster",
        )
        for profile_name in ("default", "ci"):
            overrides = config.get("profile", {}).get(profile_name, {}).get("overrides", [])
            members = [
                override
                for override in overrides
                if override.get("test-group") == "known-deterministic-reds"
            ]
            self.assertEqual(
                members,
                [],
                f"{profile_name} must not reintroduce an unreviewed known-red member",
            )

if __name__ == "__main__":
    unittest.main()
