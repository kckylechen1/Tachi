"""Regression tests for the release-version inventory checker."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_release_versions.py")
SPEC = importlib.util.spec_from_file_location("check_release_versions", SCRIPT)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


RUST_MANIFEST_PATHS = checker.RUST_MANIFEST_PATHS
JS_PACKAGE_PATHS = checker.JS_PACKAGE_PATHS
VERSION = "1.9.0"


@contextlib.contextmanager
def release_fixture(rename_manifest: str | None = None, lock_version: str = VERSION):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        package_names: list[str] = []
        for path in RUST_MANIFEST_PATHS:
            name = Path(path).parent.name
            if path == "crates/tachi-server/Cargo.toml":
                name = "tachi-server"
            if path == rename_manifest:
                name = "renamed-release-crate"
            package_names.append(name)
            manifest = root / path
            manifest.parent.mkdir(parents=True, exist_ok=True)
            manifest.write_text(
                f'[package]\nname = "{name}"\nversion = "{VERSION}"\n', encoding="utf-8"
            )

        (root / "Cargo.lock").write_text(
            "\n".join(
                f'[[package]]\nname = "{name}"\nversion = "{lock_version}"\n'
                for name in package_names
            ),
            encoding="utf-8",
        )
        for path in JS_PACKAGE_PATHS:
            package = root / path
            package.parent.mkdir(parents=True, exist_ok=True)
            package.write_text(json.dumps({"version": VERSION}), encoding="utf-8")
            package.with_name("package-lock.json").write_text(
                json.dumps({"version": VERSION, "packages": {"": {"version": VERSION}}}),
                encoding="utf-8",
            )
        (root / "docs").mkdir()
        (root / "docs/current-state.agent.yaml").write_text(
            f"release: v{VERSION}\n", encoding="utf-8"
        )
        for path in [
            "README.md",
            "scripts/install.sh",
            "docs/INSTALL.md",
            "integrations/openclaw/README.md",
        ]:
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(f"/{'v' + VERSION}/scripts/install\n", encoding="utf-8")

        previous_root = checker.ROOT
        checker.ROOT = root
        try:
            yield root
        finally:
            checker.ROOT = previous_root


class ReleaseVersionInventoryTests(unittest.TestCase):
    def test_lock_package_names_follow_the_manifest_inventory(self) -> None:
        with release_fixture(rename_manifest="crates/tachi-merge-ops/Cargo.toml"):
            self.assertEqual(checker.main(), 0)

    def test_lock_version_mismatch_stays_loud(self) -> None:
        stderr = io.StringIO()
        with release_fixture(lock_version="9.9.9"), contextlib.redirect_stderr(stderr):
            self.assertEqual(checker.main(), 1)
        self.assertIn(
            "Cargo.lock memcore: expected 1.9.0, got 9.9.9",
            stderr.getvalue(),
        )

    def test_duplicate_or_missing_cargo_names_fail_loudly(self) -> None:
        cases = (
            (
                '[package]\nname = "memcore"\nversion = "1.9.0"\n',
                "Cargo manifests: duplicate package name(s): memcore",
            ),
            (
                '[package]\nversion = "1.9.0"\n',
                "crates/memory-server-capture-gate/Cargo.toml: missing package.name",
            ),
        )
        for manifest_body, expected_error in cases:
            with self.subTest(expected_error=expected_error), release_fixture() as root:
                (root / RUST_MANIFEST_PATHS[1]).write_text(manifest_body, encoding="utf-8")
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    self.assertEqual(checker.main(), 1)
                self.assertIn(expected_error, stderr.getvalue())

    def test_package_lock_path_is_derived_from_its_package_path(self) -> None:
        self.assertEqual(
            checker.package_lock_path("nested/tool/package.json"),
            "nested/tool/package-lock.json",
        )


if __name__ == "__main__":
    unittest.main()
