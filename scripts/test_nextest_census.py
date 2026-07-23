#!/usr/bin/env python3
"""Direct behavioral coverage for scripts/nextest-census.sh."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SOURCE_SCRIPT = Path(__file__).with_name("nextest-census.sh")


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
junit = Path(os.environ[\"FAKE_WORKSPACE\"]) / \"target/nextest/census/junit.xml\"
junit.parent.mkdir(parents=True, exist_ok=True)
junit.write_text(\"\"\"<testsuites><testsuite><testcase classname=\"tachi_server::census\" name=\"records_provenance\" time=\"1.5\"><failure message=\"thread 'census' (4242) panicked at deliberate failure\" /></testcase></testsuite></testsuites>\"\"\")
sys.exit(7)
""",
                encoding="utf-8",
            )
            fake_cargo.chmod(0o755)

            target_dir = temp / "fresh-target"
            evidence_dir = temp / "evidence"
            cargo_args = temp / "cargo-args.json"
            env = os.environ | {
                "CARGO_TARGET_DIR": str(target_dir),
                "NEXTEST_CENSUS_DIR": str(evidence_dir),
                "NEXTEST_TEST_THREADS": "8",
                "FAKE_CARGO_ARGS": str(cargo_args),
                "FAKE_WORKSPACE": str(workspace),
                "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
            }

            first = subprocess.run(
                ["bash", str(script)], text=True, capture_output=True, env=env, check=False
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(
                json.loads(cargo_args.read_text(encoding="utf-8")),
                [
                    "nextest",
                    "run",
                    "-p",
                    "tachi-server",
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
            self.assertEqual(first_row["target_pre_run_state"], "absent")
            self.assertTrue(first_row["target_clean_before_run"])
            self.assertGreaterEqual(first_row["run_runtime_s"], 0)
            self.assertEqual(first_row["nextest_exit"], 7)
            self.assertEqual(first_row["test_id"], "tachi_server::census::records_provenance")
            self.assertTrue(first_row["failure_line1_hash"])
            self.assertEqual(first_row["prior_matching_failures"], 0)
            self.assertEqual(first_row["recurrence"], "novel")

            second = subprocess.run(
                ["bash", str(script)], text=True, capture_output=True, env=env, check=False
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            second_row = json.loads(jsonl.read_text(encoding="utf-8").splitlines()[1])
            self.assertEqual(second_row["prior_matching_failures"], 1)
            self.assertEqual(second_row["recurrence"], "recurrent")


if __name__ == "__main__":
    unittest.main()
