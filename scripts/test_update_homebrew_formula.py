#!/usr/bin/env python3
"""Discrimination tests for public-binary Homebrew formula updates (#728/#874)."""

from __future__ import annotations

import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "update_homebrew_formula.py"
FIXTURE = ROOT / "scripts" / "fixtures" / "homebrew-tachi.rb.fixture"

# Fixed digest used only for offline unit tests (does not match a real asset).
FAKE_SHA = "a" * 64
VERSION = "1.7.0"
TRIPLE = "aarch64-apple-darwin"
ASSET_REPO = "kckylechen1/homebrew-tachi"
ASSET_TAG = f"tachi-{VERSION}"
ASSET_URL = (
    f"https://github.com/{ASSET_REPO}/releases/download/{ASSET_TAG}/"
    f"tachi-v{VERSION}-{TRIPLE}.tar.gz"
)


class UpdateHomebrewFormulaBinaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="tachi-formula-"))
        self.formula = self.tmp / "tachi.rb"
        shutil.copy(FIXTURE, self.formula)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_script(self, *extra: str) -> subprocess.CompletedProcess[str]:
        cmd = [
            sys.executable,
            str(SCRIPT),
            str(self.formula),
            "--version",
            VERSION,
            "--mode",
            "binary",
            f"--sha256={TRIPLE}={FAKE_SHA}",
            *extra,
        ]
        return subprocess.run(cmd, check=True, capture_output=True, text=True)

    def test_binary_mode_rewrites_private_source_to_public_asset(self) -> None:
        """RED on pre-fix script: default was private archive URL (404 public)."""
        before = self.formula.read_text(encoding="utf-8")
        self.assertIn("archive/refs/tags/", before)
        self.assertIn("disable!", before)
        self.assertIn('depends_on "rust" => :build', before)

        self.run_script()
        after = self.formula.read_text(encoding="utf-8")

        self.assertNotIn("archive/refs/tags/", after)
        self.assertNotIn("disable!", after)
        self.assertNotIn('depends_on "rust" => :build', after)
        self.assertNotIn("head ", after)
        self.assertIn(ASSET_URL, after)
        self.assertIn(f'sha256 "{FAKE_SHA}"', after)
        self.assertIn('version "1.7.0"', after)
        self.assertIn("on_arm do", after)
        self.assertIn('bin.install "tachi"', after)
        self.assertNotIn("cargo", after)
        self.assertIn("homebrew-tachi/releases", after)

    def test_binary_mode_keeps_service_and_hub_test(self) -> None:
        self.run_script()
        after = self.formula.read_text(encoding="utf-8")
        self.assertIn("service do", after)
        self.assertIn('tachi hub --help', after)
        self.assertIn("--no-project-db", after)


if __name__ == "__main__":
    unittest.main()
