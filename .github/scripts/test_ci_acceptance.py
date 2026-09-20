"""Discriminators for real CI result aggregation and the workflow wiring."""
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_acceptance", Path(__file__).with_name("ci_acceptance.py"))
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)
PLAN = json.loads((ROOT / ".github/acceptance-plan.json").read_text())


def green():
    return {job: {"result": "success", "outputs": {}} for job in PLAN["required_jobs"]}


class AcceptanceTests(unittest.TestCase):
    def test_all_required_jobs_pass(self):
        passed, rows = ci.evaluate(PLAN, green())
        self.assertTrue(passed)
        self.assertEqual(set(rows.values()), {"passed"})

    def test_each_non_success_is_blocking_for_every_required_job(self):
        for job in PLAN["required_jobs"]:
            for result in ("failure", "cancelled", "skipped", "neutral", "pending", "", None, 0, True, [], {}):
                with self.subTest(job=job, result=result):
                    needs = green()
                    needs[job]["result"] = result
                    self.assertFalse(ci.evaluate(PLAN, needs)[0])

    def test_skipped_and_neutral_cannot_hide_behind_green(self):
        for result in ("skipped", "neutral"):
            needs = green()
            needs["rust"]["result"] = result
            self.assertFalse(ci.evaluate(PLAN, needs)[0])

    def test_failure_cause_is_not_invented(self):
        needs = green()
        needs["node"]["result"] = "failure"
        self.assertEqual(ci.evaluate(PLAN, needs)[1]["node"], "failed_cause_unclassified")

    def test_cancelled_and_skipped_are_distinct(self):
        needs = green()
        needs["rust"]["result"] = "cancelled"
        needs["node"]["result"] = "skipped"
        rows = ci.evaluate(PLAN, needs)[1]
        self.assertEqual((rows["rust"], rows["node"]), ("cancelled", "not_run"))

    def test_missing_extra_empty_and_wrong_shaped_inventory_refuse(self):
        for needs in ({}, [], None, True):
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(PLAN, needs)
        for job in PLAN["required_jobs"]:
            needs = green()
            del needs[job]
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(PLAN, needs)
        needs = green()
        needs["extra"] = {"result": "success"}
        with self.assertRaises(ci.InvalidEvidence):
            ci.evaluate(PLAN, needs)

    def test_missing_or_malformed_job_rows_are_unknown(self):
        for row in (None, [], "success", {}, {"conclusion": "success"}):
            needs = green()
            needs["rust"] = row
            passed, results = ci.evaluate(PLAN, needs)
            self.assertFalse(passed)
            self.assertEqual(results["rust"], "unknown")

    def test_plan_is_nonempty_unique_and_nonrecursive(self):
        for jobs in ([], ["rust", "rust"], ["acceptance"], [""], ["a\nb"], [0], "rust"):
            plan = dict(PLAN, required_jobs=jobs)
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(plan, green())

    def test_plan_version_scope_and_shape_are_closed(self):
        for plan in (None, [], {}, dict(PLAN, schema_version=True),
                     dict(PLAN, schema_version=2), dict(PLAN, scope="merge_approved"),
                     dict(PLAN, optional_jobs=["rust"])):
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(plan, green())

    def test_duplicate_json_fields_are_rejected_not_collapsed(self):
        for raw in ('{"rust":{},"rust":{}}', '{"result":"failure","result":"success"}',
                    '{"schema_version":1,"schema_version":1}'):
            with self.assertRaises(ci.InvalidEvidence):
                ci.decode(raw)

    def test_raw_job_outputs_and_error_payloads_never_print(self):
        sentinel = "DO_NOT_PRINT_PRIVATE_OUTPUT"
        for value in (json.dumps(dict(green(), rust={"result": {sentinel: True}})),
                      '{"broken": "' + sentinel):
            output = io.StringIO()
            with patch.dict(os.environ, {"CI_NEEDS_JSON": value}), contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                self.assertEqual(ci.main(), 1)
            self.assertNotIn(sentinel, output.getvalue())

    def test_real_cli_exit_status(self):
        for result, code in (("success", 0), ("skipped", 1), ("failure", 1), ("neutral", 1)):
            needs = green()
            needs["rust"]["result"] = result
            env = {"CI_NEEDS_JSON": json.dumps(needs)}
            run = subprocess.run([sys.executable, str(ROOT / ".github/scripts/ci_acceptance.py")], env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(run.returncode, code, run.stderr)
            self.assertEqual(json.loads(run.stdout)["accepted"], code == 0)


class WorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        cls.rust = cls.workflow.split("  rust:\n", 1)[1].split("  physical-db-identity-windows:\n", 1)[0]

    def test_plan_needs_and_workflow_job_inventory_agree(self):
        jobs = self.workflow.split("jobs:\n", 1)[1]
        identities = re.findall(r"^  ([A-Za-z_][A-Za-z0-9_-]*):$", jobs, re.M)
        self.assertEqual(set(identities), set(PLAN["required_jobs"]) | {"acceptance"})
        needs = re.search(r"^    needs: \[([^\]]+)\]$", jobs, re.M).group(1)
        self.assertEqual([name.strip() for name in needs.split(",")], PLAN["required_jobs"])

    def test_aggregate_runs_after_failure_but_retains_owner_guard(self):
        job = self.workflow.split("  acceptance:\n", 1)[1]
        self.assertIn("always() && (", job)
        for gate in ("github.actor == github.repository_owner", "github.triggering_actor == github.repository_owner",
                     "github.event.pull_request.head.repo.full_name == github.repository",
                     "github.event.pull_request.user.login == github.repository_owner"):
            self.assertIn(gate, job)
        self.assertIn("CI_NEEDS_JSON: ${{ toJSON(needs) }}", job)
        self.assertNotIn("continue-on-error:", self.workflow)

    def test_independent_rust_checks_do_not_depend_on_audit_or_fmt_success(self):
        for name in ("Run clippy", "Check formatting", "Install cargo-audit", "Install cargo-nextest",
                     "Verify portable-kernel feature boundary", "Run doc tests"):
            body = self.rust.split("      - name: " + name + "\n", 1)[1].split("\n      - ", 1)[0]
            self.assertIn("if: ${{ !cancelled() && steps.rust_setup.outcome == 'success' }}", body)
        tests = self.rust.split("      - name: Run workspace tests\n", 1)[1].split("\n      - ", 1)[0]
        self.assertIn("steps.nextest_install.outcome == 'success'", tests)
        self.assertNotIn("steps.audit", tests)
        self.assertNotIn("steps.fmt", tests)

    def test_audit_installer_is_a_real_prerequisite(self):
        body = self.rust.split("      - name: Run cargo audit\n", 1)[1].split("\n      - ", 1)[0]
        self.assertIn("steps.audit_install.outcome == 'success'", body)
        self.assertIn("cargo audit --deny warnings", body)

    def test_success_requires_junit_but_unstarted_tests_do_not(self):
        body = self.rust.split("      - name: Archive nextest JUnit timing report\n", 1)[1].split("\n      - ", 1)[0]
        self.assertIn("steps.workspace_tests.outcome == 'success'", body)
        self.assertIn("steps.workspace_tests.outcome == 'failure' && hashFiles(", body)
        self.assertIn("if-no-files-found: error", body)
        self.assertNotIn("if: always()", body)

    def test_execution_identity_and_regression_suite_are_wired(self):
        self.assertIn("git rev-parse HEAD", self.rust)
        self.assertIn("git rev-parse 'HEAD^{tree}'", self.rust)
        self.assertIn('python3 -m unittest discover -s .github/scripts -p "test_ci_acceptance.py" -v', self.workflow)


if __name__ == "__main__":
    unittest.main()
