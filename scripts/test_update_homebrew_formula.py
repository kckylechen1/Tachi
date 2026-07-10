#!/usr/bin/env python3
"""Discrimination tests for public-binary Homebrew formula updates (#728/#874)."""

from __future__ import annotations

import importlib.util
import pathlib
import shutil
import subprocess
import sys
import tempfile
import types
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "update_homebrew_formula.py"
FIXTURE = ROOT / "scripts" / "fixtures" / "homebrew-tachi.rb.fixture"

# Fixed digest used only for offline unit tests (does not match a real asset).
FAKE_SHA = "a" * 64
FAKE_INTEL_SHA = "b" * 64
VERSION = "1.7.0"
TRIPLE = "aarch64-apple-darwin"
INTEL_TRIPLE = "x86_64-apple-darwin"
ASSET_REPO = "kckylechen1/homebrew-tachi"
ASSET_TAG = f"tachi-{VERSION}"
ASSET_URL = (
    f"https://github.com/{ASSET_REPO}/releases/download/{ASSET_TAG}/"
    f"tachi-v{VERSION}-{TRIPLE}.tar.gz"
)


def _load_module() -> types.ModuleType:
    """Import update_homebrew_formula.py by path (scripts/ is not a package)."""
    spec = importlib.util.spec_from_file_location("update_homebrew_formula", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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

    def test_service_block_sets_keep_alive(self) -> None:
        """#936: the service block must set `keep_alive true` so launchd respawns
        the daemon after a fail-loud / watchdog exit. Without it, 'exit → launchd
        respawns' is false and the liveness recovery premise breaks."""
        self.run_script()
        after = self.formula.read_text(encoding="utf-8")
        service = self.module_service_block(after)
        self.assertIn("keep_alive true", service)

    @staticmethod
    def module_service_block(text: str) -> str:
        import re

        match = re.search(r"  service do\n.*?  end\n?", text, re.DOTALL)
        assert match is not None, f"no service block in formula:\n{text}"
        return match.group(0)

    def test_service_block_keep_alive_is_idempotent(self) -> None:
        """Re-running the updater on its own output must not duplicate keep_alive."""
        self.run_script()
        once = self.formula.read_text(encoding="utf-8")
        # Second pass over the already-updated formula (re-promote path).
        self.run_script()
        twice = self.formula.read_text(encoding="utf-8")
        self.assertEqual(once.count("keep_alive true"), 1)
        self.assertEqual(twice.count("keep_alive true"), 1)


class ApplyBinaryUrlsRepromoteTests(unittest.TestCase):
    """#907 (reviewed in #907): on_macos removal regex must remove the WHOLE
    outer block on re-promote, not stop at the nested on_arm `end`.

    The generated ``on_macos`` block nests ``on_arm``/``on_intel`` at 4-space
    indent inside a 2-space-indented outer block:

        \x20\x20on_macos do
        \x20\x20\x20\x20on_arm do
        \x20\x20\x20\x20\x20\x20...
        \x20\x20\x20\x20end
        \x20\x20\x20\x20on_intel do
        \x20\x20\x20\x20\x20\x20...
        \x20\x20\x20\x20end
        \x20\x20end

    Running the updater a second time (manual re-promote via
    .github/workflows/update-homebrew-tap.yml) must be idempotent: the old
    block is fully removed before the fresh one is inserted. A regex that
    matches "  end\\n" as a *substring* of the 4-space inner "    end\\n"
    line stops early and leaves `on_intel .. end` + the outer `end`
    orphaned, corrupting the formula.
    """

    def setUp(self) -> None:
        self.module = _load_module()
        self.base_text = FIXTURE.read_text(encoding="utf-8")

    def _apply(self, text: str) -> str:
        return self.module.apply_binary_urls(
            text,
            version=VERSION,
            asset_repo=ASSET_REPO,
            asset_tag=ASSET_TAG,
            platform_triples={"arm": TRIPLE, "intel": INTEL_TRIPLE},
            sha_by_triple={TRIPLE: FAKE_SHA, INTEL_TRIPLE: FAKE_INTEL_SHA},
        )

    def test_repromote_is_idempotent_arm_and_intel(self) -> None:
        """RED on pre-fix regex: second apply orphans on_intel/end (#907)."""
        once = self._apply(self.base_text)
        self.assertEqual(once.count("on_macos do"), 1)
        self.assertEqual(once.count("on_arm do"), 1)
        self.assertEqual(once.count("on_intel do"), 1)

        # Simulate a re-promote: run the updater again on its own output.
        twice = self._apply(once)

        self.assertEqual(
            twice.count("on_macos do"),
            1,
            f"expected exactly one on_macos block after re-promote, got:\n{twice}",
        )
        self.assertEqual(twice.count("on_arm do"), 1)
        self.assertEqual(
            twice.count("on_intel do"),
            1,
            f"orphaned on_intel block survived re-promote:\n{twice}",
        )
        # The on_macos block itself must be well-formed (outer do/end
        # balanced around exactly one on_arm and one on_intel sub-block) —
        # extractable as a single self-contained regex match, not split
        # across an orphaned tail.
        match = self.module.re.search(
            r"\n  on_macos do\n"
            r"    on_arm do\n.*?\n    end\n"
            r"    on_intel do\n.*?\n    end\n"
            r"  end\n",
            twice,
            flags=self.module.re.DOTALL,
        )
        self.assertIsNotNone(
            match, f"on_macos block is not a single well-formed block:\n{twice}"
        )

        # Strongest check: re-promote must reproduce byte-identical output
        # (the freshly generated block, nothing orphaned around it).
        self.assertEqual(
            twice,
            once,
            "re-promote (second apply) must be idempotent with the first apply",
        )

    def test_repromote_is_idempotent_arm_only(self) -> None:
        """Same idempotency property for the arm-only (odie-intel) shape."""

        def apply_arm_only(text: str) -> str:
            return self.module.apply_binary_urls(
                text,
                version=VERSION,
                asset_repo=ASSET_REPO,
                asset_tag=ASSET_TAG,
                platform_triples={"arm": TRIPLE},
                sha_by_triple={TRIPLE: FAKE_SHA},
            )

        once = apply_arm_only(self.base_text)
        twice = apply_arm_only(once)
        self.assertEqual(twice.count("on_macos do"), 1)
        self.assertEqual(twice.count("on_arm do"), 1)
        self.assertEqual(twice.count("odie "), 1)
        self.assertEqual(twice, once)


if __name__ == "__main__":
    unittest.main()
