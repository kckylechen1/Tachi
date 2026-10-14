"""Step helper probes using real owned hook leases and process groups."""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import unittest
import test_cargo_gate as fixtures


class CargoStepTests(unittest.TestCase):
    setUp = fixtures.CargoGateTests.setUp
    tearDown = fixtures.CargoGateTests.tearDown
    fixture_source = fixtures.CargoGateTests.fixture_source

    def helper(self):
        source = (fixtures.SOURCE / 'cargo_gate_step.py').read_text()
        target = self.scripts / 'cargo_gate_step.py'
        target.write_text(source.replace("HOOK_ROOT = Path.home() / 'bin'", f'HOOK_ROOT = Path({str(self.scripts)!r})'))
        for name in ('cargo-gate-ci-start', 'cargo-gate-ci-end'):
            (self.scripts / name).chmod(0o755)
        return target

    def launch_step(self, command):
        helper = self.helper()
        driver = self.root / 'step-driver.sh'
        driver.write_text('''#!/bin/bash
python3 "$1" run -- /bin/bash -c "$2" > "$3/output" 2>&1
echo "$?" > "$3/result"
while [ ! -f "$3/finish" ]; do sleep 0.05; done
bash "$4" >> "$3/output" 2>&1
''')
        child = subprocess.Popen([str(self.worker), str(driver), str(helper), command,
                                  str(self.root), str(self.scripts / 'cargo-gate-ci-end')], env=self.env)
        self.children.append(child)
        return child

    def free_lock(self):
        with (self.gate / 'lock').open() as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return True
            except BlockingIOError:
                return False

    def test_live_command_holds_then_releases_without_waiting_for_worker_end(self):
        marker = self.root / 'started'
        child = self.launch_step(f'touch "{marker}"; sleep .5; exit 7')
        fixtures.wait_for(marker.exists)
        self.assertFalse(self.free_lock())
        fixtures.wait_for(lambda: (self.root / 'result').exists())
        self.assertEqual((self.root / 'result').read_text().strip(), '7')
        self.assertTrue(self.free_lock())
        self.assertIsNone(child.poll())
        (self.root / 'finish').touch()
        child.wait(timeout=8)

    def test_same_group_descendant_refuses_release_until_job_cleanup(self):
        child = self.launch_step('sleep .8 & exit 0')
        fixtures.wait_for(lambda: (self.root / 'result').exists())
        self.assertEqual((self.root / 'result').read_text().strip(), '70')
        self.assertIn('descendants remain', (self.root / 'output').read_text())
        self.assertFalse(self.free_lock())
        # Simulate job cleanup after the owned command descendant finishes.
        import time
        time.sleep(1)
        (self.root / 'finish').touch()
        child.wait(timeout=8)
        fixtures.wait_for(self.free_lock)

    def test_missing_installed_hook_refuses_without_command(self):
        helper = self.helper()
        (self.scripts / 'cargo-gate-ci-end').unlink()
        marker = self.root / 'started'
        result = subprocess.run([sys.executable, str(helper), 'run', '--', 'touch', str(marker)], env=self.env,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 70)
        self.assertFalse(marker.exists())
        self.assertTrue(self.free_lock())

    def test_light_or_unknown_jobs_cannot_run_ungated_commands(self):
        helper = self.helper()
        for job in ('node', 'future_rust_job'):
            with self.subTest(job=job):
                marker = self.root / job
                result = subprocess.run([sys.executable, str(helper), 'run', '--', 'touch', str(marker)],
                    env=dict(self.env, GITHUB_JOB=job), capture_output=True, text=True)
                self.assertEqual(result.returncode, 70)
                self.assertFalse(marker.exists())

    def test_interrupted_helper_retains_lock_for_job_cleanup(self):
        marker = self.root / 'started'
        anchor_pid = self.root / 'anchor'
        child = self.launch_step(f'echo "$PPID" > "{anchor_pid}"; touch "{marker}"; sleep .7')
        fixtures.wait_for(marker.exists)
        parent = subprocess.check_output(['ps', '-p', anchor_pid.read_text().strip(), '-o', 'ppid='], text=True)
        os.kill(int(parent.strip()), signal.SIGTERM)
        fixtures.wait_for(lambda: (self.root / 'result').exists())
        self.assertEqual((self.root / 'result').read_text().strip(), '70')
        self.assertFalse(self.free_lock())
        (self.root / 'finish').touch()
        child.wait(timeout=8)
        fixtures.wait_for(self.free_lock)

    def test_workflow_wraps_absolute_nextest_and_all_compiling_commands(self):
        source = (fixtures.SOURCE.parent / 'workflows/ci.yml').read_text()
        rust = source.split('  rust:', 1)[1].split('  physical-db-identity-windows:', 1)[0]
        for command in ('cargo clippy --workspace --all-targets --locked -- -D warnings',
                        '"${AUDITED_NEXTEST:?}" nextest run --workspace --locked --profile ci',
                        'cargo test -p portable-kernel --features portable-contract-test --locked',
                        'cargo test --workspace --locked --doc'):
            self.assertIn('python3 .github/scripts/cargo_gate_step.py run -- ' + command, rust)
        self.assertLess(rust.index('Setup Rust'), rust.index('cargo_gate_step.py release'))
        self.assertLess(rust.index('cargo_gate_step.py release'), rust.index('Run clippy'))


if __name__ == '__main__':
    unittest.main()
