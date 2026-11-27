"""Regression tests for the daemon service functions of scripts/install.sh.

The functions are lifted out of the script by name and run under bash with
stubbed tools, so nothing here talks to launchd or to a real daemon.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/install.sh"
LABEL = "com.kckylechen.tachi.daemon"

STATUS_FUNCTIONS = [
    "daemon_status_for_scope",
    "daemon_status_text",
    "daemon_status_number",
]
STOP_FUNCTIONS = STATUS_FUNCTIONS + [
    "daemon_status_is_running",
    "stop_existing_daemon_for_scope",
]
SERVICE_FUNCTIONS = STOP_FUNCTIONS + [
    "xml_escape",
    "launchctl_bootout_plist",
    "launchctl_bootstrap_plist",
    "stop_existing_daemon_for_scope",
    "wait_for_daemon_for_scope",
    "install_daemon_service",
]

# Answers `daemon status` from a file, and reports `none` once a kill or a
# bootstrap has happened.
FAKE_TACHI = """#!/bin/sh
case "$*" in
  *"daemon status"*)
    if [ -f "$BOOTSTRAP_MARKER" ]; then
      printf '{\\n  "state": "running",\\n  "pid": 5,\\n  "lock_path": "/tmp/lock"\\n}\\n'
    else
      cat "$STATUS_FILE"
    fi
    ;;
  *"daemon kill"*)
    printf 'daemon kill\\n' >> "$CALLS"
    printf '{\\n  "state": "none"\\n}\\n' > "$STATUS_FILE"
    ;;
esac
"""


def function(name: str) -> str:
    text = SCRIPT.read_text(encoding="utf-8")
    start = text.index(f"\n{name}() {{\n") + 1
    return text[start : text.index("\n}\n", start) + 3]


def foreign(reason: str) -> str:
    return (
        '{\n  "state": "foreign",\n  "pid": 4242,\n  "lock_path": "/tmp/lock",\n'
        f'  "reason": "{reason}",\n  "version": "2.0.0",\n'
        '  "port": 6919,\n  "global_db": "/tmp/tachi-memory.db"\n}\n'
    )


class InstallScriptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.home = self.root / "home"
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()
        self.home.mkdir()
        fake = self.fake_bin / "tachi"
        fake.write_text(FAKE_TACHI, encoding="utf-8")
        fake.chmod(0o755)
        self.status = self.root / "status.json"
        self.calls = self.root / "calls"
        self.base_env = os.environ | {
            "HOME": str(self.home),
            "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
            "STATUS_FILE": str(self.status),
            "CALLS": str(self.calls),
            "BOOTSTRAP_MARKER": str(self.root / "bootstrapped"),
            "LAUNCHCTL_LOG": str(self.root / "launchctl"),
        }

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def run_bash(self, body: str, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", "-c", "set -euo pipefail\n" + body],
            env=self.base_env | env,
            capture_output=True,
            text=True,
            check=False,
        )

    def call_log(self) -> list[str]:
        if not self.calls.exists():
            return []
        return self.calls.read_text(encoding="utf-8").splitlines()

    # ── stopping what holds the scope ─────────────────────────────────────

    def stop_driver(self) -> str:
        return (
            "\n".join(function(name) for name in STOP_FUNCTIONS)
            + """
sleep() { :; }
kill() {
  printf 'kill %s\n' "$*" >> "$CALLS"
  printf '{\n  "state": "none"\n}\n' > "$STATUS_FILE"
}
stop_existing_daemon_for_scope "$TACHI_FAKE" /tmp/tachi-memory.db
"""
        )

    def test_a_running_daemon_is_stopped_through_the_cli(self) -> None:
        self.status.write_text('{\n  "state": "running",\n  "pid": 7\n}\n', encoding="utf-8")
        result = self.run_bash(
            self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.call_log(), ["daemon kill"])
        self.assertIn("Stopping existing Tachi daemon", result.stdout)

    def test_the_previous_versions_daemon_is_stopped_before_the_new_service_starts(self) -> None:
        self.status.write_text(
            foreign("daemon version 2.0.0 does not match binary 2.1.0"), encoding="utf-8"
        )
        result = self.run_bash(
            self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.call_log(), ["kill -TERM 4242"], result.stdout)
        self.assertIn("previous version", result.stdout)

    def test_another_foreign_daemon_is_left_alone_and_reported(self) -> None:
        self.status.write_text(
            foreign("daemon global_db /elsewhere/tachi-memory.db does not match /tmp/tachi-memory.db"),
            encoding="utf-8",
        )
        result = self.run_bash(
            self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
        )
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertEqual(self.call_log(), [])
        self.assertIn("foreign Tachi daemon holds this DB scope", result.stdout)

    def test_a_status_that_cannot_be_read_stops_the_install(self) -> None:
        # `unavailable` is a live pid whose state could not be read; an empty
        # or unparsable answer means the binary did not say. Neither is
        # "nothing holds the scope".
        for status in (
            '{"state": "unavailable", "pid": 4242, "lock_path": "/tmp/lock", "reason": "x"}',
            "not json",
            "",
        ):
            with self.subTest(status=status):
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("cannot tell what holds this DB scope", result.stdout)
                self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_a_stale_or_absent_record_lets_the_install_continue(self) -> None:
        for status in (
            '{"state": "stale_pid", "pid": 4242, "lock_path": "/tmp/lock"}',
            '{"state": "none"}',
        ):
            with self.subTest(status=status):
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
                )
                self.assertEqual(result.returncode, 0, result.stdout)
                self.assertEqual(self.call_log(), [])

    # ── the LaunchAgent plist ─────────────────────────────────────────────

    def service_driver(self, *, fail_the_write: bool = False) -> str:
        stubs = """
require_cmd() { :; }
uname() { echo Darwin; }
launchctl() {
  printf '%s\n' "$*" >> "$LAUNCHCTL_LOG"
  case "$*" in bootstrap*) : > "$BOOTSTRAP_MARKER" ;; esac
}
sleep() { :; }
"""
        if fail_the_write:
            stubs += "cat() { printf 'partial'; return 1; }\n"
        return (
            "\n".join(function(name) for name in SERVICE_FUNCTIONS)
            + stubs
            + "\nDAEMON_PORT=6919\ninstall_daemon_service\n"
        )

    def launch_agents(self) -> Path:
        directory = self.home / "Library/LaunchAgents"
        directory.mkdir(parents=True)
        return directory

    def test_the_plist_is_written_complete_and_nothing_else_is_left_behind(self) -> None:
        directory = self.launch_agents()
        # A fresh install: the binary answers that nothing holds the scope.
        # No answer at all is not the same thing, and is refused.
        self.status.write_text('{\n  "state": "none"\n}\n', encoding="utf-8")
        result = self.run_bash(self.service_driver(), {})
        self.assertEqual(result.returncode, 0, result.stderr)
        plist = directory / f"{LABEL}.plist"
        self.assertEqual([entry.name for entry in directory.iterdir()], [plist.name])
        text = plist.read_text(encoding="utf-8")
        self.assertIn(f"<string>{LABEL}</string>", text)
        self.assertIn("<string>6919</string>", text)
        self.assertTrue(text.startswith("<?xml"), text[:40])
        self.assertTrue(text.rstrip().endswith("</plist>"))
        self.assertIn(f"bootstrap gui/{os.getuid()} {plist}", (self.root / "launchctl").read_text(encoding="utf-8"))

    def test_a_failed_write_leaves_the_previous_plist_in_place(self) -> None:
        directory = self.launch_agents()
        plist = directory / f"{LABEL}.plist"
        plist.write_text("previous plist\n", encoding="utf-8")
        result = self.run_bash(self.service_driver(fail_the_write=True), {})
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertEqual(plist.read_text(encoding="utf-8"), "previous plist\n")
        self.assertEqual([entry.name for entry in directory.iterdir()], [plist.name])


if __name__ == "__main__":
    unittest.main()
