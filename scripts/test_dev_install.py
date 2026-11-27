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
    "[action=respond/send|required] Stable recipient AgentIdentity id.",
    "[action=status/receive] Row limit, 1..=100; defaults to 20. Status never consumes; receive deliberately consumes.",
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

    def run_install(
        self, content: str, extra_env: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ | {
            "HOME": str(self.home),
            # Pin the build output into the temp root: an inherited
            # CARGO_TARGET_DIR (e.g. a CI runner's shared target) must never
            # receive the fake release binary.
            "CARGO_TARGET_DIR": str(self.root / "target"),
            "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
            "FAKE_CARGO_ARGS": str(self.root / "cargo-args"),
            "FAKE_BINARY_CONTENT": content,
        }
        env |= extra_env or {}
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

    def test_installed_path_stays_occupied_while_the_binary_is_replaced(self) -> None:
        destination = self.home / "bin/tachi"
        destination.parent.mkdir()
        destination.write_text("old binary", encoding="utf-8")
        observed = self.root / "mv-observations"
        # Every mv the script runs reports, once it returns, whether the
        # installed path is there. A daemon or a CLI call that starts in that
        # moment must find a complete executable.
        make_executable(
            self.fake_bin / "mv",
            """#!/bin/sh
set -eu
real_mv=
for candidate in /bin/mv /usr/bin/mv; do
  if [ -x "$candidate" ]; then real_mv="$candidate"; break; fi
done
"$real_mv" "$@"
if [ -e "$OBSERVED_PATH" ] || [ -L "$OBSERVED_PATH" ]; then
  printf 'present\n' >> "$OBSERVED_LOG"
else
  printf 'missing\n' >> "$OBSERVED_LOG"
fi
""",
        )

        result = self.run_install(
            self.fake_binary(),
            {"OBSERVED_PATH": str(destination), "OBSERVED_LOG": str(observed)},
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("dispatch_outcomes", destination.read_text(encoding="utf-8"))
        observations = observed.read_text(encoding="utf-8").split()
        self.assertTrue(observations, "the script did not move anything")
        self.assertEqual(set(observations), {"present"}, observations)

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
            "[action=respond/send|required] Stable recipient AgentIdentity id.",
            "handle_a2a_respond",
        )

        result = self.run_install(content)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "release literal gate failed: missing [action=respond/send|required] Stable recipient AgentIdentity id.",
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

# #2132: eng-auto-checkpoint pure tests (CI picks up imported TestCase)
# Imported TestCase subclasses are discovered by unittest.loadTestsFromModule,
# so we avoid editing .github/workflows/ci.yml (OAuth often lacks workflow scope).
from scripts.test_eng_checkpoint import EngCheckpointPureTests  # noqa: E402,F401
# #2135: binary-backed integration (temp TACHI_HOME only). Skips unless a tachi
# binary is available via TACHI_BIN or PATH, so CI without a build stays green.
from scripts.test_eng_checkpoint import EngCheckpointGlobalScopeIntegrationTests  # noqa: E402,F401

