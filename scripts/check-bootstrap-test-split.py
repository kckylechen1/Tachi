#!/usr/bin/env python3
"""Fail closed if #1714's test split widens or loses its frozen surface."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def fail(message: str) -> None:
    print(f"bootstrap-test-split: FAIL — {message}", file=sys.stderr)
    raise SystemExit(1)


workspace = read("Cargo.toml")
if '"crates/tachi-bootstrap-tests"' not in workspace:
    fail("workspace member crates/tachi-bootstrap-tests is missing")

server_manifest = read("crates/tachi-server/Cargo.toml")
if not re.search(r"(?m)^bootstrap-test-api\s*=\s*\[\s*\]\s*$", server_manifest):
    fail("non-default bootstrap-test-api feature is missing or has dependencies")
default_line = re.search(r'(?m)^default\s*=\s*\[(.*?)\]\s*$', server_manifest)
if default_line and "bootstrap-test-api" in default_line.group(1):
    fail("bootstrap-test-api must not be enabled by default")

lib_rs = read("crates/tachi-server/src/lib.rs")
required_module = (
    '#[cfg(feature = "bootstrap-test-api")]\n'
    "#[doc(hidden)]\n"
    "pub mod bootstrap_test_api;"
)
if required_module not in lib_rs:
    fail("bootstrap_test_api is not gated and doc-hidden exactly as frozen")
for forbidden in (
    "bootstrap",
    "daemon_lock",
    "db_ownership",
    "doctor",
    "manifest",
    "utils",
):
    if re.search(rf"(?m)^pub mod {forbidden};$", lib_rs):
        fail(f"private module {forbidden} was widened")

expected_native_types = {
    "MigrationConfig",
    "SetupItem",
    "SetupReport",
    "TidyAppliedStep",
    "TidyApplySummary",
    "TidyExecuteSummary",
    "TidyFinding",
    "TidyGroupSummary",
    "TidyMigration",
    "TidyMigrationOutcome",
    "TidyPlanStep",
    "TidyReport",
}
native_sources = read("crates/tachi-server/src/bootstrap/mod.rs") + read(
    "crates/tachi-server/src/bootstrap/tidy/migration.rs"
)
actual_native_types = set(re.findall(r"(?m)^pub struct ([A-Za-z0-9_]+)\b", native_sources))
if actual_native_types != expected_native_types:
    fail(
        "native public type census drifted: "
        f"expected={sorted(expected_native_types)} actual={sorted(actual_native_types)}"
    )

facade = read("crates/tachi-server/src/bootstrap_test_api.rs")
if re.search(r"pub use[^;]*PhysicalMutationAuthority", facade, re.DOTALL):
    fail("raw PhysicalMutationAuthority escaped through the facade")

expected_facade_types = {
    "AuthorizedMigrationSources",
    "BootstrapTestLock",
    "DoctorInventoryCounts",
    "OwnershipInjection",
    "OwnershipInjectionGuard",
}
actual_facade_types = set(
    re.findall(r"(?m)^pub (?:struct|enum) ([A-Za-z0-9_]+)\b", facade)
)
if actual_facade_types != expected_facade_types:
    fail(
        "opaque facade type census drifted: "
        f"expected={sorted(expected_facade_types)} actual={sorted(actual_facade_types)}"
    )

expected_facade_functions = {
    "authorized_migration_sources",
    "build_migration_plan",
    "build_setup_report",
    "build_tidy_report",
    "capture_migration_sources",
    "execute_tidy_apply",
    "execute_tidy_migrations",
    "force_boundary_failure_after_archive_stage",
    "hold_outer_target_lock",
    "hold_source_scope_lock",
    "inject_ownership",
    "is_empty",
    "len",
    "scan_doctor_inventory_counts",
    "update_manifest_after_migration",
}
actual_facade_functions = set(re.findall(r"(?m)^\s*pub fn ([A-Za-z0-9_]+)\b", facade))
if actual_facade_functions != expected_facade_functions:
    fail(
        "facade function census drifted: "
        f"expected={sorted(expected_facade_functions)} actual={sorted(actual_facade_functions)}"
    )

old_root = ROOT / "crates/tachi-server/src/tests/bootstrap_tests.rs"
old_dir = ROOT / "crates/tachi-server/src/tests/bootstrap_tests"
if old_root.exists() or old_dir.exists():
    fail("old tachi-server bootstrap test sources still exist")
if re.search(r"(?m)^mod bootstrap_tests;$", read("crates/tachi-server/src/tests/mod.rs")):
    fail("old tachi-server bootstrap test module is still registered")

test_root = ROOT / "crates/tachi-bootstrap-tests/tests"
test_sources = [test_root / "bootstrap_tests.rs", *sorted((test_root / "bootstrap_tests").glob("*.rs"))]
test_names: list[str] = []
for source in test_sources:
    text = source.read_text(encoding="utf-8")
    test_names.extend(re.findall(r"(?m)^fn ([A-Za-z0-9_]+)\(\) \{$", text))
test_names = {
    name
    for name in test_names
    if name.startswith(("tidy_", "migration_", "manifest_", "setup_"))
}
expected_test_names = {
    "manifest_update_drops_migrated_sources_and_inserts_target",
    "migration_plan_targets_only_legacy_dbs_and_skips_keep_actions",
    "setup_report_detects_readiness_from_local_state",
    "tidy_apply_removes_broken_memory_db_symlink",
    "tidy_apply_writes_report_and_only_confirms_safe_actions",
    "tidy_execute_archive_failure_rolls_back_overwritten_target_rows_atomically",
    "tidy_execute_boundary_failure_keeps_live_source_after_archive_staging",
    "tidy_execute_migrates_a_source_containing_an_anchor_row",
    "tidy_execute_migrates_cross_scope_source_while_outer_target_lock_is_held",
    "tidy_execute_migrates_rows_and_archives_source",
    "tidy_execute_never_archives_symlink_target_selected_as_inventory_open_path",
    "tidy_execute_refuses_replaced_source_inode_after_plan_before_source_open",
    "tidy_execute_refuses_source_becoming_target_after_plan",
    "tidy_execute_refuses_symlink_substitution_after_scan_before_source_open",
    "tidy_execute_rolls_back_when_source_db_is_owned",
    "tidy_execute_rolls_back_when_source_db_ownership_is_unknown",
    "tidy_execute_rolls_back_when_source_scope_lock_is_held",
    "tidy_report_counts_physical_db_once_across_path_symlink_and_hardlink",
    "tidy_report_does_not_count_unresolved_paths_as_aliases",
    "tidy_report_prefers_hardlink_alias_with_active_wal_sidecars",
    "tidy_report_preserves_open_error_and_adds_typed_failure",
    "tidy_report_reads_committed_wal_while_writer_owns_database",
    "tidy_report_refuses_mutation_authority_for_multiple_live_hardlink_sidecars",
    "tidy_report_scans_memory_dbs_and_suggests_scope",
}
if test_names != expected_test_names:
    fail(
        "moved test-name census drifted: "
        f"expected={sorted(expected_test_names)} actual={sorted(test_names)}"
    )


def package_vector(relative: str, marker: str) -> set[str]:
    line = next((line for line in read(relative).splitlines() if marker in line), None)
    if line is None:
        fail(f"could not find package-vector marker {marker} in {relative}")
    return set(re.findall(r"-p\s+([A-Za-z0-9_-]+)", line))


census_packages = package_vector("scripts/nextest-census.sh", "nextest_args=(nextest run")
known_red_packages = package_vector(
    "scripts/nextest-known-reds-diff.sh", "NEXTEST_PACKAGES=("
)
if census_packages != known_red_packages:
    fail(
        "nextest package rosters differ: "
        f"census={sorted(census_packages)} known_reds={sorted(known_red_packages)}"
    )
if "tachi-bootstrap-tests" not in census_packages:
    fail("tachi-bootstrap-tests is absent from the nextest package rosters")

print("bootstrap-test-split: passed (24 moved tests; bounded feature/API/package census)")
