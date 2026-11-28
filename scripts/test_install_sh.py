"""Regression tests for the daemon service functions of scripts/install.sh.

The functions are lifted out of the script by name and run under bash with
stubbed tools, so nothing here talks to launchd or to a real daemon.
"""

from __future__ import annotations

import json
import os
import plistlib
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/install.sh"
LABEL = "com.kckylechen.tachi.daemon"
SCOPED_LOCK = "/tmp/daemon-0123456789abcdef.lock"

STATUS_FUNCTIONS = [
    "json_object_fields",
    "daemon_status_for_scope",
    "daemon_status_field",
]
STOP_FUNCTIONS = STATUS_FUNCTIONS + [
    "absolute_db_path",
    "canonical_db_path",
    "daemon_scope_verdict",
    "check_daemon_for_upgrade",
]
SERVICE_FUNCTIONS = STOP_FUNCTIONS + [
    "xml_escape",
    "check_launch_agent_db",
    "check_existing_plist_scope",
    "read_launchctl_scope",
    "check_loaded_job_scope",
    "launchctl_bootstrap_plist",
    "wait_for_daemon_to_stop",
    "wait_for_daemon_for_scope",
    "install_daemon_service",
    "install_locked_daemon_service",
]

PROCESS_STUBS = """
sleep() { :; }
ps() {
  [ "${PS_ROW+x}" = x ] || return 1
  printf '%s\n' "$PS_ROW"
}
lsof() {
  # A foreign daemon may have the target SQLite file open as a project DB.
  if [ "$7" != /tmp/tachi-memory.db ]; then
    [ "$7" = "${OPEN_LOCK_PATH-$SCOPED_LOCK}" ] || return 1
  fi
  printf '%s\n' "$5"
}
kill() {
  printf 'kill %s\n' "$*" >> "$CALLS"
  if [ -n "${STATUS_AFTER_KILL+set}" ]; then
    printf '%b\n' "$STATUS_AFTER_KILL" > "$STATUS_FILE"
  else
    printf '%s\n' '{"state":"none"}' > "$STATUS_FILE"
  fi
}
"""

# Answers `daemon status` from a file and then exits with STATUS_EXIT. A
# draining launchd job answers once before reporting `none`. After bootstrap,
# it answers STATUS_AFTER_BOOTSTRAP if set, else `running`.
FAKE_TACHI = """#!/bin/sh
case "$*" in
  *"daemon status"*)
    if [ -f "$BOOTSTRAP_MARKER" ]; then
      if [ -n "${STATUS_AFTER_BOOTSTRAP+set}" ]; then
        printf '%b\\n' "$STATUS_AFTER_BOOTSTRAP"
      else
        printf '{\\n  "state": "running",\\n  "pid": 5,\\n  "lock_path": "/tmp/lock"\\n}\\n'
      fi
    elif [ -f "$DRAIN_MARKER" ] && [ -n "${STATUS_AFTER_BOOTOUT+set}" ]; then
      printf '%s\\n' "$STATUS_AFTER_BOOTOUT"
    else
      cat "$STATUS_FILE"
      if [ -f "$DRAIN_MARKER" ]; then
        printf '%s\\n' '{"state":"none"}' > "$STATUS_FILE"
      fi
    fi
    exit "${STATUS_EXIT:-0}"
    ;;
  *"daemon kill"*)
    printf 'daemon kill\\n' >> "$CALLS"
    ;;
esac
"""


def function(name: str) -> str:
    text = SCRIPT.read_text(encoding="utf-8")
    start = text.index(f"\n{name}() {{\n") + 1
    return text[start : text.index("\n}\n", start) + 3]


def foreign(reason: str) -> str:
    return (
        '{\n  "state": "foreign",\n  "pid": 4242,\n'
        f'  "lock_path": "{SCOPED_LOCK}",\n'
        f'  "reason": "{reason}",\n  "version": "2.0.0",\n'
        '  "port": 6919,\n  "global_db": "/tmp/tachi-memory.db"\n}\n'
    )


def running(pid: int, lock_path: str = SCOPED_LOCK) -> str:
    return json.dumps({"state": "running", "pid": pid, "lock_path": lock_path})


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
        self.db = self.home / ".tachi/global/tachi-memory.db"
        self.loaded_job = self.root / "loaded-job"
        self.base_env = os.environ | {
            "HOME": str(self.home),
            "TEST_ROOT": str(self.root),
            "PATH": f"{self.fake_bin}:{os.environ['PATH']}",
            "STATUS_FILE": str(self.status),
            "SCOPED_LOCK": SCOPED_LOCK,
            "CALLS": str(self.calls),
            "BOOTSTRAP_MARKER": str(self.root / "bootstrapped"),
            "DRAIN_MARKER": str(self.root / "draining"),
            "LAUNCHCTL_LOG": str(self.root / "launchctl"),
            "LOADED_JOB": str(self.loaded_job),
            "TACHI_HOME": str(self.db.parents[1]),
            "TACHI_DAEMON_GLOBAL_DB": str(self.db),
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
            + PROCESS_STUBS
            + '\ncheck_daemon_for_upgrade "$TACHI_FAKE" /tmp/tachi-memory.db\n'
        )

    def test_an_unmanaged_running_daemon_is_never_signalled(self) -> None:
        self.status.write_text(running(7), encoding="utf-8")
        result = self.run_bash(
            self.stop_driver(),
            {
                "TACHI_FAKE": str(self.fake_bin / "tachi"),
                "PS_ROW": "tachi /old/bin/tachi --daemon --global-db /tmp/tachi-memory.db",
            },
        )
        self.assertEqual(self.call_log(), [], result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Stop it through its service manager", result.stderr)

    def test_an_unmanaged_previous_version_is_never_signalled(self) -> None:
        self.status.write_text(
            foreign("daemon version 2.0.0 does not match binary 2.1.0"), encoding="utf-8"
        )
        result = self.run_bash(
            self.stop_driver(),
            {
                "TACHI_FAKE": str(self.fake_bin / "tachi"),
                "PS_ROW": "tachi /old/bin/tachi --daemon --global-db /tmp/tachi-memory.db",
            },
        )
        self.assertEqual(self.call_log(), [], result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Stop it through its service manager", result.stderr)

    def test_a_recycled_pid_is_not_signalled_on_version_mismatch(self) -> None:
        for row, lock in (
            ("sleep /old/bin/tachi --daemon --global-db /tmp/tachi-memory.db", SCOPED_LOCK),
            ("tachi /old/bin/tachi --daemon --global-db /elsewhere/tachi-memory.db", ""),
            ("tachi /old/bin/tachi --global-db /tmp/tachi-memory.db daemon status", SCOPED_LOCK),
            ("", SCOPED_LOCK),
        ):
            with self.subTest(row=row):
                self.status.write_text(
                    foreign("daemon version 2.0.0 does not match binary 2.1.0"),
                    encoding="utf-8",
                )
                self.calls.unlink(missing_ok=True)
                result = self.run_bash(
                    self.stop_driver(),
                    {"TACHI_FAKE": str(self.fake_bin / "tachi"), "PS_ROW": row, "OPEN_LOCK_PATH": lock},
                )
                self.assertEqual(self.call_log(), [], result.stdout)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("outside the verified LaunchAgent", result.stderr)

    def test_a_matching_command_without_an_open_scoped_lock_is_not_signalled(self) -> None:
        for reported, opened in ((SCOPED_LOCK, ""), ("/tmp/daemon.lock", "/tmp/daemon.lock")):
            with self.subTest(reported=reported, opened=opened):
                self.status.write_text(running(7, reported), encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(),
                    {
                        "TACHI_FAKE": str(self.fake_bin / "tachi"),
                        "PS_ROW": "tachi /old/bin/tachi --daemon --global-db /tmp/tachi-memory.db",
                        "OPEN_LOCK_PATH": opened,
                    },
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("outside the verified LaunchAgent", result.stderr)
                self.assertEqual(self.call_log(), [])

    def test_a_project_db_match_does_not_authorize_signalling_a_foreign_daemon(self) -> None:
        self.status.write_text(
            foreign("daemon version 2.0.0 does not match binary 2.1.0"), encoding="utf-8"
        )
        # The global DB path contains a space. ps flattens argv, and the
        # process really does have the requested DB open as its project DB.
        result = self.run_bash(
            self.stop_driver(),
            {
                "TACHI_FAKE": str(self.fake_bin / "tachi"),
                "PS_ROW": (
                    "tachi /old/bin/tachi --daemon --global-db /tmp/tachi-memory.db foreign"
                    " --project-db /tmp/tachi-memory.db"
                ),
                "OPEN_LOCK_PATH": "/tmp/daemon-fedcba9876543210.lock",
            },
        )
        self.assertEqual(self.call_log(), [], result.stdout)
        self.assertNotEqual(result.returncode, 0, result.stdout)

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
dscacheutil() { printf 'dir: %s\n' "$TEST_ROOT/home"; }
mkdir() {
  case "$1" in
    /private/tmp/tachi-launchagent-*) command mkdir "$TEST_ROOT/install.lock" ;;
    *) command mkdir "$@" ;;
  esac
}
rmdir() {
  case "$1" in
    /private/tmp/tachi-launchagent-*) command rmdir "$TEST_ROOT/install.lock" ;;
    *) command rmdir "$@" ;;
  esac
}
pause_install() {
  [ "${PAUSE_INSTALL:-0}" = 1 ] || return 0
  : > "$TEST_ROOT/$1-entered"
  until [ -e "$TEST_ROOT/$1-release" ]; do /bin/sleep 0.02; done
}
launchctl() {
  if [ "$1" = print ]; then
    if [ "$2" = "gui/$(id -u)" ]; then
      printf '%s = {\n\tenvironment = {\n%b\t}\n}\n' "$2" "${DOMAIN_ENVIRONMENT:-}"
      return
    fi
    pause_install print
    if [ -n "${PRINT_ERROR:-}" ]; then printf '%s\n' "$PRINT_ERROR" >&2; return 1; fi
    if [ ! -f "$LOADED_JOB" ]; then
      printf 'Bad request.\nCould not find service "%s" in domain for user gui: %s\n' "${2##*/}" "$(id -u)" >&2
      return 1
    fi
    /bin/cat "$LOADED_JOB"
    return
  fi
  printf '%s\n' "$*" >> "$LAUNCHCTL_LOG"
  case "$1" in
    bootout) if [ "${DRAIN_MANAGED_JOB:-0}" = 1 ]; then : > "$DRAIN_MARKER"; fi ;;
    bootstrap) pause_install bootstrap; : > "$BOOTSTRAP_MARKER" ;;
  esac
}
"""
        if fail_the_write:
            stubs += "cat() { printf 'partial'; return 1; }\n"
        return (
            "\n".join(function(name) for name in SERVICE_FUNCTIONS)
            + PROCESS_STUBS
            + stubs
            + "\nDAEMON_PORT=6919\ninstall_daemon_service\n"
        )

    def launch_agents(self) -> Path:
        directory = self.home / "Library/LaunchAgents"
        directory.mkdir(parents=True)
        return directory

    def write_launch_agent(
        self, plist: Path, *args: str, fmt: plistlib.PlistFormat = plistlib.FMT_XML,
        environment: dict[str, str] | None = None,
    ) -> bytes:
        contents = plistlib.dumps(
            {
                "Label": LABEL,
                "ProgramArguments": ["/old/bin/tachi", "--daemon", *args],
                "WorkingDirectory": str(self.home),
                "EnvironmentVariables": environment or {},
            },
            fmt=fmt,
        )
        plist.write_bytes(contents)
        return contents

    def write_loaded_job(
        self, *args: str, pid: int | None = None, indent: str = "\t",
        environment: dict[str, str] | None = None,
    ) -> None:
        launch_args = "".join(f"{indent * 2}{arg}\n" for arg in ("/old/bin/tachi", "--daemon", *args))
        pid_line = f"{indent}pid = {pid}\n" if pid is not None else ""
        env_lines = "".join(f"{indent * 2}{key} => {value}\n" for key, value in (environment or {}).items())
        self.loaded_job.write_text(
            f"gui/{os.getuid()}/{LABEL} = {{\n{indent}working directory = {self.home}\n"
            f"{indent}arguments = {{\n{launch_args}{indent}}}\n"
            f"{indent}environment = {{\n{env_lines}{indent}}}\n{pid_line}}}\n",
            encoding="utf-8",
        )

    def test_foreign_existing_plist_is_preserved_before_bootout(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        foreign_db = str(self.root / "different scope/db.sqlite")
        previous = self.write_launch_agent(plist, "--global-db", foreign_db, fmt=plistlib.FMT_BINARY)
        self.status.write_text('{"state":"none"}', encoding="utf-8")

        result = self.run_bash(self.service_driver(), {})

        self.assertEqual(plist.read_bytes(), previous)
        self.assertFalse((self.root / "launchctl").exists())
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(foreign_db, result.stdout + result.stderr)

    def test_foreign_loaded_job_is_refused_even_with_matching_plist(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        previous = self.write_launch_agent(plist, "--global-db", str(self.db))
        foreign_db = str(self.root / "different scope/db.sqlite")
        self.write_loaded_job(f"--global-db={foreign_db}", indent="    ")
        self.status.write_text('{"state":"none"}', encoding="utf-8")

        result = self.run_bash(self.service_driver(), {})

        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), previous)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(foreign_db, result.stdout + result.stderr)

    def test_foreign_environment_scope_and_empty_flags_are_refused(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        foreign_db = str(self.root / "foreign.db")
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        for loaded in (False, True):
            for args, environment, diagnostic in (
                ((), {"MEMORY_DB_PATH": foreign_db}, foreign_db),
                ((), {"HOME": str(self.root / "other-home")}, "other-home"),
                ((), {"TACHI_HOME": str(self.root / "other-tachi")}, "other-tachi"),
                (("--global-db", ""), {"MEMORY_DB_PATH": str(self.db)}, "empty"),
                (("--global-db=",), {}, "empty"),
            ):
                with self.subTest(loaded=loaded, args=args, environment=environment):
                    self.loaded_job.unlink(missing_ok=True)
                    (self.root / "launchctl").unlink(missing_ok=True)
                    (self.root / "bootstrapped").unlink(missing_ok=True)
                    if loaded:
                        before = self.write_launch_agent(plist, "--global-db", str(self.db))
                        self.write_loaded_job(*args, environment=environment)
                    else:
                        before = self.write_launch_agent(plist, *args, environment=environment)
                    result = self.run_bash(self.service_driver(), {})
                    self.assertFalse((self.root / "launchctl").exists())
                    self.assertEqual(plist.read_bytes(), before)
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn(diagnostic, result.stderr)

    def test_matching_environment_and_default_scopes_can_upgrade(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        for environment in ({}, {"MEMORY_DB_PATH": f"~/.tachi/global/{self.db.name}", "HOME": str(self.home)},
                            {"TACHI_HOME": "~/.tachi"}):
            with self.subTest(environment=environment):
                for marker in ("launchctl", "bootstrapped", "draining"):
                    (self.root / marker).unlink(missing_ok=True)
                self.write_launch_agent(plist, "--no-project-db", environment=environment)
                self.write_loaded_job("--no-project-db", environment=environment, pid=4242)
                self.status.write_text(running(4242), encoding="utf-8")
                result = self.run_bash(self.service_driver(), {"DRAIN_MANAGED_JOB": "1"})
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.call_log(), [])
                self.assertEqual(
                    (self.root / "launchctl").read_text(encoding="utf-8").splitlines(),
                    [f"bootout gui/{os.getuid()}/{LABEL}", f"bootstrap gui/{os.getuid()} {plist}"],
                )

    def test_unloaded_plist_inherits_db_scope_from_gui_domain(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        before = self.write_launch_agent(plist, "--no-project-db")
        foreign_db = self.root / "inherited.db"
        self.status.write_text('{"state":"none"}', encoding="utf-8")

        result = self.run_bash(self.service_driver(), {
            "DOMAIN_ENVIRONMENT": f"\t\tMEMORY_DB_PATH => {foreign_db}\n",
        })

        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), before)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(str(foreign_db), result.stderr)

    def test_missing_loaded_environment_cannot_prove_an_implicit_scope(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        before = self.write_launch_agent(plist, "--global-db", str(self.db))
        self.write_loaded_job("--no-project-db")
        self.loaded_job.write_text(
            self.loaded_job.read_text(encoding="utf-8").replace("\tenvironment = {\n\t}\n", ""),
            encoding="utf-8",
        )
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        result = self.run_bash(self.service_driver(), {})
        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), before)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Cannot read the loaded LaunchAgent environment", result.stderr)

    def test_a_running_job_with_a_retargeted_db_alias_is_left_loaded(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        self.db.parent.mkdir(parents=True)
        self.db.touch()
        alias = self.root / "db-alias"
        alias.symlink_to(self.db)
        before = self.write_launch_agent(plist, "--global-db", str(alias))
        self.write_loaded_job("--global-db", str(alias), pid=4242)
        # The job's path now points to the target, but it is still running
        # with the other scope it opened before the symlink was retargeted.
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        result = self.run_bash(self.service_driver(), {})
        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), before)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Cannot confirm running LaunchAgent PID 4242", result.stderr)

    def test_running_job_without_a_readable_pid_is_left_loaded(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        before = self.write_launch_agent(plist, "--global-db", str(self.db))
        self.write_loaded_job("--global-db", str(self.db))
        job = self.loaded_job.read_text(encoding="utf-8")
        self.loaded_job.write_text(job.replace("\n\tworking directory", "\n\tstate = running\n\tworking directory"),
                                   encoding="utf-8")
        self.status.write_text('{"state":"none"}', encoding="utf-8")

        result = self.run_bash(self.service_driver(), {})

        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), before)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Cannot identify the running LaunchAgent", result.stderr)

    def test_ambiguous_paths_do_not_alias_home(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        for path in ("~//data/db.sqlite", "~other/data/db.sqlite", "data/db.sqlite"):
            with self.subTest(path=path):
                before = self.write_launch_agent(plist, "--global-db", path)
                result = self.run_bash(self.service_driver(), {
                    "TACHI_DAEMON_GLOBAL_DB": str(self.home / "data/db.sqlite"),
                })
                self.assertFalse((self.root / "launchctl").exists())
                self.assertEqual(plist.read_bytes(), before)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(path, result.stderr)

    def test_launchctl_query_failure_does_not_mean_the_job_is_absent(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        before = self.write_launch_agent(plist, "--global-db", str(self.db))
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        result = self.run_bash(self.service_driver(), {"PRINT_ERROR": "Operation not permitted"})
        self.assertFalse((self.root / "launchctl").exists())
        self.assertEqual(plist.read_bytes(), before)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Operation not permitted", result.stderr)

    def test_another_install_cannot_pass_preflight_during_print_or_bootstrap(self) -> None:
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        body = self.service_driver()
        env = self.base_env | {"PAUSE_INSTALL": "1"}
        with subprocess.Popen(
            ["/bin/bash", "-c", "set -euo pipefail\n" + body], env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        ) as first:
            try:
                for phase in ("print", "bootstrap"):
                    deadline = time.monotonic() + 5
                    while not (self.root / f"{phase}-entered").exists():
                        self.assertIsNone(first.poll())
                        self.assertLess(time.monotonic(), deadline, f"First installer did not reach {phase}")
                        time.sleep(0.02)
                    second = self.run_bash(body, {
                        "HOME": str(self.root / "other-home"),
                        "TACHI_DAEMON_GLOBAL_DB": str(self.root / "other.db"),
                    })
                    self.assertNotEqual(second.returncode, 0, second.stdout)
                    self.assertIn("another installer", second.stderr)
                    (self.root / f"{phase}-release").touch()
            finally:
                for phase in ("print", "bootstrap"):
                    (self.root / f"{phase}-release").touch()
                stdout, stderr = first.communicate(timeout=10)
        self.assertEqual(first.returncode, 0, stdout + stderr)
        self.assertTrue((self.root / "bootstrapped").exists())
        self.assertFalse((self.root / "install.lock").exists())
        self.assertFalse((self.root / "other-home").exists())

    def test_a_legacy_relative_scope_is_not_assumed_to_be_relative_to_launchd_cwd(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        args = ("--global-db", "db.sqlite", "--no-project-db")
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        for loaded_only in (False, True):
            with self.subTest(loaded_only=loaded_only):
                if loaded_only:
                    self.write_loaded_job(*args)
                else:
                    self.write_launch_agent(plist, *args)
                before = plist.read_bytes() if plist.exists() else None
                result = self.run_bash(
                    self.service_driver(), {"TACHI_DAEMON_GLOBAL_DB": str(self.home / "db.sqlite")}
                )
                self.assertFalse((self.root / "launchctl").exists())
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("db.sqlite", result.stderr)
                self.assertEqual(plist.read_bytes() if plist.exists() else None, before)
                plist.unlink(missing_ok=True)

    def test_same_scope_upgrade_accepts_canonical_aliases(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        self.db.parent.mkdir(parents=True)
        alias = self.home / "global-alias"
        alias.symlink_to(self.db.parent, target_is_directory=True)
        previous = self.write_launch_agent(plist, "--global-db", f"~/global-alias/{self.db.name}")
        self.write_loaded_job("--global-db", str(self.db), pid=4242)
        self.status.write_text(
            foreign("daemon version 2.0.0 does not match binary 2.1.0"), encoding="utf-8"
        )

        result = self.run_bash(self.service_driver(), {"DRAIN_MANAGED_JOB": "1"})

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.call_log(), [])
        self.assertNotEqual(plist.read_bytes(), previous)
        self.assertEqual(
            (self.root / "launchctl").read_text(encoding="utf-8").splitlines(),
            [f"bootout gui/{os.getuid()}/{LABEL}", f"bootstrap gui/{os.getuid()} {plist}"],
        )

    def test_a_matching_loaded_daemon_can_drain_without_a_numeric_signal(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        self.write_launch_agent(plist, "--global-db", str(self.db))
        self.write_loaded_job("--global-db", str(self.db), pid=4242)
        self.status.write_text(
            foreign("daemon version 2.0.0 does not match binary 2.1.0"), encoding="utf-8"
        )

        result = self.run_bash(self.service_driver(), {"DRAIN_MANAGED_JOB": "1"})

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.call_log(), [])
        self.assertEqual(
            (self.root / "launchctl").read_text(encoding="utf-8").splitlines(),
            [f"bootout gui/{os.getuid()}/{LABEL}", f"bootstrap gui/{os.getuid()} {plist}"],
        )

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
        previous = self.write_launch_agent(plist, "--no-project-db")
        self.status.write_text('{"state":"none"}', encoding="utf-8")
        result = self.run_bash(self.service_driver(fail_the_write=True), {})
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("could not write", result.stdout)
        self.assertFalse((self.root / "install.lock").exists())
        self.assertEqual(plist.read_bytes(), previous)
        self.assertEqual([entry.name for entry in directory.iterdir()], [plist.name])

    def test_a_refused_scope_is_refused_before_anything_is_replaced(self) -> None:
        directory = self.launch_agents()
        plist = directory / f"{LABEL}.plist"
        held = (
            foreign(
                "daemon global_db /elsewhere/tachi-memory.db does not match /tmp/tachi-memory.db"
            ),
            "foreign Tachi daemon holds this DB scope",
        )
        unreadable = ('{"state": "none"]', "cannot tell what holds this DB scope")
        for has_plist, (status, complaint) in (
            (True, held),
            (False, held),
            (True, unreadable),
            (False, unreadable),
        ):
            with self.subTest(has_plist=has_plist, status=status):
                self.calls.unlink(missing_ok=True)
                for entry in directory.iterdir():
                    entry.unlink()
                previous = self.write_launch_agent(plist, "--no-project-db") if has_plist else None
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(self.service_driver(), {})
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(complaint, result.stdout)
                expected = [plist.name] if previous is not None else []
                self.assertEqual([entry.name for entry in directory.iterdir()], expected)
                if previous is not None:
                    self.assertEqual(plist.read_bytes(), previous)
                self.assertFalse(
                    (self.root / "launchctl").exists(),
                    "launchd was asked to change the service before the scope was checked",
                )

    # ── what counts as an answer ───────────────────────────────────────────

    def test_only_one_json_object_is_an_answer(self) -> None:
        def fields_of(text: str) -> subprocess.CompletedProcess[str]:
            body = (
                function("json_object_fields")
                + "\ncat <<'ANSWER' | json_object_fields\n" + text + "\nANSWER\n"
            )
            return self.run_bash(body, {})

        whole = (
            '{\n  "state": "none",\n  "pid": 4242,\n  "ratio": 1.5,\n  "flag": true,\n'
            '  "nothing": null,\n  "nested": {"a": [1, "}"]},\n  "q": "say \\"hi\\""\n}\n'
        )
        # One line per top-level field, with its type. A nested value is one
        # field of its own kind, whatever it holds.
        expected = (
            "state\tstring\tnone\npid\tnumber\t4242\nratio\tnumber\t1.5\nflag\ttrue\t\n"
            'nothing\tnull\t\nnested\tobject\t\nq\tstring\tsay \\"hi\\"\n'
        )
        result = fields_of(whole)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, expected)
        # Whitespace around the object is the only thing beyond it that passes.
        self.assertEqual(fields_of("\n  " + whole + "\t\n").stdout, expected)
        for text in (
            "",
            "not json\n",
            "a warning line first\n" + whole,  # text before the object
            whole + "and then this\n",  # text after it
            whole + whole,  # a second object
            '[\n  "state"\n]',  # not an object
            '{\n  "state": "none"\n',  # the closing brace never came
            '{\n  "state": "non\n',  # cut inside a string
            '{\n  "nested": {"a": 1}\n',  # the inner one closed, the outer did not
            '{"state": "none"]',  # a bracket where a brace belongs
            '{"state": "none",}',
            "{'state': 'none'}",
            '{"state": truex}',
            '{"state": "running", "pid": 12abc}',
            '{"state": "none", "state": "running"}',  # the same name twice
            '{"state": "run\nning"}',  # a raw control character in a string
            '{"state": "a\\qb"}',  # not an escape
            '{"state": "non\\u0065"}',  # a code point given as an escape
            '{"state":"none","\\u0073tate":"unavailable"}',  # one name, written twice
            '{"a\\"b": 1}',  # a name with an escape in it
        ):
            with self.subTest(text=text):
                result = fields_of(text)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertEqual(result.stdout, "")

    def test_an_escaped_or_nul_bearing_answer_is_not_an_answer(self) -> None:
        # A name written through an escape is one name twice; a state spelled
        # through one is not read; and a NUL byte anywhere is not whitespace.
        for status in (
            b'{"state":"none","\\u0073tate":"unavailable"}',
            b'{"state":"n\\u006fne"}',
            b'{"state":"no\x00ne"}',
            b'{"state":"none"}\x00and then this',
            b'\x00{"state":"none"}',
            b'{"state":"none"}\x00',
        ):
            with self.subTest(status=status):
                self.calls.unlink(missing_ok=True)
                self.status.write_bytes(status)
                result = self.run_bash(
                    self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("cannot tell what holds this DB scope", result.stdout)
                self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_a_name_that_only_collates_like_state_is_not_state(self) -> None:
        # In a UTF-8 locale a name with a soft hyphen or a zero-width space in
        # it collates like `state`. It must neither stand in for a missing
        # state nor hide the real one.
        for status in (
            '{"st\u00adate":"none","state":"unavailable"}',
            '{"st\u00adate":"none"}',
            '{"st\u200bate":"none","state":"unavailable"}',
        ):
            with self.subTest(status=status):
                self.calls.unlink(missing_ok=True)
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(),
                    {"TACHI_FAKE": str(self.fake_bin / "tachi"), "LC_ALL": "en_US.UTF-8"},
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("cannot tell what holds this DB scope", result.stdout)
                self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_a_cut_short_answer_is_not_read_as_free(self) -> None:
        for status in (
            '{"state": "none"',
            '{\n  "state": "none"\n',
            '{\n  "state": "stale_pid", "pid": 4242\n',
            '{"state": "none"]',  # closed with the wrong bracket
            'a warning line\n{"state": "none"}',  # something was printed first
        ):
            with self.subTest(status=status):
                self.calls.unlink(missing_ok=True)
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("cannot tell what holds this DB scope", result.stdout)
                self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_a_nested_or_mistyped_field_is_not_the_status(self) -> None:
        # Each answer is one JSON object. None of them says what a match on
        # the text would find.
        for status, complaint in (
            (
                '{"state": "unavailable", "detail": {"state": "running"}}',
                "cannot tell what holds this DB scope",
            ),
            (
                '{"state": "foreign", "reason": "daemon version 2.0.0 does not match binary 2.1.0",'
                ' "pid": "4242"}',
                "foreign Tachi daemon holds this DB scope",
            ),
            (
                '{"state": "foreign", "reason": {"text": "daemon version 2.0.0 does not match'
                ' binary 2.1.0"}, "pid": 4242}',
                "foreign Tachi daemon holds this DB scope",
            ),
        ):
            with self.subTest(status=status):
                self.calls.unlink(missing_ok=True)
                self.status.write_text(status, encoding="utf-8")
                result = self.run_bash(
                    self.stop_driver(), {"TACHI_FAKE": str(self.fake_bin / "tachi")}
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(complaint, result.stdout)
                self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_a_failing_binary_has_not_answered(self) -> None:
        self.status.write_text('{\n  "state": "none"\n}\n', encoding="utf-8")
        result = self.run_bash(
            self.stop_driver(),
            {"TACHI_FAKE": str(self.fake_bin / "tachi"), "STATUS_EXIT": "3"},
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("cannot tell what holds this DB scope", result.stdout)
        self.assertEqual(self.call_log(), [], "nothing was signalled")

    def test_an_answer_that_breaks_while_waiting_for_the_stop_ends_the_install(self) -> None:
        # The daemon was asked to stop. The next answer is cut short right
        # after its state field, or closed with the wrong bracket. Neither
        # says the scope is free.
        plist = self.launch_agents() / f"{LABEL}.plist"
        previous = self.write_launch_agent(plist, "--global-db", str(self.db))
        self.write_loaded_job("--global-db", str(self.db), pid=7)
        for answer in ('{"state": "none"', '{"state": "none"]'):
            with self.subTest(answer=answer):
                (self.root / "launchctl").unlink(missing_ok=True)
                (self.root / "draining").unlink(missing_ok=True)
                self.status.write_text(running(7), encoding="utf-8")
                result = self.run_bash(
                    self.service_driver(),
                    {
                        "DRAIN_MANAGED_JOB": "1",
                        "STATUS_AFTER_BOOTOUT": answer,
                    },
                )
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertEqual(self.call_log(), [])
                self.assertEqual(plist.read_bytes(), previous)
                self.assertEqual((self.root / "launchctl").read_text().splitlines(),
                                 [f"bootout gui/{os.getuid()}/{LABEL}"])
                self.assertIn("became unreadable while waiting", result.stdout)

    def test_a_nested_running_does_not_end_the_wait_for_the_new_daemon(self) -> None:
        self.launch_agents()
        self.status.write_text('{\n  "state": "none"\n}\n', encoding="utf-8")
        # The service was asked to start. `running` nested under another
        # state is not the new daemon reporting in.
        result = self.run_bash(
            self.service_driver(),
            {"STATUS_AFTER_BOOTSTRAP": '{"state": "none", "detail": {"state": "running"}}'},
        )
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("did not report state=running", result.stdout)

    def test_existing_plist_with_globbing_or_root_directory_is_refused(self) -> None:
        plist = self.launch_agents() / f"{LABEL}.plist"
        self.status.write_text('{"state": "none"}\n', encoding="utf-8")
        for extra in (
            {"EnableGlobbing": True},
            {"EnableGlobbing": "false"},
            {"RootDirectory": "/chroot"},
            {"RootDirectory": 123},
        ):
            with self.subTest(extra=extra):
                (self.root / "launchctl").unlink(missing_ok=True)
                payload = {
                    "Label": LABEL,
                    "ProgramArguments": ["/old/bin/tachi", "--daemon", "--global-db", str(self.db)],
                    **extra,
                }
                previous = plistlib.dumps(payload, fmt=plistlib.FMT_XML)
                plist.write_bytes(previous)
                result = self.run_bash(self.service_driver(), {})
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("Cannot verify existing LaunchAgent plist", result.stderr)
                self.assertEqual(plist.read_bytes(), previous)
                self.assertFalse((self.root / "launchctl").exists())

    def test_unmanaged_daemon_under_inherited_sigil_home_blocks_install(self) -> None:
        self.launch_agents()
        inherited_home = self.root / "alt-sigil-home"
        inherited_home.mkdir()
        # Fake tachi answers `none` for default TACHI_HOME, but reports an
        # unmanaged running daemon when queried under the inherited SIGIL_HOME.
        fake = self.fake_bin / "tachi"
        fake.write_text(
            "#!/bin/sh\n"
            f'if [ "${{TACHI_HOME:-}}" = "{inherited_home}" ]; then\n'
            f"  printf '%s\\n' '{running(88)}'\n"
            "else\n"
            '  printf \'%s\\n\' \'{"state": "none"}\'\n'
            "fi\n",
            encoding="utf-8",
        )
        fake.chmod(0o755)
        env = dict(self.base_env)
        env.pop("TACHI_HOME", None)
        env["SIGIL_HOME"] = str(inherited_home)
        result = subprocess.run(
            ["/bin/bash", "-c", "set -euo pipefail\n" + self.service_driver()],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("outside the verified LaunchAgent", result.stderr)
        self.assertIn("88", result.stderr)
        self.assertFalse((self.root / "launchctl").exists())


if __name__ == "__main__":
    unittest.main()
