#!/usr/bin/env python3
"""Direct behavioral coverage for scripts/nextest-census.sh."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


SOURCE_SCRIPT = Path(__file__).with_name("nextest-census.sh")
KNOWN_REDS_SCRIPT = Path(__file__).with_name("nextest-known-reds-diff.sh")
EXPECTED_PACKAGES = [
    "tachi-server",
    "tachi-bootstrap-tests",
    "tachi-contract-tests",
    "tachi-credential-profile",
    "tachi-gh-safe-merge",
    "tachi-github-runtime",
    "tachi-lesson-forge",
    "tachi-build-broker",
    "tachi-llm",
    "tachi-foundry",
]


def package_inventory(script: Path, assignment: str) -> list[str]:
    text = script.read_text(encoding="utf-8")
    match = re.search(rf"^[ \t]*{assignment}=\(([^)]*)\)$", text, re.MULTILINE)
    if match is None:
        raise AssertionError(f"missing {assignment} package inventory in {script}")
    return re.findall(r"(?:^|\s)-p\s+([a-z0-9-]+)(?=\s|$)", match.group(1))


def run_exit_contract_case(
    temp: Path, *, cargo_exit: int, junit_mode: str
) -> tuple[subprocess.CompletedProcess[str], Path]:
    workspace = temp / "workspace"
    script_dir = workspace / "scripts"
    script_dir.mkdir(parents=True)
    script = script_dir / "nextest-census.sh"
    shutil.copy2(SOURCE_SCRIPT, script)

    fake_bin = temp / "bin"
    fake_bin.mkdir()
    fake_cargo = fake_bin / "cargo"
    fake_cargo.write_text(
        """#!/usr/bin/env python3
import os
from pathlib import Path
import sys

junit = Path(os.environ["FAKE_WORKSPACE"]) / "target/nextest/census/junit.xml"
mode = os.environ["FAKE_JUNIT_MODE"]
if mode != "missing":
    junit.parent.mkdir(parents=True, exist_ok=True)
if mode == "failure":
    junit.write_text('<testsuites><testsuite><testcase classname="census" name="fails"><failure message="deliberate failure" /></testcase></testsuite></testsuites>')
elif mode == "empty":
    junit.write_text('<testsuites><testsuite><testcase classname="census" name="passes" /></testsuite></testsuites>')
elif mode == "malformed":
    junit.write_text("<testsuites>")
elif mode != "missing":
    raise ValueError(f"unsupported FAKE_JUNIT_MODE: {mode}")
sys.exit(int(os.environ["FAKE_CARGO_EXIT"]))
""",
        encoding="utf-8",
    )
    fake_cargo.chmod(0o755)

    evidence_dir = temp / "evidence"
    env = os.environ | {
        "CARGO_TARGET_DIR": str(temp / "cargo-target"),
        "NEXTEST_CENSUS_DIR": str(evidence_dir),
        "FAKE_CARGO_EXIT": str(cargo_exit),
        "FAKE_JUNIT_MODE": junit_mode,
        "FAKE_WORKSPACE": str(workspace),
        "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
    }
    result = subprocess.run(
        ["bash", str(script)],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )
    return result, evidence_dir / "census.jsonl"


class NextestCensusScriptTest(unittest.TestCase):
    def test_records_explicit_parallel_clean_target_and_recurrence(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            workspace = temp / "workspace"
            script_dir = workspace / "scripts"
            script_dir.mkdir(parents=True)
            script = script_dir / "nextest-census.sh"
            shutil.copy2(SOURCE_SCRIPT, script)

            fake_bin = temp / "bin"
            fake_bin.mkdir()
            fake_cargo = fake_bin / "cargo"
            fake_cargo.write_text(
                """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

Path(os.environ[\"FAKE_CARGO_ARGS\"]).write_text(json.dumps(sys.argv[1:]))
target = Path(os.environ[\"CARGO_TARGET_DIR\"])
if not target.exists():
    target_state = \"absent\"
elif not target.is_dir():
    target_state = \"not_a_directory\"
elif any(target.iterdir()):
    target_state = \"nonempty\"
else:
    target_state = \"empty\"
Path(os.environ[\"FAKE_TARGET_STATE\"]).write_text(target_state)
junit = Path(os.environ[\"FAKE_WORKSPACE\"]) / \"target/nextest/census/junit.xml\"
junit.parent.mkdir(parents=True, exist_ok=True)
junit.write_text(\"\"\"<testsuites><testsuite><testcase classname=\"tachi_server::census\" name=\"records_provenance\" time=\"1.5\"><failure message=\"thread 'census' (4242) panicked at deliberate failure\" /></testcase></testsuite></testsuites>\"\"\")
sys.exit(7)
""",
                encoding="utf-8",
            )
            fake_cargo.chmod(0o755)

            target_dir = temp / "fresh-target"
            evidence_dir = target_dir / "nextest-census"
            cargo_args = temp / "cargo-args.json"
            target_state = temp / "target-state.txt"
            env = os.environ | {
                "CARGO_TARGET_DIR": str(target_dir),
                "NEXTEST_TEST_THREADS": "8",
                "FAKE_CARGO_ARGS": str(cargo_args),
                "FAKE_TARGET_STATE": str(target_state),
                "FAKE_WORKSPACE": str(workspace),
                "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
            }
            env.pop("NEXTEST_CENSUS_DIR", None)

            first = subprocess.run(
                ["bash", str(script)], text=True, capture_output=True, env=env, check=False
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(target_state.read_text(encoding="utf-8"), "absent")
            self.assertEqual(
                json.loads(cargo_args.read_text(encoding="utf-8")),
                [
                    "nextest",
                    "run",
                    *[
                        item
                        for package in EXPECTED_PACKAGES
                        for item in ("-p", package)
                    ],
                    "--no-fail-fast",
                    "--profile",
                    "census",
                    "--target-dir",
                    str(target_dir),
                    "--test-threads",
                    "8",
                ],
            )

            jsonl = evidence_dir / "census.jsonl"
            first_row = json.loads(jsonl.read_text(encoding="utf-8").splitlines()[0])
            self.assertEqual(first_row["test_threads"], "8")
            self.assertEqual(first_row["target_dir"], str(target_dir))
            self.assertEqual(first_row["target_source"], "CARGO_TARGET_DIR")
            self.assertEqual(first_row["target_state_at_invocation"], "absent")
            self.assertTrue(first_row["target_clean_at_invocation"])
            self.assertGreaterEqual(first_row["run_runtime_s"], 0)
            self.assertEqual(first_row["nextest_exit"], 7)
            self.assertIn("summary nextest_exit=7", first.stdout)
            self.assertEqual(first_row["test_id"], "tachi_server::census::records_provenance")
            self.assertTrue(first_row["failure_line1_hash"])
            self.assertEqual(first_row["prior_matching_failures"], 0)
            self.assertEqual(first_row["recurrence"], "novel")

            second = subprocess.run(
                ["bash", str(script)], text=True, capture_output=True, env=env, check=False
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(target_state.read_text(encoding="utf-8"), "nonempty")
            second_row = json.loads(jsonl.read_text(encoding="utf-8").splitlines()[1])
            self.assertEqual(second_row["target_state_at_invocation"], "nonempty")
            self.assertFalse(second_row["target_clean_at_invocation"])
            self.assertEqual(second_row["prior_matching_failures"], 1)
            self.assertEqual(second_row["recurrence"], "recurrent")

    def test_package_inventories_match_without_duplicates(self) -> None:
        census_packages = package_inventory(SOURCE_SCRIPT, "nextest_args")
        known_reds_packages = package_inventory(KNOWN_REDS_SCRIPT, "NEXTEST_PACKAGES")

        self.assertEqual(census_packages, EXPECTED_PACKAGES)
        self.assertEqual(known_reds_packages, EXPECTED_PACKAGES)
        self.assertEqual(census_packages, known_reds_packages)
        self.assertEqual(len(census_packages), len(set(census_packages)))
        self.assertEqual(len(known_reds_packages), len(set(known_reds_packages)))

    def test_exit_contract_distinguishes_capture_failures(self) -> None:
        cases = (
            ("success_without_failures", 0, "empty", 0, 0),
            ("nonzero_with_failure", 100, "failure", 0, 1),
            ("nonzero_without_failures", 100, "empty", 100, 0),
            ("missing_junit", 100, "missing", 2, 0),
            ("malformed_junit", 100, "malformed", 2, 0),
        )
        for name, cargo_exit, junit_mode, wrapper_exit, expected_rows in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp_dir:
                result, jsonl = run_exit_contract_case(
                    Path(temp_dir), cargo_exit=cargo_exit, junit_mode=junit_mode
                )

                self.assertEqual(result.returncode, wrapper_exit, result.stderr)
                rows = jsonl.read_text(encoding="utf-8").splitlines() if jsonl.exists() else []
                self.assertEqual(len(rows), expected_rows)
                if expected_rows:
                    self.assertEqual(json.loads(rows[0])["nextest_exit"], cargo_exit)
                if name == "nonzero_without_failures":
                    self.assertIn(
                        "summary nextest_exit=100",
                        result.stdout,
                    )
                    self.assertIn("failed=0", result.stdout)
                    self.assertIn(
                        "nextest exited 100 but valid JUnit contained zero failure/error nodes",
                        result.stderr,
                    )

    def test_malformed_junit_names_original_nextest_exit_without_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            result, jsonl = run_exit_contract_case(
                Path(temp_dir), cargo_exit=100, junit_mode="malformed"
            )

            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("malformed JUnit", result.stderr)
            self.assertIn("nextest_exit=100", result.stderr)
            rows = jsonl.read_text(encoding="utf-8").splitlines() if jsonl.exists() else []
            self.assertEqual(rows, [], "malformed JUnit must not append a receipt")


if __name__ == "__main__":
    unittest.main()
