import fcntl
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).parent
SOURCE = ROOT / 'cargo'


def wait_for(fn, timeout=8):
    end = time.monotonic() + timeout
    while not fn():
        if time.monotonic() > end:
            raise AssertionError('fixture timeout')
        time.sleep(.03)


class LocalCargo(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Program bytes are immutable; each test still owns its gate and state.
        cls.programs = tempfile.TemporaryDirectory(prefix='cargo-local-program-')
        cls.addClassCleanup(cls.programs.cleanup)
        cls.realbin = Path(cls.programs.name) / 'real'
        cls.realbin.mkdir()
        cls.real = cls.realbin / 'cargo'
        cls.real.write_text('''#!/usr/bin/env python3
import os,time,subprocess,sys
from pathlib import Path
p=Path(os.environ['MARKER'])
p.write_text('started')
if os.environ.get('NEST') == '1':
 env=dict(os.environ,NEST='0',MARKER=str(p)+'.nested')
 sys.exit(subprocess.call([sys.executable,os.environ['WRAPPER'],'build'],env=env))
if os.environ.get('LINGER') == '1':
 subprocess.Popen([sys.executable,'-c','import time;time.sleep(1.0)'],close_fds=True)
else:
 time.sleep(float(os.environ.get('DELAY','0')))
sys.exit(int(os.environ.get('RESULT','0')))
''')
        cls.real.chmod(0o555)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='cargo-local-test-')
        self.root = Path(self.temp.name)
        self.gate = self.root / 'gate'
        self.gate.mkdir()
        (self.gate / 'lock').touch()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.wrapper = self.bin / 'cargo'
        self.wrapper.write_text(SOURCE.read_text().replace('GATE_DIR = "/tmp/cargo-gate"', f'GATE_DIR = {str(self.gate)!r}'))
        self.wrapper.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin)+':'+str(self.realbin)+':'+os.environ['PATH'],
                        WRAPPER=str(self.wrapper), MARKER=str(self.root/'started'))
        for key in ('GITHUB_ACTIONS','BUILDGATE_HELD','CARGO_GATE'):
            self.env.pop(key,None)
        self.children=[]

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=5)
        wait_for(self.free)
        self.temp.cleanup()

    def free(self):
        try:
            with (self.gate/'lock').open('rb') as f:
                fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)
                return True
        except BlockingIOError:
            return False
        except (FileNotFoundError,IsADirectoryError):
            return True

    def run_cargo(self,*args,**env):
        return subprocess.run([sys.executable,str(self.wrapper),*args],env=dict(self.env,**env),capture_output=True,text=True,timeout=6)

    def start(self,name='started',**env):
        child=subprocess.Popen([sys.executable,str(self.wrapper),'build'],env=dict(self.env,MARKER=str(self.root/name),**env),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        self.children.append(child)
        return child

    def test_nested_build_interprets_the_private_wrapper(self):
        source=self.wrapper.read_text()
        self.wrapper.write_text('#!/usr/bin/false\n'+source.split('\n',1)[1])
        result=self.run_cargo('build',NEST='1')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue((self.root/'started').exists())
        self.assertTrue((self.root/'started.nested').exists())

    def test_executable_entry_retains_the_private_lock(self):
        child=subprocess.Popen([str(self.wrapper),'build'],
            env=dict(self.env,DELAY='1.0'),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        self.children.append(child)
        wait_for(lambda:(self.root/'started').exists())
        self.assertFalse(self.free())
        self.assertEqual(child.wait(timeout=4),0)
        self.assertTrue(self.free())

    def test_missing_lock_refuses_and_light_version_works(self):
        (self.gate/'lock').unlink()
        self.assertEqual(self.run_cargo('build').returncode,70)
        self.assertFalse((self.root/'started').exists())
        self.assertFalse((self.gate/'lock').exists())
        self.assertEqual(self.run_cargo('--version').returncode,0)

    def test_unsafe_lock_refuses(self):
        lock=self.gate/'lock'
        lock.unlink()
        target=self.root/'other';target.touch();lock.symlink_to(target)
        self.assertEqual(self.run_cargo('build').returncode,70)
        self.assertFalse((self.root/'started').exists())

    def test_competing_cli_serializes_and_off_does_not_bypass(self):
        first=self.start(DELAY='1.0')
        wait_for(lambda:(self.root/'started').exists())
        second=self.start('second',CARGO_GATE='off',BUILDGATE_HELD=str(os.getpid()))
        time.sleep(.2)
        self.assertFalse((self.root/'second').exists())
        self.assertEqual(first.wait(timeout=4),0)
        self.assertEqual(second.wait(timeout=4),0)

    def test_nested_build_does_not_deadlock(self):
        result=self.run_cargo('build',NEST='1')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue((self.root/'started.nested').exists())

    def test_wrapper_sigkill_keeps_child_admission(self):
        first=self.start(DELAY='1.2')
        wait_for(lambda:(self.root/'started').exists())
        first.kill();first.wait()
        self.assertFalse(self.free())
        second=self.start('second')
        time.sleep(.2)
        self.assertFalse((self.root/'second').exists())
        self.assertEqual(second.wait(timeout=4),0)

    def test_lingering_descendant_keeps_lease(self):
        child=self.start(LINGER='1')
        wait_for(lambda:(self.root/'started').exists())
        time.sleep(.2)
        self.assertFalse(self.free())
        self.assertIsNone(child.poll())
        self.assertEqual(child.wait(timeout=4),0)

    def test_term_drains_group_and_preserves_failure(self):
        child=self.start(DELAY='4')
        wait_for(lambda:(self.root/'started').exists())
        child.terminate()
        self.assertNotEqual(child.wait(timeout=4),0)
        wait_for(self.free)
        self.assertEqual(self.run_cargo('build',RESULT='9').returncode,9)

    def test_run_and_watch_do_not_release_before_cargo(self):
        for command in ('run','watch'):
            with self.subTest(command=command):
                (self.root/'started').unlink(missing_ok=True)
                child=subprocess.Popen([sys.executable,str(self.wrapper),command],env=dict(self.env,DELAY='.5'),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                self.children.append(child)
                wait_for(lambda:(self.root/'started').exists())
                self.assertFalse(self.free())
                self.assertEqual(child.wait(timeout=4),0)

    def test_foreign_ci_pid_is_not_a_job_lease(self):
        (self.gate/'ci-holder.pid').write_text(str(os.getpid()))
        result=self.run_cargo('build',GITHUB_ACTIONS='true')
        self.assertEqual(result.returncode,70)
        self.assertFalse((self.root/'started').exists())

    def test_global_option_operand_cannot_impersonate_light_command(self):
        (self.gate/'lock').unlink()
        result=self.run_cargo('--config','config','build')
        self.assertEqual(result.returncode,70)
        self.assertFalse((self.root/'started').exists())

    def test_cancel_during_guardian_handoff_does_not_launch_cargo(self):
        source=self.wrapper.read_text()
        anchor='    info = os.fstat(fd)\n    current = os.stat(LOCK_PATH, follow_symlinks=False)'
        self.assertEqual(source.count(anchor),1)
        source=source.replace(anchor,'    os.kill(os.getpid(), signal.SIGTERM)\n'+anchor)
        self.wrapper.write_text(source)
        result=self.run_cargo('build')
        self.assertEqual(result.returncode,143,result.stderr)
        self.assertFalse((self.root/'started').exists())


if __name__=='__main__':
    unittest.main()
