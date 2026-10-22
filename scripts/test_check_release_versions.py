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
            "README.zh-CN.md",
            "README.classical.md",
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
        with release_fixture(rename_manifest="crates/tachi-llm/Cargo.toml"):
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

    def test_toml_comments_single_quotes_and_multiline_strings(self) -> None:
        inline_comment = '[package] # release package\nname = "memcore"\nversion = "1.9.0"\n'
        found, fields = checker._parse_toml_package_fields(inline_comment)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "1.9.0"))

        single_quotes = "[package]\nname = 'memcore'\nversion = '1.9.0'\n"
        found, fields = checker._parse_toml_package_fields(single_quotes)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "1.9.0"))

        multiline_literal = (
            '[package]\nname = "memcore"\nversion = "9.9.9"\n'
            "description = '''\nversion = \"1.9.0\"\n'''\n"
        )
        found, fields = checker._parse_toml_package_fields(multiline_literal)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "9.9.9"))

        multiline_basic = (
            '[package]\nname = "memcore"\nversion = "9.9.9"\n'
            'description = """\nversion = "1.9.0"\n"""\n'
        )
        found, fields = checker._parse_toml_package_fields(multiline_basic)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "9.9.9"))

        comment_quote = (
            '[package]\nname = "memcore"\nkeywords = [ # "unclosed in comment\n    "database",\n]\nversion = "2.0.0"\n'
        )
        found, fields = checker._parse_toml_package_fields(comment_quote)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "2.0.0"))

        comment_apostrophe = (
            "[package]\nname = \"memcore\"\nkeywords = [ # don't reorder\n    \"database\",\n]\nversion = \"2.0.0\"\n"
        )
        found, fields = checker._parse_toml_package_fields(comment_apostrophe)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "2.0.0"))

        escaped_multiline_delim = (
            '[package]\nname = "memcore"\ndescription = """Example: \\""" [example]\ntext\n"""\nversion = "2.0.0"\n'
        )
        found, fields = checker._parse_toml_package_fields(escaped_multiline_delim)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "2.0.0"))

        escaped_multiline_mismatch = (
            '[package]\nname = "memcore"\nversion = "9.9.9"\ndescription = """Example: \\""" version = "2.0.0"\n"""\n'
        )
        found, fields = checker._parse_toml_package_fields(escaped_multiline_mismatch)
        self.assertTrue(found)
        self.assertEqual((fields.get("name"), fields.get("version")), ("memcore", "9.9.9"))


    def test_fallback_decodes_quoted_keys_and_unicode_without_weakening_mismatch(self):
        previous = checker.tomllib
        checker.tomllib = None
        try:
            for header in ['["package"]', "['package']", r'["pack\u0061ge"]']:
                with self.subTest(header=header), release_fixture() as root:
                    manifest = root / RUST_MANIFEST_PATHS[0]
                    manifest.write_text(header + '\n"name" = "memcore"\n"version" = "\\u0031.9.0"\n')
                    self.assertEqual(checker.main(), 0)
                    manifest.write_text(header + '\nname = "memcore"\nversion = "\\U00000039.9.9"\n')
                    with contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(checker.main(), 1)
            for body in [
                '[package]\nname="memcore"\nversion="\\uD800"\n',
                '[package]\nname="memcore"\nversion="\\q"\n',
                '[package]\nname="memcore"\nversion="1.9.0"\nversion="9.9.9"\n',
                '[package]\nname="memcore"\nversion="1.9.0',
            ]:
                with self.subTest(body=body):
                    with self.assertRaises(ValueError):
                        checker._parse_toml_package_fields(body)
        finally:
            checker.tomllib = previous

    def test_lock_ignores_same_named_external_packages_in_both_orders(self):
        external = '\n[[package]]\nname = "memcore"\nversion = "9.9.9"\nsource = "registry+https://example.invalid"\n'
        for first in [True, False]:
            with self.subTest(first=first), release_fixture() as root:
                lock = root / 'Cargo.lock'
                original = lock.read_text()
                lock.write_text(external + original if first else original + external)
                self.assertEqual(checker.main(), 0)
            with release_fixture(lock_version='9.9.9') as root:
                lock = root / 'Cargo.lock'
                lock.write_text(lock.read_text() + external.replace('9.9.9', VERSION))
                with contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(checker.main(), 1)


if __name__ == "__main__":
    unittest.main()
