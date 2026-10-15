import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
import test_cargo_gate as fixtures


class LocalCiInterop(unittest.TestCase):
    setUp=fixtures.CargoGateTests.setUp
    tearDown=fixtures.CargoGateTests.tearDown
    fixture_source=fixtures.CargoGateTests.fixture_source
    launch=fixtures.CargoGateTests.launch
    finish=fixtures.CargoGateTests.finish
    denied=fixtures.CargoGateTests.denied

    def launch_verifier(self, open_gate=True):
        scripts=self.root/'scripts';scripts.mkdir()
        guardian=scripts/'verification_guard.py'
        guardian.write_text('''import fcntl,sys,time
from pathlib import Path
root=Path(sys.argv[2])
if sys.argv[3]=='hold':
 stream=(root/'gate'/'lock').open('r')
 fcntl.flock(stream,fcntl.LOCK_EX)
(root/'verifier-ready').touch()
while not (root/'release-verifier').exists(): time.sleep(.03)
''')
        child=subprocess.Popen([sys.executable,str(guardian.resolve()),'guardian',str(self.root),'hold' if open_gate else 'none'])
        self.children.append(child)
        fixtures.wait_for(lambda:(self.root/'verifier-ready').exists())
        return child

    def test_ci_waits_for_known_verifier_without_sidecar(self):
        verifier=self.launch_verifier()
        worker,directory=self.launch()
        time.sleep(2)
        self.assertFalse((directory/'start-result').exists())
        self.assertFalse((directory/'cargo-started').exists())
        (self.root/'release-verifier').touch()
        self.assertEqual(verifier.wait(timeout=3),0)
        fixtures.wait_for(lambda:(directory/'cargo-started').exists())
        self.finish(worker,directory)

    def test_verifier_name_without_gate_fd_does_not_recognize_unknown_owner(self):
        self.launch_verifier(open_gate=False)
        with (self.gate/'lock').open('r') as held:
            fixtures.fcntl.flock(held,fixtures.fcntl.LOCK_EX)
            self.denied('GATE_E_HOLDER_START')

    def test_known_verifier_does_not_override_malformed_sidecar(self):
        self.launch_verifier()
        (self.gate/'holder.json').write_text('{broken')
        self.denied('GATE_E_HOLDER_START')
        self.assertEqual((self.gate/'holder.json').read_text(),'{broken')

    def prepare_local(self):
        text=(fixtures.SOURCE/'cargo').read_text().replace('GATE_DIR = "/tmp/cargo-gate"',f'GATE_DIR = {str(self.gate)!r}')
        wrapper=self.scripts/'cargo';wrapper.write_text(text);wrapper.chmod(0o755)
        # macOS TemporaryDirectory may spell /var while Python's SELF is
        # /private/var. Match the installed helper's literal wrapper identity.
        helper=self.scripts/'cargo-gate-hold'
        helper.write_text(helper.read_text().replace(repr(str(wrapper)),repr(str(wrapper.resolve()))))
        real=self.root/'real';real.mkdir()
        cargo=real/'cargo'
        cargo.write_text('#!/bin/bash\ntouch "$MARKER"\nsleep "${DELAY:-0}"\n')
        cargo.chmod(0o755)
        self.env.update(PATH=str(self.scripts)+':'+str(real)+':'+os.environ['PATH'],MARKER=str(self.root/'cargo-ran'))
        return wrapper

    def test_local_wrapper_borrows_its_real_job_lease(self):
        wrapper=self.prepare_local()
        driver=self.root/'interop.sh'
        driver.write_text('''#!/bin/bash
set -e
bash "$1/cargo-gate-ci-start"
GITHUB_ACTIONS=true "$1/cargo" build
bash "$1/cargo-gate-ci-end"
''')
        child=subprocess.Popen([str(self.worker),str(driver),str(self.scripts)],env=self.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        self.children.append(child)
        out,err=child.communicate(timeout=10)
        self.assertEqual(child.returncode,0,out+err)
        self.assertTrue((self.root/'cargo-ran').exists())

    def test_ci_waits_behind_local_guardian_on_the_same_lock(self):
        wrapper=self.prepare_local()
        env=dict(self.env,DELAY='1.5')
        env.pop('GITHUB_ACTIONS',None)
        child=subprocess.Popen([str(wrapper),'build'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        self.children.append(child)
        fixtures.wait_for(lambda:(self.root/'cargo-ran').exists())
        worker,directory=self.launch()
        time.sleep(.2)
        self.assertFalse((directory/'cargo-started').exists())
        self.assertEqual(child.wait(timeout=5),0)
        fixtures.wait_for(lambda:(directory/'cargo-started').exists())
        self.finish(worker,directory)


if __name__=='__main__':unittest.main()
