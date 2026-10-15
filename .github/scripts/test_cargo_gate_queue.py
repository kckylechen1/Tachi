"""Admission regressions using private inodes and owned fixture Workers."""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import unittest
import test_cargo_gate as fixtures


class CargoQueueTests(unittest.TestCase):
    setUp = fixtures.CargoGateTests.setUp
    tearDown = fixtures.CargoGateTests.tearDown
    fixture_source = fixtures.CargoGateTests.fixture_source
    launch = fixtures.CargoGateTests.launch
    finish = fixtures.CargoGateTests.finish

    def limited_start(self, seconds):
        path = self.root / 'limited-start.sh'
        path.write_text(f'exec bash "{self.scripts / "cargo-gate-ci-start"}" --queue-seconds {seconds}\n')
        return path

    def free_lock(self):
        with (self.gate / 'lock').open() as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return True
            except BlockingIOError:
                return False

    def test_queue_deadline_never_launches_cargo_or_disturbs_owner(self):
        one, first = self.launch('one')
        fixtures.wait_for(lambda: (first / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        two, second = self.launch('two', start=self.limited_start(.7))
        fixtures.wait_for(lambda: (second / 'start-result').exists())
        self.assertEqual((second / 'start-result').read_text().strip(), '75')
        two.wait(timeout=3)
        self.assertIn('infra_blocked', (second / 'output').read_text())
        self.assertFalse((second / 'cargo-started').exists())
        self.assertEqual(json.loads((self.gate / 'holder.json').read_text())['token'], owner['token'])
        self.finish(one, first)
        fixtures.wait_for(self.free_lock)
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_kernel_wait_acquires_while_observer_sleeps_and_retains_custody(self):
        one, first = self.launch('one')
        fixtures.wait_for(lambda: (first / 'cargo-started').exists())
        helper = self.scripts / 'cargo-gate-hold'
        text = helper.read_text().replace('POLL = 0.5', 'POLL = 3.0')
        needle = '    fcntl.flock(fd, fcntl.LOCK_EX)\n'
        self.assertIn(needle, text)
        text = text.replace(needle, "    (GATE_DIR / 'waiter-entered').touch()\n" + needle + "    (GATE_DIR / 'waiter-acquired').touch()\n")
        helper.write_text(text)
        two, second = self.launch('two')
        fixtures.wait_for(lambda: (self.gate / 'waiter-entered').exists())
        time.sleep(.2)
        (first / 'finish').touch()
        fixtures.wait_for(lambda: (self.gate / 'waiter-acquired').exists())
        self.assertFalse((second / 'cargo-started').exists())
        self.assertFalse(self.free_lock(), 'acquisition child exit must retain parent flock custody')
        one.wait(timeout=5)
        fixtures.wait_for(lambda: (second / 'cargo-started').exists(), 10)
        owner = json.loads((self.gate / 'holder.json').read_text())
        self.assertGreater(owner['queue_seconds'], 0)
        self.assertLessEqual(owner['acquired_monotonic'], time.monotonic())
        self.finish(two, second)
        self.assertTrue(self.free_lock())

    def test_queued_worker_exit_reaps_only_its_acquisition_helper(self):
        one, first = self.launch('one')
        fixtures.wait_for(lambda: (first / 'cargo-started').exists())
        helper = self.scripts / 'cargo-gate-hold'
        needle = '    fcntl.flock(fd, fcntl.LOCK_EX)\n'
        source = helper.read_text()
        helper.write_text(source.replace(needle, "    (GATE_DIR / 'waiter-pid').write_text(str(os.getpid()))\n" + needle))
        two, second = self.launch('two')
        fixtures.wait_for(lambda: (self.gate / 'waiter-pid').exists())
        pid = int((self.gate / 'waiter-pid').read_text())
        two.terminate(); two.wait(timeout=3)
        fixtures.wait_for(lambda: self.module.process(pid) is None)
        self.assertFalse((second / 'cargo-started').exists())
        self.assertFalse(self.free_lock())
        self.finish(one, first)
        fixtures.wait_for(self.free_lock)

    def test_killed_waiting_holder_leaves_no_orphan_kernel_waiter(self):
        one, first = self.launch('one')
        fixtures.wait_for(lambda: (first / 'cargo-started').exists())
        helper = self.scripts / 'cargo-gate-hold'
        needle = '    fcntl.flock(fd, fcntl.LOCK_EX)\n'
        source = helper.read_text()
        helper.write_text(source.replace(needle, "    (GATE_DIR / 'waiter-identity').write_text(json.dumps([os.getpid(), os.getppid()]))\n" + needle))
        two, second = self.launch('two')
        fixtures.wait_for(lambda: (self.gate / 'waiter-identity').exists())
        pid, parent = json.loads((self.gate / 'waiter-identity').read_text())
        os.kill(parent, signal.SIGKILL)  # exact owned fixture holder only
        fixtures.wait_for(lambda: self.module.process(pid) is None)
        fixtures.wait_for(lambda: (second / 'start-result').exists())
        self.assertNotEqual((second / 'start-result').read_text().strip(), '0')
        self.assertFalse((second / 'cargo-started').exists())
        self.assertFalse(self.free_lock())
        two.wait(timeout=3)
        self.finish(one, first)
        fixtures.wait_for(self.free_lock)

    def test_invalid_or_expanded_queue_budgets_refuse_before_admission(self):
        for value in ('0', '-1', '901', 'nan', 'inf'):
            with self.subTest(value=value):
                result = subprocess.run([sys.executable, str(self.scripts / 'cargo-gate-hold'), 'start',
                                         '--queue-seconds', value], env=self.env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 70)
                self.assertIn('GATE_E_QUEUE_LIMIT', result.stderr)
                self.assertTrue(self.free_lock())


if __name__ == '__main__':
    unittest.main()
