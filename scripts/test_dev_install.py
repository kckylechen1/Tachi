"""Regression tests for scripts/dev-install.sh without touching the real HOME."""

from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/dev-install.sh"
SENTINELS = (
    "tachi_memory(action='sticky_leave')",
    "tachi_memory(action='sticky_check')",
    "FROM session_claims WHERE claim_id = ?1",
    "dispatch_outcomes row ",
)


def make_executable(path: Path, body: str) -> None:
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class DevInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.home = self.root / "home"
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()
        self.home.mkdir()
        make_executable(
            self.fake_bin / "cargo",
            """#!/bin/sh
set -eu
printf '%s\\n' \"$*\" > \"$FAKE_CARGO_ARGS\"
if [ \"$*\" != \"build --locked --release --bin tachi\" ]; then
  echo \"unexpected cargo invocation: $*\" >&2
  exit 64
fi
mkdir -p \"$CARGO_TARGET_DIR/release\"
printf '%s\\n' \"$FAKE_BINARY_CONTENT\" > \"$CARGO_TARGET_DIR/release/tachi\"
chmod +x \"$CARGO_TARGET_DIR/release/tachi\"
""",
        )
        make_executable(
            self.fake_bin / "strings",
            "#!/bin/sh\nsed -n 's/^: \"\\(.*\\)\"$/\\1/p' \"$1\"\n",
        )

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def run_install(self, content: str) -> subprocess.CompletedProcess[str]:
        env = os.environ | {
            "HOME": str(self.home),
            "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
            "FAKE_CARGO_ARGS": str(self.root / "cargo-args"),
            "FAKE_BINARY_CONTENT": content,
        }
        return subprocess.run(
            [str(SCRIPT)],
            cwd=ROOT,
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

    @staticmethod
    def fake_binary(
        *, include_all_sentinels: bool = True, fail_version_check: bool = False
    ) -> str:
        sentinels = list(SENTINELS)
        if not include_all_sentinels:
            sentinels.remove("dispatch_outcomes row ")
        version_check = "exit 1" if fail_version_check else "echo 'tachi 1.9.0-test'"
        return "\n".join(
            ["#!/bin/sh", version_check, *(f': "{s}"' for s in sentinels)]
        )

    def test_builds_gates_backs_up_and_installs_a_real_file(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        destination.write_text("old binary", encoding="utf-8")

        result = self.run_install(self.fake_binary())

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.root / "cargo-args").read_text(encoding="utf-8").strip(),
            "build --locked --release --bin tachi",
        )
        self.assertIn("release literal gate passed", result.stdout)
        self.assertIn("tachi 1.9.0-test", result.stdout)
        self.assertIn("rollback: mv", result.stdout)
        self.assertFalse(destination.is_symlink())
        self.assertIn("dispatch_outcomes", destination.read_text(encoding="utf-8"))
        backups = list(destination.parent.glob("tachi.backup.*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_text(encoding="utf-8"), "old binary")

    def test_replaces_a_symlink_but_preserves_it_as_the_backup(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        legacy = self.root / "legacy-tachi"
        legacy.write_text("legacy binary", encoding="utf-8")
        destination.symlink_to(legacy)

        result = self.run_install(self.fake_binary())

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"replacing symlink {destination} -> {legacy}", result.stdout)
        self.assertFalse(destination.is_symlink())
        backups = list(destination.parent.glob("tachi.backup.*"))
        self.assertEqual(len(backups), 1)
        self.assertTrue(backups[0].is_symlink())
        self.assertTrue(backups[0].samefile(legacy))

    def test_missing_literal_refuses_before_backing_up_the_current_binary(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        destination.write_text("old binary", encoding="utf-8")

        result = self.run_install(self.fake_binary(include_all_sentinels=False))

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release literal gate failed: missing dispatch_outcomes row", result.stderr)
        self.assertEqual(destination.read_text(encoding="utf-8"), "old binary")
        self.assertEqual(list(destination.parent.glob("tachi.backup.*")), [])

    def test_private_identifier_cannot_satisfy_a_public_literal_gate(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        destination.write_text("old binary", encoding="utf-8")
        content = self.fake_binary().replace(
            "tachi_memory(action='sticky_leave')", "handle_sticky_leave"
        )

        result = self.run_install(content)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "release literal gate failed: missing tachi_memory(action='sticky_leave')",
            result.stderr,
        )
        self.assertEqual(destination.read_text(encoding="utf-8"), "old binary")

    def test_failed_version_check_restores_the_previous_binary(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        destination.write_text("old binary", encoding="utf-8")

        result = self.run_install(self.fake_binary(fail_version_check=True))

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("installed binary failed its version check", result.stderr)
        self.assertEqual(destination.read_text(encoding="utf-8"), "old binary")
        self.assertEqual(list(destination.parent.glob("tachi.backup.*")), [])


if __name__ == "__main__":
    unittest.main()
