#!/usr/bin/env python3
"""Discrimination and integration tests for the lock-census validator (#1476).

Run from the repo root:
    python3 -m unittest scripts.test_lock_census
or:
    python3 scripts/test_lock_census.py

Covers:
  - synthetic red/green surface for validate_census (stale/missing/duplicate/
    invalid-enum/count-drift including per_deletion_scope);
  - classifier goldens proving bare make_server does NOT imply
    behavior_under_test while true from_env/env-report functions do;
  - temp Rust source discovery + classification;
  - historical exact-function mapping (never nearest-line cross-fn);
  - integration: real discovery against the committed live fixture.
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

# Make the script importable from repo root AND from scripts/.
SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from lock_census import (  # noqa: E402
    VALID_DELETION_SCOPES,
    VALID_ENV_ROLES,
    classify_env_role,
    deletion_scope_for,
    discover_raw_hits,
    classify_raw_hits,
    extract_env_vars,
    find_enclosing_function,
    match_historical,
    validate_census,
)

REPO_ROOT = SCRIPTS_DIR.parent
COMMITTED_FIXTURE = REPO_ROOT / "docs/engineering/receipts/1096-leaf0-global-test-lock-baseline.json"


# -- helpers ----------------------------------------------------------------

def _base_callsite(file: str, line: int, **kw) -> dict:
    entry = {
        "file": file,
        "line": line,
        "test_or_fn_name": "dummy_fn",
        "class": "class2_runtime_config",
        "evidence": "structural inspection",
        "env_vars_touched": [],
        "env_role": "unknown",
        "env_role_evidence": "none found",
        "deletion_scope": "none",
    }
    entry.update(kw)
    return entry


def _base_fixture(callsites: list[dict]) -> dict:
    from collections import Counter
    per_role = Counter(c["env_role"] for c in callsites)
    per_del = Counter(c["deletion_scope"] for c in callsites)
    return {
        "schema_version": "3",
        "callsites": callsites,
        "stats": {
            "total_callsites": len(callsites),
            "per_env_role": dict(sorted(per_role.items())),
            "per_deletion_scope": dict(sorted(per_del.items())),
        },
    }


# -- synthetic red/green surface -------------------------------------------

class ValidateCensusGreen(unittest.TestCase):
    """The exact-match fixture must be accepted."""

    def test_exact_match_is_green(self):
        callsites = [
            _base_callsite("crates/foo/a.rs", 10),
            _base_callsite("crates/foo/b.rs", 20, env_role="behavior_under_test"),
            _base_callsite("crates/foo/c.rs", 30, env_role="incidental_delivery",
                           deletion_scope="1319"),
        ]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 20), ("crates/foo/c.rs", 30)]
        errors = validate_census(fixture, live)
        self.assertEqual(errors, [], f"expected no errors, got: {errors}")


class ValidateCensusRed(unittest.TestCase):
    """Each drift class must be caught."""

    def test_missing_callsite_is_red(self):
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 99)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("missing" in e and "b.rs:99" in e for e in errors),
                        f"expected missing, got: {errors}")

    def test_stale_callsite_is_red(self):
        callsites = [
            _base_callsite("crates/foo/a.rs", 10),
            _base_callsite("crates/foo/old.rs", 50),
        ]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("stale" in e and "old.rs:50" in e for e in errors),
                        f"expected stale, got: {errors}")

    def test_duplicate_identity_is_red(self):
        callsites = [
            _base_callsite("crates/foo/a.rs", 10),
            _base_callsite("crates/foo/a.rs", 10),
        ]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("duplicate" in e for e in errors),
                        f"expected duplicate, got: {errors}")

    def test_invalid_env_role_is_red(self):
        callsites = [_base_callsite("crates/foo/a.rs", 10, env_role="totally_migratable")]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("invalid env_role" in e for e in errors),
                        f"expected invalid enum, got: {errors}")

    def test_invalid_deletion_scope_is_red(self):
        callsites = [_base_callsite("crates/foo/a.rs", 10, deletion_scope="maybe")]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("invalid deletion_scope" in e for e in errors),
                        f"expected invalid enum, got: {errors}")

    def test_total_count_drift_is_red(self):
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        fixture["stats"]["total_callsites"] = 999
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("total_callsites" in e and "count drift" in e for e in errors),
                        f"expected count drift, got: {errors}")

    def test_per_env_role_count_drift_is_red(self):
        callsites = [
            _base_callsite("crates/foo/a.rs", 10, env_role="behavior_under_test"),
            _base_callsite("crates/foo/b.rs", 20, env_role="behavior_under_test"),
        ]
        fixture = _base_fixture(callsites)
        fixture["stats"]["per_env_role"]["behavior_under_test"] = 5
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 20)]
        errors = validate_census(fixture, live)
        self.assertTrue(
            any("per_env_role.behavior_under_test" in e for e in errors),
            f"expected per-role drift, got: {errors}",
        )

    def test_per_deletion_scope_count_drift_is_red(self):
        callsites = [
            _base_callsite("crates/foo/a.rs", 10, deletion_scope="1319"),
            _base_callsite("crates/foo/b.rs", 20, deletion_scope="1319"),
        ]
        fixture = _base_fixture(callsites)
        fixture["stats"]["per_deletion_scope"]["1319"] = 99
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 20)]
        errors = validate_census(fixture, live)
        self.assertTrue(
            any("per_deletion_scope.1319" in e for e in errors),
            f"expected per-deletion-scope drift, got: {errors}",
        )

    def test_line_drift_is_red(self):
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 11)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("stale" in e and "a.rs:10" in e for e in errors))
        self.assertTrue(any("missing" in e and "a.rs:11" in e for e in errors))


class EnumCoverage(unittest.TestCase):
    def test_all_env_roles_accepted(self):
        for role in VALID_ENV_ROLES:
            cs = [_base_callsite("crates/foo/a.rs", 10, env_role=role)]
            errors = validate_census(_base_fixture(cs), [("crates/foo/a.rs", 10)])
            self.assertEqual(
                [e for e in errors if "invalid env_role" in e], [],
                f"role {role} should be valid",
            )

    def test_all_deletion_scopes_accepted(self):
        for scope in VALID_DELETION_SCOPES:
            cs = [_base_callsite("crates/foo/a.rs", 10, deletion_scope=scope)]
            errors = validate_census(_base_fixture(cs), [("crates/foo/a.rs", 10)])
            self.assertEqual(
                [e for e in errors if "invalid deletion_scope" in e], [],
                f"scope {scope} should be valid",
            )


# -- classifier goldens (Finding 2) ----------------------------------------

class ClassifierGoldens(unittest.TestCase):
    """Bare make_server must NOT imply behavior_under_test; true env-reading
    constructors must."""

    def test_bare_make_server_does_not_imply_behavior_under_test(self):
        body = [
            "fn some_test() {",
            "    let _guard = crate::utils::global_test_lock().lock();",
            "    let server = make_server();",
            "    let _voyage = EnvRestore::set(\"VOYAGE_API_KEY\", \"secret\");",
            "    assert_eq!(server.something(), 42);",
            "}",
        ]
        role, evidence = classify_env_role(body)
        self.assertNotEqual(role, "behavior_under_test",
                            f"make_server must not imply behavior_under_test; got: {evidence}")

    def test_from_env_implies_behavior_under_test(self):
        body = [
            "fn test_default() {",
            "    let _guard = crate::test_support::global_test_lock().lock();",
            "    let cfg = RerankConfig::from_env().expect(\"default\");",
            "    assert_eq!(cfg.provider, RerankProviderKind::Voyage);",
            "}",
        ]
        role, evidence = classify_env_role(body)
        self.assertEqual(role, "behavior_under_test",
                         f"from_env must imply behavior_under_test; got: {evidence}")

    def test_model_lanes_json_implies_behavior_under_test(self):
        body = [
            "fn test_model_lanes() {",
            "    let _guard = global_test_lock().lock();",
            "    let lanes = model_lanes_json();",
            "    assert_eq!(lanes[\"rerank\"][\"provider\"], \"voyage\");",
            "}",
        ]
        role, _ = classify_env_role(body)
        self.assertEqual(role, "behavior_under_test")

    def test_new_with_config_implies_incidental_delivery(self):
        body = [
            "fn test_wire_shape() {",
            "    let _guard = global_test_lock().lock();",
            "    let config = ProviderRuntimeConfig { rerank: RerankConfig { ... } };",
            "    let client = LlmClient::new_with_config(config, None);",
            "    let out = client.rerank(\"q\", &docs, 2).await;",
            "}",
        ]
        role, _ = classify_env_role(body)
        self.assertEqual(role, "incidental_delivery")

    def test_no_constructors_yield_unknown(self):
        body = [
            "fn test_something() {",
            "    let _guard = global_test_lock().lock();",
            "    let _env = EnvRestore::set(\"SOME_VAR\", \"value\");",
            "    assert_eq!(compute_something(), 42);",
            "}",
        ]
        role, _ = classify_env_role(body)
        self.assertEqual(role, "unknown")


# -- temp Rust source discovery + classification (Finding 3) ---------------

class TempRustSourceDiscovery(unittest.TestCase):
    """Create synthetic .rs files in a temp dir and test find_enclosing_function
    + classify_env_role + extract_env_vars end-to-end."""

    def test_discovery_and_classification_from_temp_source(self):
        rust_src = """\
use crate::test_support::global_test_lock;

fn helper_make_server() -> i32 {
    let _guard = global_test_lock().lock();
    let _x = EnvRestore::set_path("TACHI_HOME", "/tmp/foo");
    42
}

fn test_parses_env() {
    let _guard = global_test_lock().lock();
    let cfg = RerankConfig::from_env().expect("cfg");
    assert_eq!(cfg.provider, Voyage);
}
"""
        with tempfile.NamedTemporaryFile(suffix=".rs", mode="w", delete=False) as f:
            f.write(rust_src)
            f.flush()
            path = Path(f.name)
        try:
            # Line 4 has a callsite in helper_make_server
            fn_name, body = find_enclosing_function(path, 4)
            self.assertEqual(fn_name, "helper_make_server")
            role, _ = classify_env_role(body)
            # helper_make_server has no env-reading or injection constructor
            self.assertEqual(role, "unknown")
            # set_path should be captured
            env_vars = extract_env_vars(body)
            self.assertIn("TACHI_HOME", env_vars)

            # Line 10 has a callsite in test_parses_env
            fn_name2, body2 = find_enclosing_function(path, 10)
            self.assertEqual(fn_name2, "test_parses_env")
            role2, _ = classify_env_role(body2)
            self.assertEqual(role2, "behavior_under_test")
        finally:
            path.unlink(missing_ok=True)


# -- historical exact-function mapping (Finding 1) -------------------------

class HistoricalMapping(unittest.TestCase):
    """match_historical must match by (file, fn_name) first, never nearest-line
    cross-function."""

    def test_matches_by_file_and_fn_name(self):
        historical = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "my_test",
                 "evidence": "hand-audited evidence", "class": "class2_runtime_config"},
            ]
        }
        callsite = {"file": "crates/foo/a.rs", "line": 55}  # line drifted by 5
        matched, label = match_historical(callsite, "my_test", [historical])
        self.assertIsNotNone(matched)
        self.assertEqual(matched["evidence"], "hand-audited evidence")

    def test_does_not_match_different_function_at_same_line(self):
        historical = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "other_fn",
                 "evidence": "WRONG evidence"},
            ]
        }
        callsite = {"file": "crates/foo/a.rs", "line": 50}  # exact same line
        matched, _ = match_historical(callsite, "my_test", [historical])
        # Must NOT match: different function name even at same line
        self.assertIsNone(matched)

    def test_returns_none_when_no_match(self):
        historical = {"callsites": []}
        callsite = {"file": "crates/foo/a.rs", "line": 10}
        matched, _ = match_historical(callsite, "my_test", [historical])
        self.assertIsNone(matched)

    def test_archive_richer_evidence_beats_prior_placeholder(self):
        """When committed-prior has a structural placeholder for the same
        (file, fn_name) but the archive has hand-audited evidence, the archive
        evidence must win."""
        prior = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "my_test",
                 "evidence": "structural inspection of my_test", "class": "class2_runtime_config"},
            ]
        }
        archive = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 48, "test_or_fn_name": "my_test",
                 "evidence": "owner-audited: verifies from_env parsing of VOYAGE_API_KEY",
                 "class": "class2_runtime_config"},
            ]
        }
        callsite = {"file": "crates/foo/a.rs", "line": 50}
        matched, label = match_historical(callsite, "my_test", [prior, archive])
        self.assertIsNotNone(matched)
        self.assertEqual(label, "archive_fallback",
                         "archive must win over prior placeholder")
        self.assertIn("owner-audited", matched["evidence"])

    def test_prior_manual_evidence_beats_archive(self):
        """When committed-prior has non-placeholder (manually edited) evidence
        for the same (file, fn_name), it must be preferred over the archive
        even if the archive also has evidence."""
        prior = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "my_test",
                 "evidence": "manually reviewed in PR #999: confirms drift detection",
                 "class": "class2_runtime_config"},
            ]
        }
        archive = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 48, "test_or_fn_name": "my_test",
                 "evidence": "original hand-audited evidence",
                 "class": "class2_runtime_config"},
            ]
        }
        callsite = {"file": "crates/foo/a.rs", "line": 50}
        matched, label = match_historical(callsite, "my_test", [prior, archive])
        self.assertIsNotNone(matched)
        self.assertEqual(label, "committed_prior",
                         "prior manual evidence must win over archive")
        self.assertIn("manually reviewed", matched["evidence"])

    def test_cross_function_invariant_under_evidence_preference(self):
        """The evidence-preference logic must never attach a DIFFERENT
        function's evidence, even when that function has richer evidence."""
        prior = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "other_fn",
                 "evidence": "manually reviewed", "class": "class2_runtime_config"},
            ]
        }
        archive = {
            "callsites": [
                {"file": "crates/foo/a.rs", "line": 50, "test_or_fn_name": "other_fn",
                 "evidence": "owner-audited", "class": "class2_runtime_config"},
            ]
        }
        callsite = {"file": "crates/foo/a.rs", "line": 50}
        # Looking for my_test but both sources only have other_fn — no match.
        matched, _ = match_historical(callsite, "my_test", [prior, archive])
        self.assertIsNone(matched,
                          "must not match other_fn evidence to my_test callsite")


# -- deletion_scope narrowing (Finding 4) ----------------------------------

class DeletionScopeNarrowing(unittest.TestCase):
    """Only Shell/Arena/Task-dispatch-test paths are 1319; surviving dispatch
    kernel, prompt, and bootstrap/serve must NOT be blanket-marked."""

    def test_shell_ops_is_1319(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/shell_ops/tests.rs"), "1319")

    def test_arena_ops_is_1319(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/arena_ops/tests/harness.rs"), "1319")

    def test_dispatch_tests_dir_is_1319(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/tests/dispatch_tests/board_first.rs"), "1319")

    def test_dispatch_kernel_survives(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/dispatch_ops/dispatch/tests.rs"), "none")

    def test_dispatch_prompt_survives(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/dispatch_ops/prompt.rs"), "none")

    def test_bootstrap_serve_survives(self):
        self.assertEqual(deletion_scope_for("crates/tachi-server/src/bootstrap/serve.rs"), "none")


# -- integration: real discovery against committed fixture (Finding 3) -----

class IntegrationAgainstCommittedFixture(unittest.TestCase):
    """The committed fixture must validate against the live repo."""

    def test_committed_fixture_validates_green(self):
        if not COMMITTED_FIXTURE.exists():
            self.skipTest("committed fixture not found")
        fixture = json.loads(COMMITTED_FIXTURE.read_text(encoding="utf-8"))
        hits = discover_raw_hits()
        _, _, _, raw = classify_raw_hits(hits)
        live = [(c["file"], c["line"]) for c in raw]
        errors = validate_census(fixture, live)
        self.assertEqual(errors, [], f"committed fixture has {len(errors)} errors; first 5: {errors[:5]}")

    def test_stale_synthetic_fixture_remains_red(self):
        """A fixture with a deliberately wrong line stays red even when the
        validator is given the real live set."""
        live = [("crates/totally/fake.rs", 99999)]
        fixture = _base_fixture([_base_callsite("crates/totally/fake.rs", 1)])
        errors = validate_census(fixture, live)
        self.assertTrue(len(errors) > 0, "stale fixture must be red")


if __name__ == "__main__":
    unittest.main()
