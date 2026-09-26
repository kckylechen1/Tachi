#!/usr/bin/env python3
"""Hermetic tests for .github/scripts/bind_audited_tool.sh (#1998).

Each case builds a fake HOME, CARGO_HOME and RUNNER_TEMP. Files written
before `mark()` play leftovers from earlier jobs; files written after it play
what this run's install step wrote. Stubs print a chosen version line and,
when given a marker, record that they were executed, so a test can prove an
unaudited copy never runs.
"""

from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("bind_audited_tool.sh")
# 2026-07-05, the release date an extracted archive member keeps as its mtime.
ARCHIVE_MTIME = 1783209600


def write_stub(path: Path, version_line: str, executed: Path | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = ["#!/bin/sh"]
    if executed is not None:
        body.append(f"echo executed >> '{executed}'")
    body.append(f"printf '%s\\n' '{version_line}'")
    body.append("printf 'release: stub\\n'")
    path.write_text("\n".join(body) + "\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class BindAuditedToolTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name).resolve()
        self.home = self.root / "home"
        self.prebuilt_dir = self.home / ".install-action" / "bin"
        self.cargo_home = self.root / "cargo-home"
        self.cargo_bin = self.cargo_home / "bin"
        self.path_dir = self.root / "path-bin"
        self.runner_temp = self.root / "runner-temp"
        self.github_env = self.root / "github_env"
        self.github_env.write_text("")
        self.stale_executed = self.root / "stale-executed"
        self.path_dir.mkdir()
        self.runner_temp.mkdir()

    def tearDown(self):
        self._tmp.cleanup()

    def env(self, **overrides: str) -> dict[str, str]:
        env = {
            "HOME": str(self.home),
            "CARGO_HOME": str(self.cargo_home),
            "GITHUB_ENV": str(self.github_env),
            "RUNNER_TEMP": str(self.runner_temp),
            # The shadow directory comes first, as a hostile PATH would.
            "PATH": f"{self.path_dir}:/usr/bin:/bin",
        }
        env.update(overrides)
        return {key: value for key, value in env.items() if value is not None}

    def run_bind(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        return subprocess.run(["/bin/bash", str(SCRIPT), *args], env=self.env(**env),
                              capture_output=True, text=True, check=False)

    def mark(self, binary: str = "cargo-nextest") -> Path:
        """The install marker, strictly after every file written so far."""
        marker = self.runner_temp / f"audited-install-{binary}.marker"
        time.sleep(0.02)
        marker.write_text("")
        time.sleep(0.02)
        return marker

    def add_stale_shadows(self, binary: str, version_line: str) -> None:
        """Leftovers from an earlier job; call before mark()."""
        write_stub(self.cargo_bin / binary, version_line, self.stale_executed)
        write_stub(self.path_dir / binary, version_line, self.stale_executed)

    def bound_value(self, env_var: str) -> str | None:
        for line in self.github_env.read_text().splitlines():
            key, _, value = line.partition("=")
            if key == env_var:
                return value
        return None

    def assert_bound(self, result: subprocess.CompletedProcess, env_var: str, path: Path) -> None:
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bound_value(env_var), str(path))
        self.assertFalse(self.stale_executed.exists(), "an unaudited copy was executed")

    def assert_rejected(self, result: subprocess.CompletedProcess, needle: str) -> None:
        self.assertEqual(result.returncode, 6, result.stdout + result.stderr)
        self.assertIn(needle, result.stderr)
        self.assertEqual(self.github_env.read_text(), "", "a rejected binding must not export anything")
        self.assertFalse(self.stale_executed.exists(), "an unaudited copy was executed")

    # --- binding the fresh install -------------------------------------------------

    def test_binds_audited_nextest_and_ignores_shadows(self):
        self.add_stale_shadows("cargo-nextest", "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)")
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140 (a9fef2964 2026-07-05)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        bound = self.prebuilt_dir / "cargo-nextest"
        self.assert_bound(result, "AUDITED_NEXTEST", bound)
        self.assertIn(f"cargo-nextest bound path: {bound}", result.stdout)
        self.assertIn("cargo-nextest bound version: cargo-nextest 0.9.140 (a9fef2964 2026-07-05)", result.stdout)
        self.assertIn(f"cargo-nextest PATH lookup (bypassed): {self.path_dir / 'cargo-nextest'}", result.stdout)
        self.assertIn(f"cargo-nextest stale copy, not written by this run (bypassed): {self.cargo_bin / 'cargo-nextest'}",
                      result.stdout)

    def test_binds_audited_cargo_audit_subcommand_version_form(self):
        # `cargo-audit audit --version` prints "cargo-audit-audit <version>".
        self.mark("cargo-audit")
        write_stub(self.prebuilt_dir / "cargo-audit", "cargo-audit-audit 0.22.2")
        result = self.run_bind("AUDITED_CARGO_AUDIT", "cargo-audit", "audit", "0.22.2")
        self.assert_bound(result, "AUDITED_CARGO_AUDIT", self.prebuilt_dir / "cargo-audit")

    def test_binds_fresh_file_that_keeps_the_archive_mtime(self):
        # install-action extracts with `tar x` and moves with `mv`: the file keeps
        # the release date as its mtime, older than the marker. Freshness is
        # the ctime, which the extraction sets to now; an mtime test (`-nt`)
        # would reject every genuine install.
        self.mark()
        stub = self.prebuilt_dir / "cargo-nextest"
        write_stub(stub, "cargo-nextest 0.9.140")
        os.utime(stub, (ARCHIVE_MTIME, ARCHIVE_MTIME))
        marker = self.runner_temp / "audited-install-cargo-nextest.marker"
        self.assertLess(stub.stat().st_mtime, marker.stat().st_mtime)
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_bound(result, "AUDITED_NEXTEST", stub)

    def test_binds_fresh_cargo_home_copy_when_install_action_dir_holds_a_leftover(self):
        # astra r1 finding 3: the installer wrote $CARGO_HOME/bin while
        # ~/.install-action/bin kept a same-version copy from an earlier job.
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140", self.stale_executed)
        self.mark()
        write_stub(self.cargo_bin / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_bound(result, "AUDITED_NEXTEST", self.cargo_bin / "cargo-nextest")
        self.assertIn(f"stale copy, not written by this run (bypassed): {self.prebuilt_dir / 'cargo-nextest'}",
                      result.stdout)

    def test_same_directory_named_twice_is_one_candidate(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140",
                               CARGO_HOME=str(self.home / ".install-action"))
        self.assert_bound(result, "AUDITED_NEXTEST", self.prebuilt_dir / "cargo-nextest")

    def test_mark_mode_then_bind(self):
        result = self.run_bind("--mark", "cargo-nextest")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        marker = self.runner_temp / "audited-install-cargo-nextest.marker"
        self.assertTrue(marker.is_file())
        self.assertIn(f"cargo-nextest install marker: {marker}", result.stdout)
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_bound(result, "AUDITED_NEXTEST", self.prebuilt_dir / "cargo-nextest")

    # --- freshness rejections ------------------------------------------------------

    def test_rejects_stale_same_version_leftover_older_than_marker(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140", self.stale_executed)
        self.mark()
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "no cargo-nextest was written after")
        self.assertIn(str(self.prebuilt_dir / "cargo-nextest"), result.stderr)

    def test_rejects_fresh_copies_in_both_locations(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        write_stub(self.cargo_bin / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "in both ~/.install-action/bin and $CARGO_HOME/bin")

    def test_rejects_missing_marker(self):
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "no install marker")

    def test_rejects_marker_of_another_tool(self):
        self.mark("cargo-audit")
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "no install marker")

    def test_rejects_missing_install_even_with_shadows_present(self):
        self.add_stale_shadows("cargo-nextest", "cargo-nextest 0.9.140")
        self.mark()
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "no cargo-nextest was written after")

    def test_requires_runner_temp(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140", RUNNER_TEMP=None)
        self.assert_rejected(result, "RUNNER_TEMP is not set")
        result = self.run_bind("--mark", "cargo-nextest", RUNNER_TEMP=None)
        self.assertEqual(result.returncode, 6)
        self.assertIn("RUNNER_TEMP is not set", result.stderr)

    # --- version and file-shape rejections -----------------------------------------

    def test_rejects_wrong_version(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.138 (fc97e97bb 2026-06-21)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_asserting_a_version_the_file_does_not_have(self):
        # Negative case for a drifted assertion: the real file is 0.9.140.
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140 (a9fef2964 2026-07-05)")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.141")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.141")

    def test_rejects_version_prefix_match(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.1400")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_other_program_name(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-evil 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "not the audited cargo-nextest 0.9.140")

    def test_rejects_symlinked_install(self):
        target = self.root / "elsewhere" / "cargo-nextest"
        write_stub(target, "cargo-nextest 0.9.140")
        self.mark()
        self.prebuilt_dir.mkdir(parents=True)
        os.symlink(target, self.prebuilt_dir / "cargo-nextest")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "is not a regular executable file")

    def test_rejects_failing_version_command(self):
        self.mark()
        stub = self.prebuilt_dir / "cargo-nextest"
        stub.parent.mkdir(parents=True)
        stub.write_text("#!/bin/sh\necho 'cargo-nextest 0.9.140'\nexit 3\n")
        stub.chmod(0o755)
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140")
        self.assert_rejected(result, "--version exited non-zero")

    def test_rejects_bad_arguments(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        cases = {
            ("NEXTEST", "cargo-nextest", "nextest", "0.9.140"): "must be named AUDITED_*",
            ("AUDITED_next", "cargo-nextest", "nextest", "0.9.140"): "must be uppercase letters, digits and _",
            ("AUDITED_NEXTEST", "../cargo-nextest", "nextest", "0.9.140"): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest", "*"): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest", ""): "[[:alnum:]._-] words",
            ("AUDITED_NEXTEST", "cargo-nextest", "nextest"): "usage:",
            ("--mark", "../cargo-nextest"): "[[:alnum:]._-] words",
            ("--mark",): "usage:",
        }
        for args, needle in cases.items():
            with self.subTest(args=args):
                self.assert_rejected(self.run_bind(*args), needle)

    def test_requires_github_env(self):
        self.mark()
        write_stub(self.prebuilt_dir / "cargo-nextest", "cargo-nextest 0.9.140")
        result = self.run_bind("AUDITED_NEXTEST", "cargo-nextest", "nextest", "0.9.140", GITHUB_ENV=None)
        self.assertEqual(result.returncode, 6)
        self.assertIn("GITHUB_ENV is not set", result.stderr)


if __name__ == "__main__":
    unittest.main()
