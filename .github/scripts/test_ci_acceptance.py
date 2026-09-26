"""Discriminators for real CI result aggregation and the workflow wiring.

Frozen-invariant update, owner adjudication 2026-09-23 (current delivery
only): `physical-db-identity-windows` moved from required to observational
in `.github/acceptance-plan.json` (schema v2). The superseded assertion --
the Windows matrix blocks acceptance -- is replaced below by: Windows failure
or non-execution never blocks while staying visibly reported as excluded.
Every other frozen assertion keeps its intent: required jobs fail closed on
any non-success, the plan shape and needs inventory stay closed, duplicate
JSON fields are refused, raw outputs never print, and the workflow carries no
`continue-on-error` or job-level Windows skip.

Frozen-literal update, leader ruling on PR #2010 (astra r1 finding 2, #1998):
the audit step's literal `cargo audit --deny warnings` becomes the bound form
`"${AUDITED_CARGO_AUDIT:?}" audit --deny warnings`. Same subcommand and flags,
same install prerequisite; the executable is now the checksum-verified file
instead of whatever Cargo's external-subcommand lookup finds first.
"""
import contextlib
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
ALL_JOBS = PLAN["required_jobs"] + PLAN["observational_jobs"]
WINDOWS = "physical-db-identity-windows"


def green():
    return {job: {"result": "success", "outputs": {}} for job in ALL_JOBS}


class AcceptanceTests(unittest.TestCase):
    def test_all_required_jobs_pass(self):
        passed, rows = ci.evaluate(PLAN, green())
        self.assertTrue(passed)
        self.assertEqual({rows[job] for job in PLAN["required_jobs"]}, {"passed"})
        self.assertEqual({rows[job] for job in PLAN["observational_jobs"]},
                         {"observational_passed"})

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

    def test_windows_failure_or_nonexecution_never_blocks_but_stays_excluded(self):
        expected = {
            "failure": "observational_failed_excluded",
            "cancelled": "observational_cancelled_excluded",
            "skipped": "observational_not_run_excluded",
        }
        for result in ("failure", "cancelled", "skipped", "neutral", "pending", "", None, 0, True, [], {}):
            with self.subTest(result=result):
                needs = green()
                needs[WINDOWS]["result"] = result
                passed, rows = ci.evaluate(PLAN, needs)
                self.assertTrue(passed)
                self.assertTrue(rows[WINDOWS].startswith("observational_"))
                self.assertIn("excluded", rows[WINDOWS])
                if isinstance(result, str) and result in expected:
                    self.assertEqual(rows[WINDOWS], expected[result])

    def test_windows_row_malformed_stays_loud_not_blocking(self):
        for row in (None, [], "success", {}, {"conclusion": "success"}):
            with self.subTest(row=row):
                needs = green()
                needs[WINDOWS] = row
                passed, rows = ci.evaluate(PLAN, needs)
                self.assertTrue(passed)
                self.assertEqual(rows[WINDOWS], "observational_unknown_excluded")

    def test_required_failure_still_blocks_alongside_excluded_windows(self):
        baseline = green()
        baseline[WINDOWS]["result"] = "failure"
        for job in PLAN["required_jobs"]:
            with self.subTest(job=job):
                needs = {name: dict(row) for name, row in baseline.items()}
                needs[job]["result"] = "failure"
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
        for job in ALL_JOBS:
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
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(dict(PLAN, required_jobs=jobs), green())
        for jobs in (["rust", "rust"], ["acceptance"], [""], ["a\nb"], [0], "rust"):
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(dict(PLAN, observational_jobs=jobs), green())
        with self.assertRaises(ci.InvalidEvidence):
            ci.evaluate(dict(PLAN, observational_jobs=list(PLAN["required_jobs"])), green())

    def test_empty_observational_list_can_restore_windows_as_required(self):
        # The exclusion is a narrow, reversible plan entry: moving the job
        # back into required_jobs reinstates blocking without schema change.
        plan = dict(PLAN, required_jobs=PLAN["required_jobs"] + [WINDOWS],
                    observational_jobs=[])
        self.assertTrue(ci.evaluate(plan, green())[0])
        needs = green()
        needs[WINDOWS]["result"] = "failure"
        self.assertFalse(ci.evaluate(plan, needs)[0])
        needs.pop(WINDOWS)
        with self.assertRaises(ci.InvalidEvidence):
            ci.evaluate(plan, needs)

    def test_plan_version_scope_and_shape_are_closed(self):
        schema_v1 = {key: value for key, value in PLAN.items()
                     if key != "observational_jobs"}
        for plan in (None, [], {}, dict(PLAN, schema_version=True),
                     dict(PLAN, schema_version=1), dict(PLAN, schema_version=3),
                     dict(PLAN, scope="merge_approved"),
                     dict(PLAN, optional_jobs=["rust"]), schema_v1):
            with self.assertRaises(ci.InvalidEvidence):
                ci.evaluate(plan, green())

    def test_duplicate_json_fields_are_rejected_not_collapsed(self):
        for raw in ('{"rust":{},"rust":{}}', '{"result":"failure","result":"success"}',
                    '{"schema_version":1,"schema_version":1}'):
            with self.assertRaises(ci.InvalidEvidence):
                ci.decode(raw)

    def test_raw_job_outputs_and_error_payloads_never_print(self):
        sentinel = "DO_NOT_PRINT_PRIVATE_OUTPUT"
        accepted = green()
        accepted[WINDOWS]["outputs"] = {sentinel: True}
        for value in (json.dumps(dict(green(), rust={"result": {sentinel: True}})),
                      json.dumps(accepted),
                      '{"broken": "' + sentinel):
            output = io.StringIO()
            with patch.dict(os.environ, {"CI_NEEDS_JSON": value}), contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                self.assertIn(ci.main(), (0, 1))
            self.assertNotIn(sentinel, output.getvalue())

    def test_real_cli_exit_status(self):
        for result, code in (("success", 0), ("skipped", 1), ("failure", 1), ("neutral", 1)):
            needs = green()
            needs["rust"]["result"] = result
            env = {"CI_NEEDS_JSON": json.dumps(needs)}
            run = subprocess.run([sys.executable, str(ROOT / ".github/scripts/ci_acceptance.py")], env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(run.returncode, code, run.stderr)
            self.assertEqual(json.loads(run.stdout)["accepted"], code == 0)

    def test_cli_accepts_current_delivery_with_windows_reported_excluded(self):
        # Mirrors CI attempt 2 (run 35838923274): required lanes green,
        # Windows legs red -- the aggregate accepts while the receipt names
        # the excluded job, its raw verdict, and the dated policy.
        needs = green()
        needs[WINDOWS]["result"] = "failure"
        env = {"CI_NEEDS_JSON": json.dumps(needs)}
        run = subprocess.run([sys.executable, str(ROOT / ".github/scripts/ci_acceptance.py")], env=env, capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 0, run.stderr)
        report = json.loads(run.stdout)
        self.assertTrue(report["accepted"])
        self.assertEqual(report["required_jobs"],
                         {job: "passed" for job in PLAN["required_jobs"]})
        self.assertEqual(report["observational_jobs"],
                         {WINDOWS: "observational_failed_excluded"})
        self.assertEqual(report["observational_exclusions"],
                         {WINDOWS: "observational_failed_excluded"})
        self.assertIn("2026-09-23", report["policy"])
        self.assertIn("#1963", report["policy"])


class WorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        cls.rust = cls.workflow.split("  rust:\n", 1)[1].split("  physical-db-identity-windows:\n", 1)[0]
        cls.windows = cls.workflow.split("  physical-db-identity-windows:\n", 1)[1].split("  node:\n", 1)[0]

    def test_plan_needs_and_workflow_job_inventory_agree(self):
        jobs = self.workflow.split("jobs:\n", 1)[1]
        identities = re.findall(r"^  ([A-Za-z_][A-Za-z0-9_-]*):$", jobs, re.M)
        self.assertEqual(set(identities), set(ALL_JOBS) | {"acceptance"})
        needs = re.search(r"^    needs: \[([^\]]+)\]$", jobs, re.M).group(1)
        self.assertEqual([name.strip() for name in needs.split(",")], ALL_JOBS)

    def test_windows_matrix_stays_defined_unconditional_and_dated(self):
        self.assertIn("runs-on: windows-latest", self.windows)
        self.assertIn("fail-fast: false", self.windows)
        self.assertEqual(re.search(r"crate: \[([^\]]+)\]", self.windows).group(1),
                         "tachi-server, memcore, tachi-clean")
        self.assertNotIn("\n    if:", self.windows)
        self.assertIn("2026-09-23", self.windows)
        self.assertIn("#1963", self.windows)

    def test_aggregate_runs_after_failure_but_retains_owner_guard(self):
        job = self.workflow.split("  acceptance:\n", 1)[1]
        self.assertIn("always() && (", job)
        for gate in ("github.actor == github.repository_owner", "github.triggering_actor == github.repository_owner",
                     "github.event.pull_request.head.repo.full_name == github.repository",
                     "github.event.pull_request.user.login == github.repository_owner"):
            self.assertIn(gate, job)
        self.assertIn("CI_NEEDS_JSON: ${{ toJSON(needs) }}", job)
        self.assertIn("python3 .github/scripts/ci_acceptance.py", job)
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
        self.assertIn('"${AUDITED_CARGO_AUDIT:?}" audit --deny warnings', body)

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

    def test_dated_platform_policy_stays_visible_in_script_and_docs(self):
        script = (ROOT / ".github/scripts/ci_acceptance.py").read_text()
        self.assertIn("2026-09-23", script)
        self.assertIn("#1963", script)
        for doc in ("docs/engineering/operations/actions-capacity.md",
                    "docs/engineering/operations/acceptance-runner.md"):
            text = (ROOT / doc).read_text()
            self.assertIn("2026-09-23", text)
            self.assertIn("#1963", text)


if __name__ == "__main__":
    unittest.main()
