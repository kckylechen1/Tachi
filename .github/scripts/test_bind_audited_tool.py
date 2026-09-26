#!/usr/bin/env python3
"""Hermetic tests for .github/scripts/bind_audited_tool.sh (#1998).

Each case builds a fake HOME whose ~/.install-action/bin holds a stub tool
that prints a chosen version line, plus an optional shadow copy in
CARGO_HOME/bin and on PATH, then runs the script as a workflow step would.
"""

from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("bind_audited_tool.sh")


def write_stub(path: Path, version_line: str, marker: Path | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = ["#!/bin/sh"]
    if marker is not None:
        body.append(f"echo executed >> '{marker}'")
    body.append(f"printf '%s\\n' '{version_line}'")
    body.append("printf 'release: stub\\n'")
    path.write_text("\n".join(body) + "\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class BindAuditedToolTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.home = self.root / "home"
        self.prebuilt_dir = self.home / ".install-action" / "bin"
        self.cargo_home = self.root / "cargo-home"
        self.path_dir = self.root / "path-bin"
        self.github_env = self.root / "github_env"
        self.github_env.write_text("")
        self.shadow_marker = self.root / "shadow-executed"
        self.path_dir.mkdir()

    def tearDown(self):
        self._tmp.cleanup()

    def run_bind(self, *args: str) -> subprocess.CompletedProcess:
        env = {
            "HOME": str(self.home),
            "CARGO_HOME": str(self.cargo_home),
            "GITHUB_ENV": str(self.github_env),
            # The shadow directory comes first, as a hostile PATH would.
            "PATH": f"{self.path_dir}:/usr/bin:/bin",
        }
        return subprocess.run(["/bin/bash", str(SCRIPT), *args], env=env,
                              capture_output=True, text=True, check=False)

    def add_shadows(self, binary: str, version_line: str) -> None:
        write_stub(self.cargo_home / "bin" / binary, version_line, self.shadow_marker)
        write_stub(self.path_dir / binary, version_line, self.shadow_marker)

    def bound_value(self, env_var: str) -> str | None:
        for line in self.github_env.read_text().splitlines():
            key, _, value = line.partition("=")
            if key == env_var:
                return value
        return None

    def assert_rejected(self, result: subprocess.CompletedProcess, needle: str) -> None:
        self.assertEqual(result.returncode, 6, result.stdout + result.stderr)
        self.assertIn(needle, result.stderr)
        self.assertEqual(self.github_env.read_text(), "", "a rejected binding must not export anything")

    def test_binds_audited_nextest_and_ignores_shadows(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140 (a9fef2964 2026-07-05)")
        self.add_shadows("cargo-nextest", "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bound = str(self.prebuilt_dir.resolve() / "cargo-nextest")
        self.assertEqual(self.bound_value("AUDITED_NEXTEST"), bound)
        self.assertIn(f"cargo-nextest bound path: {bound}", result.stdout)
        self.assertIn("cargo-nextest bound version: cargo-nextest 0.9.140 (a9fef2964 2026-07-05)", result.stdout)
        self.assertIn(f"cargo-nextest PATH lookup (bypassed): {self.path_dir / 'cargo-nextest'}", result.stdout)
        self.assertIn(f"cargo-nextest CARGO_HOME/bin copy (bypassed): {self.cargo_home / 'bin' / 'cargo-nextest'}",
                      result.stdout)
        self.assertFalse(self.shadow_marker.exists(), "an unaudited shadow binary was executed")

    def test_binds_audited_cargo_audit_subcommand_version_form(self):
        # `cargo-audit audit --version` prints "cargo-audit-audit <version>".
        write_stub(self.prebuilt_dir / "cargo-audit", "cargo-audit-audit 0.22.2")
        result = self.run_bind("AUDITED_CARGO_AUDIT", "cargo-audit", "audit", "0.22.2")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bound_value("AUDITED_CARGO_AUDIT"), str(self.prebuilt_dir.resolve() / "cargo-audit"))

    def test_rejects_wrong_version(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_asserting_a_version_the_file_does_not_have(self):
        # Negative case for a drifted assertion: the real file is 0.9.140.
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140 (a9fef2964 2026-07-05)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.141")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.141")

    def test_rejects_version_prefix_match(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.1400")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_other_program_name(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-evil 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_missing_install_even_with_shadows_present(self):
        self.add_shadows("cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "is not a regular executable file")
        self.assertFalse(self.shadow_marker.exists())

    def test_rejects_symlinked_install(self):
        target = self.root / "elsewhere" / "cargo-nextest"
        write_stub(target, "cargo-nextest 0.9.140")
        self.prebuilt_dir.mkdir(parents=True)
        os.symlink(target, self.prebuilt_dir / "cargo-nextest")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "is not a regular executable file")

    def test_rejects_failing_version_command(self):
        stub = self.prebuilt_dir / "cargo-nextest"
        stub.parent.mkdir(parents=True)
        stub.write_text("#!/bin/sh\necho 'cargo-nextest 0.9.140'\nexit 3\n")
        stub.chmod(0o755)
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "--version exited non-zero")

    def test_rejects_bad_arguments(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        cases = {
            ("NEXTEST", "cargo-nextest", "nextest", "0.9.140"): "must be named AUDITED_*",
            ("AUDITED_next", "cargo-nextest", "nextest", "0.9.140"): "must be uppercase letters, digits and _",
            ("AUDITED_NEXTEST", "../cargo-nextest", "nextest", "0.9.140"): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest", "*"): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest", ""): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest"): "usage:",
        }
        for args, needle in cases.items():
            with self.subTest(args=args):
                self.assert_rejected(self.run_bind(*args), needle)

    def test_requires_github_env(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        env = {"HOME": str(self.home), "PATH": "/usr/bin:/bin"}
        result = subprocess.run(["/bin/bash", str(SCRIPT), "AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140"],
                                env=env, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 6)
        self.assertIn("GITHUB_ENV is not set", result.stderr)


if __name__ == "__main__":
    unittest.main()
