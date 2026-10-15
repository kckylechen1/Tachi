"""Step budget probes with real owned hooks, private locks and process groups."""
import fcntl
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time
import threading
import unittest
import test_cargo_gate as fixtures


class CargoStepTests(unittest.TestCase):
    tearDown = fixtures.CargoGateTests.tearDown

    def setUp(self):
        fixtures.CargoGateTests.setUp(self)
        self.runner_temp = self.root / 'runner-temp'
        self.runner_temp.mkdir()
        self.env['RUNNER_TEMP'] = str(self.runner_temp)
        self.queue_budget, self.exec_budget = 900.0, 1800.0

    def fixture_source(self, name):
        return fixtures.CargoGateTests.fixture_source(self, name).replace('POLL = 0.5', 'POLL = 0.02')

    def helper(self):
        source = (fixtures.SOURCE / 'cargo_gate_step.py').read_text()
        target = self.scripts / 'cargo_gate_step.py'
        source = source.replace("HOOK_ROOT = Path.home() / 'bin'", f'HOOK_ROOT = Path({str(self.scripts)!r})')
        source = source.replace("'Runner.Worker'", repr(self.worker.name))
        source = source.replace('QUEUE_BUDGET = 900.0', f'QUEUE_BUDGET = {self.queue_budget!r}')
        source = source.replace('EXEC_BUDGET = 1800.0', f'EXEC_BUDGET = {self.exec_budget!r}')
        source = source.replace('TERM_GRACE = 2.0', 'TERM_GRACE = 0.2')
        target.write_text(source)
        for name in ('cargo-gate-ci-start', 'cargo-gate-ci-end'):
            (self.scripts / name).chmod(0o755)
        return target

    @staticmethod
    def command(command, suffix=''):
        return ('python3 "$1" run -- /bin/bash -c ' + shlex.quote(command) +
                ' >> "$2/output" 2>&1\necho "$?" > "$2/result' + suffix + '"\n')

    def launch_driver(self, body, pause=False, setup_delay=0, expected_initial=0):
        helper = self.helper()
        driver = self.root / 'step-driver.sh'
        driver.write_text('''#!/bin/bash
bash "$3" > "$2/output" 2>&1
initial=$?
if [ "$initial" -eq 0 ]; then
  sleep "$5"
  python3 "$1" release >> "$2/output" 2>&1
  initial=$?
fi
echo "$initial" > "$2/initial-result"
if [ "$initial" -ne 0 ]; then exit "$initial"; fi
''' + ('while [ ! -f "$2/begin" ]; do sleep .02; done\n' if pause else '') + body + '''
while [ ! -f "$2/finish" ]; do sleep .02; done
bash "$4" >> "$2/output" 2>&1
''')
        child = subprocess.Popen([str(self.worker), str(driver), str(helper), str(self.root),
                                  str(self.scripts / 'cargo-gate-ci-start'),
                                  str(self.scripts / 'cargo-gate-ci-end'), str(setup_delay)], env=self.env)
        self.children.append(child)
        fixtures.wait_for(lambda: (self.root / 'initial-result').exists())
        self.assertEqual(int((self.root / 'initial-result').read_text()), expected_initial, self.output())
        return child

    def launch_step(self, command, pause=False):
        return self.launch_driver(self.command(command), pause=pause)

    def output(self):
        return (self.root / 'output').read_text()

    def result(self, suffix=''):
        path = self.root / ('result' + suffix)
        fixtures.wait_for(path.exists)
        return int(path.read_text())

    def budget(self):
        return json.loads((self.runner_temp / 'cargo-gate-step-budget.json').read_text())

    def finish(self, child):
        (self.root / 'finish').touch()
        child.wait(timeout=8)
        fixtures.wait_for(self.free_lock)

    def blocker(self, label='blocker'):
        child, directory = fixtures.CargoGateTests.launch(self, label)
        fixtures.wait_for(lambda: (directory / 'cargo-started').exists())
        return child, directory

    def wait_queued(self):
        holder_pid = json.loads((self.gate / 'holder.json').read_text())['pid']
        fixtures.wait_for(lambda: f'\"observed_holder_pid\": {holder_pid}' in self.output())

    def unblock(self, child, directory):
        fixtures.CargoGateTests.finish(self, child, directory)

    def free_lock(self):
        with (self.gate / 'lock').open() as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return True
            except BlockingIOError:
                return False

    def assert_dead(self, pid):
        result = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True)
        self.assertTrue(result.returncode == 1 or result.stdout.strip().startswith('Z'), result.stdout)

    def test_live_command_holds_then_releases_without_waiting_for_worker_end(self):
        marker = self.root / 'started'
        child = self.launch_step(f'touch "{marker}"; sleep .5; exit 7')
        fixtures.wait_for(marker.exists)
        self.assertFalse(self.free_lock())
        self.assertEqual(self.result(), 7, self.output())
        self.assertTrue(self.free_lock())
        self.assertIsNone(child.poll())
        self.assertGreaterEqual(self.budget()['execution_seconds'], .5)
        self.finish(child)

    def test_same_group_descendant_refuses_release_until_job_cleanup(self):
        pid = self.root / 'descendant'
        child = self.launch_step(f'sleep 30 & echo $! > "{pid}"; exit 0')
        self.assertEqual(self.result(), 70, self.output())
        self.assertIn('descendants remain', self.output())
        self.assertFalse(self.free_lock())
        self.assert_dead(int(pid.read_text()))
        self.finish(child)

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

    def test_interrupted_helper_drains_owned_children_and_retains_lease_for_cleanup(self):
        marker = self.root / 'started'
        anchor_pid = self.root / 'anchor'
        command_pid = self.root / 'command-pid'
        child = self.launch_step(f'echo "$PPID" > "{anchor_pid}"; echo $$ > "{command_pid}"; touch "{marker}"; sleep 30')
        fixtures.wait_for(marker.exists)
        parent = subprocess.check_output(['ps', '-p', anchor_pid.read_text().strip(), '-o', 'ppid='], text=True)
        os.kill(int(parent.strip()), signal.SIGTERM)
        self.assertEqual(self.result(), 70, self.output())
        self.assertFalse(self.free_lock())
        self.assert_dead(int(command_pid.read_text()))
        self.assertEqual(self.budget()['status'], 'interrupted')
        self.finish(child)

    def test_initial_setup_queue_and_execution_are_imported_once(self):
        blocker, directory = self.blocker()
        timer = threading.Timer(1.0, (directory / 'finish').touch)
        timer.start()
        self.addCleanup(timer.join)
        child = self.launch_driver(self.command('true'), pause=True, setup_delay=.2)
        value = self.budget()
        self.assertGreaterEqual(value['execution_seconds'], .2)
        self.assertGreaterEqual(value['queue_seconds'], .6)
        self.unblock(blocker, directory)
        self.assertEqual(value['identity']['worker']['pid'], child.pid)
        self.assertEqual((self.runner_temp / 'cargo-gate-step-budget.json').stat().st_mode & 0o777, 0o600)
        (self.root / 'begin').touch()
        self.assertEqual(self.result(), 0, self.output())
        self.finish(child)

    def test_setup_already_consumed_execution_budget_refuses_after_release(self):
        self.exec_budget = .2
        child = self.launch_driver('', setup_delay=.3, expected_initial=124)
        child.wait(timeout=8)
        self.assertIn('GATE_E_EXEC_TIMEOUT', self.output())
        self.assertEqual(self.budget()['status'], 'execution_timeout')
        self.assertGreaterEqual(self.budget()['execution_seconds'], .3)
        self.assertTrue(self.free_lock())

    def assert_release_timeout(self, command_code):
        self.exec_budget = 5.0
        marker = self.root / 'delay-end'
        end = self.scripts / 'cargo-gate-ci-end'
        end.write_text(end.read_text().replace('#!/bin/bash',
                       '#!/bin/bash\nif [ -f ' + shlex.quote(str(marker)) + ' ]; then sleep 5; fi'))
        child = self.launch_driver(self.command(f'touch "{marker}"; exit {command_code}') + self.command('true', 'later'))
        fixtures.wait_for(marker.exists)
        execution_before = self.budget()['execution_seconds']
        acquired = json.loads((self.gate / 'holder.json').read_text())['acquired_monotonic']
        self.assertEqual(self.result(), command_code or 124, self.output())
        self.assertEqual(self.result('later'), 70, self.output())
        if command_code == 0:
            self.assertIn('GATE_E_EXEC_TIMEOUT', self.output())
        self.assertEqual(self.budget()['status'], 'execution_timeout')
        self.assertGreaterEqual(self.budget()['execution_seconds'], self.exec_budget)
        # Charging this lease twice exceeds the elapsed time since its actual
        # acquisition, independent of setup/process-scheduling overhead.
        self.assertLessEqual(self.budget()['execution_seconds'],
                             execution_before + time.monotonic() - acquired + .05)
        self.assertTrue(self.free_lock())
        self.finish(child)

    def test_final_successful_command_cannot_hide_budget_exhausted_before_release(self):
        self.assert_release_timeout(0)

    def test_failed_command_preserves_exit_code_when_release_exhausts_budget(self):
        self.assert_release_timeout(7)

    def test_waiting_is_cumulative_queue_not_execution_time(self):
        self.queue_budget, self.exec_budget = 5.0, 4.0
        child = self.launch_step('sleep 2', pause=True)
        initial = self.budget()
        blocker, directory = self.blocker()
        (self.root / 'begin').touch()
        self.wait_queued()
        time.sleep(3)
        self.unblock(blocker, directory)
        self.assertEqual(self.result(), 0, self.output())
        value = self.budget()
        self.assertGreaterEqual(value['queue_seconds'] - initial['queue_seconds'], 2.8)
        self.assertGreaterEqual(value['execution_seconds'] - initial['execution_seconds'], 2)
        self.assertLess(value['execution_seconds'], self.exec_budget)
        entries = [json.loads(line.split(': ', 1)[1]) for line in self.output().splitlines()
                   if line.startswith('cargo-gate-budget: ')]
        self.assertEqual([entry['phase'] for entry in entries], ['initialized', 'admitted', 'completed'])
        self.assertGreater(entries[-1]['queue_total_seconds'], 2.8)
        self.assertLess(entries[-1]['execution_total_seconds'], self.exec_budget)
        self.finish(child)

    def test_cumulative_execution_limit_drains_term_resistant_group(self):
        self.exec_budget = 6.0
        pid = self.root / 'command-pid'
        body = self.command('sleep 2', '1') + 'while [ ! -f "$2/begin2" ]; do sleep .02; done\n' + self.command(
            f'trap "" TERM; echo $$ > "{pid}"; sleep 30', '2') + self.command('true', '3')
        child = self.launch_driver(body)
        self.assertEqual(self.result('1'), 0, self.output())
        first_execution = self.budget()['execution_seconds']
        self.assertGreaterEqual(first_execution, 2.0)
        remaining = self.exec_budget - first_execution
        began = time.monotonic()
        (self.root / 'begin2').touch()
        self.assertEqual(self.result('2'), 124, self.output())
        self.assertLess(time.monotonic() - began, remaining + 1.5)
        self.assertEqual(self.result('3'), 70, self.output())
        self.assertIn('GATE_E_EXEC_TIMEOUT', self.output())
        self.assertTrue(self.free_lock())
        self.assert_dead(int(pid.read_text()))
        self.assertGreaterEqual(self.budget()['execution_seconds'], self.exec_budget)
        self.assertEqual(self.budget()['status'], 'execution_timeout')
        self.finish(child)

    def test_cumulative_queue_timeout_never_starts_or_retries_command(self):
        self.queue_budget = 3.0
        marker = self.root / 'never-started'
        body = self.command('true', '1') + 'while [ ! -f "$2/begin2" ]; do sleep .02; done\n'
        body += self.command(f'touch "{marker}"', '2') + self.command(f'touch "{marker}"', '3')
        child = self.launch_driver(body, pause=True)
        blocker, directory = self.blocker('blocker1')
        (self.root / 'begin').touch()
        self.wait_queued()
        time.sleep(1.2)
        self.unblock(blocker, directory)
        self.assertEqual(self.result('1'), 0, self.output())
        self.assertGreater(self.budget()['queue_seconds'], 1.0)
        blocker, directory = self.blocker('blocker2')
        (self.root / 'begin2').touch()
        self.assertEqual(self.result('2'), 75, self.output())
        self.assertEqual(self.result('3'), 70, self.output())
        self.assertIn('GATE_E_QUEUE_TIMEOUT', self.output())
        self.assertIn('infra_blocked', self.output())
        self.assertFalse(marker.exists())
        self.assertEqual(self.budget()['status'], 'queue_timeout')
        self.assertFalse(self.free_lock())  # The foreign fixture owner remains untouched.
        self.unblock(blocker, directory)
        self.finish(child)

    def test_cancelled_waiting_cleans_pending_holder_and_refuses_retry(self):
        holder = self.scripts / 'cargo-gate-hold'
        source = holder.read_text()
        boundary = 'finally:\n        os.close(log_fd)'
        self.assertIn(boundary, source)
        holder.write_text(source.replace(boundary, 'finally:\n        time.sleep(.3)\n        os.close(log_fd)'))
        marker = self.root / 'never-started'
        body = ('python3 "$1" run -- touch ' + shlex.quote(str(marker)) + ' >> "$2/output" 2>&1 &\n'
                'step=$!\necho "$step" > "$2/helper-pid"\nwait "$step"\necho "$?" > "$2/result"\n')
        body += self.command(f'touch "{marker}"', '2')
        child = self.launch_driver(body, pause=True)
        blocker, directory = self.blocker()
        (self.root / 'begin').touch()
        fixtures.wait_for(lambda: (self.root / 'helper-pid').exists())
        fixtures.wait_for(lambda: self.budget()['status'] == 'waiting')
        time.sleep(.2)
        helper_pid = int((self.root / 'helper-pid').read_text())
        os.kill(helper_pid, signal.SIGTERM)
        time.sleep(.05)
        os.kill(helper_pid, signal.SIGINT)  # Must not interrupt child-hook cleanup.
        self.assertEqual(self.result(), 70, self.output())
        self.assertEqual(self.result('2'), 70, self.output())
        self.assertFalse(marker.exists())
        leases = [json.loads(path.read_text()) for path in self.gate.glob('ci-lease.*')
                  if not path.name.endswith('.stop')]
        self.assertEqual([lease['worker']['pid'] for lease in leases], [blocker.pid])
        self.unblock(blocker, directory)
        self.finish(child)

    def test_worker_identity_mismatch_refuses_before_admission(self):
        marker = self.root / 'never-started'
        child = self.launch_step(f'touch "{marker}"', pause=True)
        value = self.budget()
        value['identity']['worker']['start'] = 'not this worker'
        (self.runner_temp / 'cargo-gate-step-budget.json').write_text(json.dumps(value))
        (self.root / 'begin').touch()
        self.assertEqual(self.result(), 70, self.output())
        self.assertFalse(marker.exists())
        self.assertTrue(self.free_lock())
        self.finish(child)

    def test_missing_malformed_and_unfinished_budget_refuse_before_admission(self):
        marker = self.root / 'never-started'
        body = ''
        for index in range(6):
            body += f'while [ ! -f "$2/begin{index}" ]; do sleep .02; done\n'
            body += self.command(f'touch "{marker}"', str(index))
        child = self.launch_driver(body)
        path = self.runner_temp / 'cargo-gate-step-budget.json'
        initial = path.read_text()
        variants = [None, '{', initial.replace('"ready"', '"waiting"'), initial.replace('"ready"', '"running"'),
                    initial.replace('"protocol": 1', '"protocol": true'),
                    initial.replace('"protocol": 1', '"protocol": 1, "protocol": 1')]
        for index, contents in enumerate(variants):
            if contents is None:
                path.unlink()
            else:
                path.write_text(contents)
                path.chmod(0o600)
            (self.root / f'begin{index}').touch()
            self.assertEqual(self.result(str(index)), 70, self.output())
            self.assertFalse(marker.exists())
            self.assertTrue(self.free_lock())
        self.finish(child)

    def test_duplicate_initialization_cannot_reset_counters(self):
        body = ('bash "$3" >> "$2/output" 2>&1\n'
                'python3 "$1" release >> "$2/output" 2>&1\necho "$?" > "$2/result"\n')
        child = self.launch_driver(body, pause=True)
        initial = self.budget()
        (self.root / 'begin').touch()
        self.assertEqual(self.result(), 70, self.output())
        self.assertEqual(self.budget(), initial)
        self.assertFalse(self.free_lock())
        self.finish(child)

    def test_missing_installed_snapshot_capability_fails_closed(self):
        holder = self.scripts / 'cargo-gate-hold'
        holder.write_text(holder.read_text().replace("'snapshot'", "'no-snapshot'"))
        helper = self.helper()
        marker = self.root / 'never-started'
        driver = self.root / 'old-driver.sh'
        driver.write_text('bash "$2"; python3 "$1" release; result=$?; bash "$3"; exit "$result"\n')
        result = subprocess.run([str(self.worker), str(driver), str(helper),
                                 str(self.scripts / 'cargo-gate-ci-start'),
                                 str(self.scripts / 'cargo-gate-ci-end')], env=self.env,
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 70, result.stdout + result.stderr)
        self.assertIn('GATE_E_STEP_SNAPSHOT', result.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((self.runner_temp / 'cargo-gate-step-budget.json').exists())

    def test_workflow_wraps_absolute_nextest_and_all_compiling_commands(self):
        source = (fixtures.SOURCE.parent / 'workflows/ci.yml').read_text()
        rust = source.split('  rust:', 1)[1].split('  physical-db-identity-windows:', 1)[0]
        self.assertIn('timeout-minutes: 50', rust)
        for command in ('cargo clippy --workspace --all-targets --locked -- -D warnings',
                        '"${AUDITED_NEXTEST:?}" nextest run --workspace --locked --profile ci',
                        'cargo test -p portable-kernel --features portable-contract-test --locked',
                        'cargo test --workspace --locked --doc'):
            self.assertIn('python3 .github/scripts/cargo_gate_step.py run -- ' + command, rust)
        self.assertLess(rust.index('Setup Rust'), rust.index('cargo_gate_step.py release'))
        self.assertLess(rust.index('cargo_gate_step.py release'), rust.index('Run clippy'))
        self.assertIn('if: always()', rust)


if __name__ == '__main__':
    unittest.main()
