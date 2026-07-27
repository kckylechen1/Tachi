#!/usr/bin/env python3
"""Synthetic discrimination tests for the lock-census validator (#1476).

Each test feeds the pure ``validate_census`` function a synthetic fixture and
a synthetic live-callsite list, then asserts the expected error surface.
The committed live fixture is never rewritten here.
"""

import unittest

from lock_census import validate_census, VALID_ENV_ROLES, VALID_DELETION_SCOPES


def _base_callsite(file: str, line: int, **kw) -> dict:
    """A minimal valid callsite entry."""
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
    """Build a fixture with internally consistent stats."""
    from collections import Counter
    per_role = Counter(c["env_role"] for c in callsites)
    per_del = Counter(c["deletion_scope"] for c in callsites)
    return {
        "schema_version": "2",
        "callsites": callsites,
        "stats": {
            "total_callsites": len(callsites),
            "per_env_role": dict(sorted(per_role.items())),
            "per_deletion_scope": dict(sorted(per_del.items())),
        },
    }


class ValidateCensusGreen(unittest.TestCase):
    """The live committed fixture must be accepted."""

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
        """A live callsite absent from the fixture."""
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 99)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("missing" in e and "b.rs:99" in e for e in errors),
                        f"expected missing error, got: {errors}")

    def test_stale_callsite_is_red(self):
        """A fixture entry absent from live code."""
        callsites = [
            _base_callsite("crates/foo/a.rs", 10),
            _base_callsite("crates/foo/old.rs", 50),
        ]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("stale" in e and "old.rs:50" in e for e in errors),
                        f"expected stale error, got: {errors}")

    def test_duplicate_identity_is_red(self):
        """Same (file, line) appearing twice."""
        callsites = [
            _base_callsite("crates/foo/a.rs", 10),
            _base_callsite("crates/foo/a.rs", 10),  # duplicate
        ]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("duplicate" in e and "a.rs:10" in e for e in errors),
                        f"expected duplicate error, got: {errors}")

    def test_invalid_env_role_enum_is_red(self):
        """An env_role outside the allowed set."""
        callsites = [_base_callsite("crates/foo/a.rs", 10, env_role="totally_migratable")]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("invalid env_role" in e for e in errors),
                        f"expected invalid enum error, got: {errors}")

    def test_invalid_deletion_scope_enum_is_red(self):
        """A deletion_scope outside the allowed set."""
        callsites = [_base_callsite("crates/foo/a.rs", 10, deletion_scope="maybe")]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("invalid deletion_scope" in e for e in errors),
                        f"expected invalid enum error, got: {errors}")

    def test_total_count_drift_is_red(self):
        """stats.total_callsites disagreeing with len(callsites)."""
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        fixture["stats"]["total_callsites"] = 999  # wrong
        live = [("crates/foo/a.rs", 10)]
        errors = validate_census(fixture, live)
        self.assertTrue(any("total_callsites" in e and "count drift" in e for e in errors),
                        f"expected count drift error, got: {errors}")

    def test_per_env_role_count_drift_is_red(self):
        """stats.per_env_role disagreeing with actual entry counts."""
        callsites = [
            _base_callsite("crates/foo/a.rs", 10, env_role="behavior_under_test"),
            _base_callsite("crates/foo/b.rs", 20, env_role="behavior_under_test"),
        ]
        fixture = _base_fixture(callsites)
        fixture["stats"]["per_env_role"]["behavior_under_test"] = 5  # wrong
        live = [("crates/foo/a.rs", 10), ("crates/foo/b.rs", 20)]
        errors = validate_census(fixture, live)
        self.assertTrue(
            any("per_env_role.behavior_under_test" in e and "count drift" in e for e in errors),
            f"expected per-role count drift error, got: {errors}",
        )

    def test_line_drift_is_red(self):
        """A callsite whose line number shifted in live code is caught as stale+missing."""
        callsites = [_base_callsite("crates/foo/a.rs", 10)]
        fixture = _base_fixture(callsites)
        live = [("crates/foo/a.rs", 11)]  # line drifted by 1
        errors = validate_census(fixture, live)
        has_stale = any("stale" in e and "a.rs:10" in e for e in errors)
        has_missing = any("missing" in e and "a.rs:11" in e for e in errors)
        self.assertTrue(has_stale and has_missing,
                        f"expected both stale and missing on line drift, got: {errors}")


class EnumCoverage(unittest.TestCase):
    """Confirm the validator accepts every documented enum value."""

    def test_all_env_roles_accepted(self):
        for role in VALID_ENV_ROLES:
            callsites = [_base_callsite("crates/foo/a.rs", 10, env_role=role)]
            fixture = _base_fixture(callsites)
            live = [("crates/foo/a.rs", 10)]
            errors = validate_census(fixture, live)
            enum_errors = [e for e in errors if "invalid env_role" in e]
            self.assertEqual(enum_errors, [], f"role {role} should be valid, got: {enum_errors}")

    def test_all_deletion_scopes_accepted(self):
        for scope in VALID_DELETION_SCOPES:
            callsites = [_base_callsite("crates/foo/a.rs", 10, deletion_scope=scope)]
            fixture = _base_fixture(callsites)
            live = [("crates/foo/a.rs", 10)]
            errors = validate_census(fixture, live)
            enum_errors = [e for e in errors if "invalid deletion_scope" in e]
            self.assertEqual(enum_errors, [], f"scope {scope} should be valid, got: {enum_errors}")


if __name__ == "__main__":
    unittest.main()
