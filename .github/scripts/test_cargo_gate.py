"""Real hook/holder/flock probes on owned paths; never touch the host gate.

Gate paths and the expected Worker basename are isolated in script copies.
The fixture Worker is a link to system bash; real ps ancestry, identity and
flock checks execute. Named faults are injected only into owned copies.
"""
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).parent


def wait_for(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError('fixture did not reach expected state')


class CargoGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='tachi-cargo-gate-test-')
        self.root = Path(self.temp.name)
        self.gate = self.root / 'gate'
        self.gate.mkdir()
        (self.gate / 'lock').touch()
        self.scripts = self.root / 'bin'
        self.scripts.mkdir()
        # Linux comm is limited to 15 bytes. A per-fixture name also prevents
        # an enclosing real CI Runner.Worker from satisfying a missing Worker.
        self.worker = self.root / ('TGW' + hashlib.sha256(str(self.root).encode()).hexdigest()[:12])
        self.worker.symlink_to('/bin/bash')
        for name in ('cargo-gate-ci-start', 'cargo-gate-ci-end', 'cargo-gate-hold'):
            (self.scripts / name).write_text(self.fixture_source(name))
        (self.scripts / 'cargo-gate-policy.json').write_text(json.dumps({
            'schema': 1, 'light_jobs': [{'repository': 'owner/Tachi',
            'workflow': 'Full CI', 'jobs': ['node', 'gitleaks', 'acceptance', 'build-seat-setup']}]}))
        self.children = []
        self.env = dict(os.environ, GITHUB_REPOSITORY='owner/Tachi', GITHUB_WORKFLOW='Full CI',
                        GITHUB_JOB='rust', GITHUB_RUN_ID='1234', GITHUB_RUN_ATTEMPT='1',
                        CARGO_GATE='off', BUILDGATE_HELD=str(os.getpid()))
        loader = importlib.machinery.SourceFileLoader('gate_fixture', str(self.scripts / 'cargo-gate-hold'))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        self.module = importlib.util.module_from_spec(spec)
        loader.exec_module(self.module)

    def fixture_source(self, name):
        text = (SOURCE / name).read_text()
        text = text.replace("GATE_DIR = Path('/tmp/cargo-gate')", f'GATE_DIR = Path({str(self.gate)!r})')
        text = text.replace("wrapper = str(Path.home() / 'bin' / 'cargo')", f'wrapper = {str(self.scripts / "cargo")!r}')
        return text.replace("'Runner.Worker'", repr(self.worker.name))

    def tearDown(self):
        # Only fixture-owned workers are signalled. Holders observe their exit.
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for _ in range(40):
            if not list(self.gate.glob('ci-lease.*')):
                break
            time.sleep(0.05)
        if (self.gate / 'lock').exists() and not (self.gate / 'lock').is_symlink():
            (self.gate / 'lock').chmod(0o600)
        self.temp.cleanup()

    def launch(self, label='one', start=None, worker=None):
        directory = self.root / label
        directory.mkdir()
        driver = directory / 'driver.sh'
        driver.write_text('''#!/bin/bash
bash "$1" > "$2/output" 2>&1 &
hook=$!
echo "$hook" > "$2/hook-pid"
wait "$hook"
result=$?
echo "$result" > "$2/start-result"
if [ "$result" -ne 0 ]; then exit "$result"; fi
# The runner would begin Cargo only after the job-started hook succeeds.
echo "fake cargo" > "$2/cargo-started"
while [ ! -f "$2/finish" ]; do sleep 0.05; done
bash "$3" >> "$2/output" 2>&1
echo "$?" > "$2/end-result"
''')
        child = subprocess.Popen([str(worker or self.worker), str(driver), str(start or self.scripts / 'cargo-gate-ci-start'),
                                  str(directory), str(self.scripts / 'cargo-gate-ci-end')],
                                 env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.children.append(child)
        return child, directory

    def denied(self, expected=None):
        child, directory = self.launch()
        wait_for(lambda: (directory / 'start-result').exists())
        self.assertNotEqual((directory / 'start-result').read_text().strip(), '0')
        child.wait(timeout=3)
        self.assertFalse((directory / 'cargo-started').exists())
        self.assertIn(expected or 'GATE_E_', (directory / 'output').read_text())
        return directory

    def finish(self, child, directory):
        (directory / 'finish').touch()
        child.wait(timeout=12)
        self.assertEqual((directory / 'end-result').read_text().strip(), '0', (directory / 'output').read_text())

    def test_missing_lock_never_starts_cargo_or_creates_lock(self):
        (self.gate / 'lock').unlink()
        self.denied('GATE_E_LOCK')
        self.assertFalse((self.gate / 'lock').exists())

    def test_permission_failure_never_starts_cargo(self):
        (self.gate / 'lock').chmod(0)
        self.denied('GATE_E_LOCK')

    def test_nonregular_symlink_and_nonempty_lock_refuse(self):
        lock = self.gate / 'lock'
        for shape in ('directory', 'symlink', 'content'):
            with self.subTest(shape=shape):
                lock.unlink()
                if shape == 'directory':
                    lock.mkdir()
                elif shape == 'symlink':
                    target = self.gate / 'other'
                    target.touch()
                    lock.symlink_to(target)
                else:
                    lock.write_text('bad')
                self.denied('GATE_E_LOCK')
                # Each subcase has independent worker output.
                shutil.rmtree(self.root / 'one')
                if shape == 'directory':
                    lock.rmdir()
                else:
                    lock.unlink()
                lock.touch()

    def test_missing_holder_and_child_exec_failure_refuse(self):
        helper = self.scripts / 'cargo-gate-hold'
        helper.unlink()
        self.denied('GATE_E_HOLDER_EXEC')

    def test_nonregular_holder_and_log_refuse_without_blocking(self):
        helper = self.scripts / 'cargo-gate-hold'
        helper.unlink()
        os.mkfifo(helper)
        self.denied('GATE_E_HOLDER_EXEC')
        helper.unlink()
        shutil.rmtree(self.root / 'one')
        helper.write_text(self.fixture_source('cargo-gate-hold'))
        os.mkfifo(self.gate / 'ci-holder.log')
        self.denied('GATE_E_')

    def test_holder_startup_failure_refuses(self):
        text = (self.scripts / 'cargo-gate-hold').read_text()
        text = text.replace("if args.action == 'hold':", "if args.action == 'hold':\n        sys.exit(71)")
        (self.scripts / 'cargo-gate-hold').write_text(text)
        self.denied('GATE_E_HOLDER_START')
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_forged_readiness_without_flock_refuses(self):
        helper = self.scripts / 'cargo-gate-hold'
        source = helper.read_text()
        needle = '        timing = acquire(fd, worker, stop, token, queue_seconds)'
        self.assertIn(needle, source)
        helper.write_text(source.replace(needle, "        timing = {'queue_seconds': 0, 'acquired_monotonic': time.monotonic()} # forge readiness without flock"))
        self.denied('GATE_E_ACK')
        self.assertFalse((self.gate / 'holder.json').exists())
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_unknown_kernel_holder_and_malformed_owner_refuse(self):
        with (self.gate / 'lock').open('r+') as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            (self.gate / 'holder.json').write_text('{bad json')
            directory = self.denied('GATE_E_HOLDER_START')
            self.assertIn('GATE_E_STATE', (self.gate / 'ci-holder.log').read_text())
            self.assertEqual((self.gate / 'holder.json').read_text(), '{bad json')
            self.assertFalse((directory / 'cargo-started').exists())

    def test_unlocked_conflicting_owner_refuses(self):
        (self.gate / 'holder.json').write_text(json.dumps({'pid': os.getpid()}))
        self.denied('GATE_E_HOLDER_START')

    def test_missing_worker_refuses_despite_environment_bypass_flags(self):
        result = subprocess.run(['bash', str(self.scripts / 'cargo-gate-ci-start')],
                                env=self.env, capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('GATE_E_WORKER', result.stderr)

    def test_missing_fixture_worker_refuses_beneath_real_runner_named_ancestor(self):
        runner = self.root / 'Runner.Worker'
        runner.symlink_to('/bin/bash')
        child, directory = self.launch(worker=runner)
        wait_for(lambda: (directory / 'start-result').exists())
        self.assertNotEqual((directory / 'start-result').read_text().strip(), '0')
        child.wait(timeout=3)
        self.assertIn('GATE_E_WORKER', (directory / 'output').read_text())
        self.assertFalse((directory / 'cargo-started').exists())
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_production_worker_basename_admits_owned_runner(self):
        helper = self.scripts / 'cargo-gate-hold'
        helper.write_text(helper.read_text().replace(repr(self.worker.name), "'Runner.Worker'"))
        runner = self.root / 'Runner.Worker'
        runner.symlink_to('/bin/bash')
        child, directory = self.launch(worker=runner)
        wait_for(lambda: (directory / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        self.assertEqual(owner['worker']['pid'], child.pid)
        self.assertEqual(Path(owner['worker']['command']).name, 'Runner.Worker')
        self.finish(child, directory)

    def test_actual_lock_failure_is_typed(self):
        with (self.gate / 'lock').open('r+') as held, patch.object(self.module.fcntl, 'flock', side_effect=OSError(5, 'fixture')):
            with self.assertRaisesRegex(self.module.GateError, 'GATE_E_LOCK'):
                self.module.try_lock(held.fileno())

    def test_single_owner_and_two_jobs_serialize_on_same_inode(self):
        identity = (self.gate / 'lock').stat().st_ino
        one, first = self.launch('one')
        wait_for(lambda: (first / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        self.assertEqual(owner['worker']['pid'], one.pid)
        with (self.gate / 'lock').open('r+') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        two, second = self.launch('two')
        time.sleep(0.8)
        self.assertFalse((second / 'cargo-started').exists())
        self.finish(one, first)
        wait_for(lambda: (second / 'cargo-started').exists())
        next_owner = json.loads((self.gate / 'holder.json').read_text())
        self.assertEqual(next_owner['worker']['pid'], two.pid)
        self.assertNotEqual(owner['token'], next_owner['token'])
        # A repeated completion of the first job cannot release the successor.
        self.module.end(owner['worker'], owner['job'])
        self.assertEqual(json.loads((self.gate / 'holder.json').read_text())['token'], next_owner['token'])
        self.finish(two, second)
        self.assertEqual((self.gate / 'lock').stat().st_ino, identity)
        self.assertFalse((self.gate / 'holder.json').exists())
        self.assertFalse((self.gate / 'ci-holder.pid').exists())
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_live_local_legacy_owner_queues_on_same_lock(self):
        wrapper = self.scripts / 'cargo'
        wrapper.write_text('''import fcntl,json,os,sys,time
from pathlib import Path
root=Path(sys.argv[1])
with (root/'lock').open('r+') as lock:
 fcntl.flock(lock,fcntl.LOCK_EX)
 (root/'holder.json').write_text(json.dumps({'pid':os.getpid(),'argv':'cargo build','cwd':str(root),'since':'fixture'}))
 (root/'local-ready').touch()
 while not (root/'local-finish').exists(): time.sleep(.05)
 (root/'holder.json').unlink()
''')
        local = subprocess.Popen(['python3', str(wrapper), str(self.gate)])
        self.children.append(local)
        wait_for(lambda: (self.gate / 'local-ready').exists())
        child, directory = self.launch()
        time.sleep(0.8)
        self.assertFalse((directory / 'cargo-started').exists())
        (self.gate / 'local-finish').touch()
        local.wait(timeout=3)
        wait_for(lambda: (directory / 'cargo-started').exists())
        self.finish(child, directory)

    def test_worker_interruption_cleans_owned_lease(self):
        child, directory = self.launch()
        wait_for(lambda: (directory / 'cargo-started').exists())
        child.terminate()
        child.wait(timeout=3)
        wait_for(lambda: not (self.gate / 'holder.json').exists())
        self.assertFalse((self.gate / 'ci-holder.pid').exists())
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_queued_start_interruption_never_starts_cargo(self):
        one, first = self.launch('one')
        wait_for(lambda: (first / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        two, second = self.launch('two')
        wait_for(lambda: (second / 'hook-pid').exists())
        time.sleep(0.4)
        os.kill(int((second / 'hook-pid').read_text()), signal.SIGTERM)
        wait_for(lambda: (second / 'start-result').exists())
        two.wait(timeout=3)
        self.assertNotEqual((second / 'start-result').read_text().strip(), '0')
        self.assertFalse((second / 'cargo-started').exists())
        self.assertEqual(json.loads((self.gate / 'holder.json').read_text())['token'], owner['token'])
        self.finish(one, first)
        self.assertFalse(list(self.gate.glob('ci-lease.*')))

    def test_unknown_stop_and_holder_signal_retain_live_worker_lock(self):
        child, directory = self.launch()
        wait_for(lambda: (directory / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        lease = self.module.lease_path(owner['worker'], owner['job'])
        stop = lease.with_name(lease.name + '.stop')
        stop.write_text('{bad}')
        os.kill(owner['pid'], signal.SIGTERM)
        time.sleep(0.4)
        with (self.gate / 'lock').open('r+') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        stop.unlink()
        self.finish(child, directory)

    def test_failed_process_inspection_and_logging_retain_active_flock(self):
        helper = self.scripts / 'cargo-gate-hold'
        text = helper.read_text().replace(
            '    if result.returncode == 1 and not result.stdout.strip():',
            "    marker = GATE_DIR / 'probe-failure-pid'\n"
            "    if marker.exists() and marker.read_text().strip() == 'timeout:' + str(pid):\n"
            "        raise subprocess.TimeoutExpired('ps', 5)\n"
            "    if marker.exists() and marker.read_text().strip() == str(pid):\n"
            "        result = subprocess.CompletedProcess([], 1, '', 'ps: malloc failed')\n"
            '    if result.returncode == 1 and not result.stdout.strip():')
        text = text.replace('\nPOLL = ', "\n_original_print = print\n"
                            "def print(*args, **kwargs):\n"
                            "    if (GATE_DIR / 'fail-holder-log').exists():\n"
                            "        raise OSError(errno.ENOSPC, 'fixture full log')\n"
                            "    return _original_print(*args, **kwargs)\n\nPOLL = ")
        helper.write_text(text)
        child, directory = self.launch()
        wait_for(lambda: (directory / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        (self.gate / 'probe-failure-pid').write_text(str(child.pid))
        (self.gate / 'fail-holder-log').touch()
        time.sleep(0.8)
        os.kill(owner['pid'], signal.SIGTERM)
        time.sleep(0.2)
        os.kill(child.pid, 0)
        with (self.gate / 'lock').open('r+') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(json.loads((self.gate / 'holder.json').read_text())['token'], owner['token'])
        # A process-probe timeout plus ENOSPC must also retain ownership;
        # this independently exercises the best-effort diagnostic handler.
        (self.gate / 'probe-failure-pid').write_text('timeout:' + str(child.pid))
        time.sleep(0.8)
        with (self.gate / 'lock').open('r+') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        (self.gate / 'probe-failure-pid').unlink()
        (self.gate / 'fail-holder-log').unlink()
        self.finish(child, directory)

    def test_signals_during_buffered_diagnostics_retain_live_worker_flock(self):
        helper = self.scripts / 'cargo-gate-hold'
        # Signal delivery happens inside the real BufferedWriter's raw write,
        # while its internal lock is held. The old Python handler reenters it.
        fixture = '''
import io
if len(sys.argv) > 1 and sys.argv[1] == 'hold':
    class SignalDuringWrite(io.RawIOBase):
        fired = False
        output_fd = sys.stderr.fileno()
        def writable(self):
            return True
        def write(self, data):
            if not self.fired and (GATE_DIR / 'signal-during-write').exists():
                self.fired = True
                (GATE_DIR / 'signal-write-entered').touch()
                for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
                    os.kill(os.getpid(), sig)
                (GATE_DIR / 'signal-write-returned').touch()
            return os.write(self.output_fd, data)
    sys.stderr = io.TextIOWrapper(io.BufferedWriter(SignalDuringWrite()), encoding='utf-8')
'''
        helper.write_text(helper.read_text().replace('\nPOLL = ', fixture + '\nPOLL = '))
        child, directory = self.launch()
        wait_for(lambda: (directory / 'cargo-started').exists())
        owner = json.loads((self.gate / 'holder.json').read_text())
        lease = self.module.lease_path(owner['worker'], owner['job'])
        stop = lease.with_name(lease.name + '.stop')
        (self.gate / 'signal-during-write').touch()
        stop.write_text('{bad}')  # Reach the admitted active-state diagnostic.
        wait_for(lambda: (self.gate / 'signal-write-entered').exists())
        wait_for(lambda: (self.gate / 'signal-write-returned').exists()
                 or not (self.gate / 'holder.json').exists())
        os.kill(child.pid, 0)
        with (self.gate / 'lock').open('r+') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertTrue((self.gate / 'signal-write-returned').exists(),
                        (self.gate / 'ci-holder.log').read_text())
        self.assertEqual(json.loads((self.gate / 'holder.json').read_text())['token'], owner['token'])
        stop.unlink()
        self.finish(child, directory)

    def test_empty_ps_result_cannot_prove_a_live_pid_absent(self):
        missing = subprocess.CompletedProcess([], 1, '', '')
        with patch.object(self.module.subprocess, 'run', return_value=missing):
            with self.assertRaisesRegex(self.module.GateError, 'GATE_E_PROCESS'):
                self.module.process(os.getpid())

    def test_reused_pid_with_changed_start_is_not_the_same_worker(self):
        worker = self.module.process(os.getpid())
        reused = dict(worker, start='different lifetime')
        with patch.object(self.module, 'process', return_value=reused):
            self.assertFalse(self.module.same_process(worker))

    def test_delayed_sidecar_writes_do_not_publish_partial_readiness_or_stop(self):
        helper = self.scripts / 'cargo-gate-hold'
        helper.write_text(helper.read_text().replace('            stream.write(data)',
                                                   '            time.sleep(0.8)\n            stream.write(data)'))
        child, directory = self.launch()
        wait_for(lambda: (directory / 'cargo-started').exists(), 12)
        owner = json.loads((self.gate / 'holder.json').read_text())
        self.assertEqual(owner['worker']['pid'], child.pid)
        self.finish(child, directory)
        self.assertFalse(list(self.gate.glob('.cargo-gate.*.tmp')))

    def test_audited_light_job_needs_no_gate_unknown_job_requires_one(self):
        (self.gate / 'lock').unlink()
        self.env['GITHUB_JOB'] = 'node'
        child, directory = self.launch('light')
        wait_for(lambda: (directory / 'cargo-started').exists())
        self.finish(child, directory)
        self.env['GITHUB_JOB'] = 'future_rust_job'
        self.denied('GATE_E_LOCK')


if __name__ == '__main__':
    unittest.main()
