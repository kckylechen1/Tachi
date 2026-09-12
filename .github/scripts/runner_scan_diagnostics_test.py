"""Execute the real hygiene scripts with synthetic scans and no real signals."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
STUBS = r'''
# Shell functions shadow builtins as well as PATH commands in every child bash.
kill() { printf '%s\n' "$*" >> "$PROBE_SIGNALS"; }
sleep() { :; }
ps() { printf '1\n'; }
df() { printf 'Filesystem 1024-blocks Used Available Capacity Mounted\nfixture 999999999 0 999999999 0%% /\n'; }
lsof() {
  local count=0 candidate=""
  [ ! -f "$PROBE_COUNT" ] || read -r count < "$PROBE_COUNT"
  count=$((count + 1))
  printf '%s\n' "$count" > "$PROBE_COUNT"
  case "$PROBE_CASE:$count" in
    unknown_initial:1|unknown_final:5|unknown_final_exe:6) return 1 ;;
    empty:*) ;;
    *:1) candidate=910001 ;;
    kill_branch:3|kill_branch:5) candidate=910001 ;;
    final_only:5|hygiene:5) candidate=910002 ;;
  esac
  if [ -n "$candidate" ]; then
    printf 'p%s\nfcwd\nn%s\n' "$candidate" "$SCAN_ROOT"
  fi
  return 0
}
'''


class RunnerScanDiagnostics(unittest.TestCase):
    def run_case(self, case, hygiene=False):
        with tempfile.TemporaryDirectory(prefix="runner-scan-fixture-") as raw:
            root = Path(raw)
            workspace = root / "runner" / "_work" / "fixture"
            workspace.mkdir(parents=True)
            workspace = workspace.resolve()
            env_file = root / "stub-env.sh"
            env_file.write_text(STUBS)
            signals = root / "signals"
            env = os.environ.copy()
            env.update(BASH_ENV=str(env_file), PROBE_CASE=case,
                       PROBE_SIGNALS=str(signals), PROBE_COUNT=str(root / "count"),
                       SCAN_ROOT=str(workspace), GITHUB_WORKSPACE=str(workspace),
                       RUNNER_ROOT=str(root / "runner"))
            script = "runner_hygiene.sh" if hygiene else "runner_kill_strays.sh"
            args = ["preflight", "10"] if hygiene else ["kill"]
            result = subprocess.run(["/bin/bash", str(SCRIPTS / script), *args],
                                    env=env, capture_output=True, text=True, timeout=10)
            attempted = signals.read_text().splitlines() if signals.exists() else []
            return result.returncode, result.stdout + result.stderr, attempted

    def test_final_only_candidate_was_not_sent_sigkill(self):
        code, output, signals = self.run_case("final_only")
        self.assertEqual(code, 4)
        self.assertEqual(signals, ["-TERM 910001"])
        self.assertNotIn("escalating to SIGKILL", output)
        self.assertNotIn("survived SIGKILL", output)
        self.assertIn("processes remain in final workspace scan; resolve manually: 910002", output)

    def test_actual_kill_branch_keeps_signal_attempts_and_failure(self):
        code, output, signals = self.run_case("kill_branch")
        self.assertEqual(code, 4)
        self.assertEqual(signals, ["-TERM 910001", "-9 910001"])
        self.assertIn("escalating to SIGKILL: 910001", output)
        self.assertIn("processes remain in final workspace scan; resolve manually: 910001", output)
        self.assertNotIn("survived SIGKILL", output)

    def test_empty_initial_scan_still_succeeds_without_signals(self):
        code, output, signals = self.run_case("empty")
        self.assertEqual(code, 0)
        self.assertEqual(signals, [])
        self.assertIn("no kill candidates", output)

    def test_unknown_initial_scan_still_refuses_without_signals(self):
        code, output, signals = self.run_case("unknown_initial")
        self.assertEqual(code, 5)
        self.assertEqual(signals, [])
        self.assertIn("UNKNOWN, nothing signalled", output)

    def test_unknown_final_scans_do_not_claim_kill_happened(self):
        for case in ("unknown_final", "unknown_final_exe"):
            with self.subTest(case=case):
                code, output, signals = self.run_case(case)
                self.assertEqual(code, 5)
                self.assertEqual(signals, ["-TERM 910001"])
                self.assertIn("final lsof", output)
                self.assertIn("process state UNKNOWN", output)
                self.assertNotIn("post-KILL", output)

    def test_hygiene_propagates_failure_without_previous_job_claim(self):
        code, output, signals = self.run_case("hygiene", hygiene=True)
        self.assertEqual(code, 4)
        self.assertEqual(signals, ["-TERM 910001"])
        self.assertIn("workspace process hygiene unresolved (killer exit 4)", output)
        self.assertIn("refusing to build", output)
        self.assertNotIn("processes from a previous job", output)
        self.assertNotIn("free disk after cleanup", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
