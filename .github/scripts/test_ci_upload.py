"""Uploader state-machine and terminal refusal; no network or Rust builds."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
ACTION = ROOT / '.github/actions/upload-junit'
SPEC = importlib.util.spec_from_file_location('upload_verify', ACTION / 'verify.py')
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class UploadTests(unittest.TestCase):
    def test_only_real_success_with_artifact_id_confirms(self):
        for successful in ('first', 'second', 'third'):
            rows = {'report': {'outcome': 'success'}}
            rows.update({name: {'outcome': 'failure', 'conclusion': 'success'}
                         for name in ('first', 'second', 'third')})
            rows[successful] = {'outcome': 'success', 'outputs': {'artifact-id': '123'}}
            self.assertTrue(VERIFY.confirmed(rows))
        for outcome in ('failure', 'cancelled', 'skipped', None):
            rows = {'report': {'outcome': 'success'},
                    'first': {'outcome': outcome, 'conclusion': 'success',
                              'outputs': {'artifact-id': '123'}}}
            self.assertFalse(VERIFY.confirmed(rows))
        for artifact_id in ('', '0', '-1', 'abc', None):
            rows = {'report': {'outcome': 'success'},
                    'first': {'outcome': 'success', 'outputs': {'artifact-id': artifact_id}}}
            self.assertFalse(VERIFY.confirmed(rows))

    def test_terminal_command_fails_closed(self):
        for raw in ('', '{}', '[]', 'null', '{"report":null}',
                    '{"report":{"outcome":"success"},"first":{"outcome":"failure","conclusion":"success"}}'):
            result = subprocess.run([sys.executable, str(ACTION / 'verify.py')],
                                    env=os.environ | {'UPLOAD_STEPS': raw}, capture_output=True)
            self.assertNotEqual(result.returncode, 0)

    def test_real_composite_wiring_bounds_retry_and_preserves_failure(self):
        action = json.loads(subprocess.check_output(
            ['ruby', '-rpsych', '-rjson', '-e',
             'puts JSON.generate(Psych.safe_load(File.read(ARGV[0])))', str(ACTION / 'action.yml')]))
        steps = action['runs']['steps']
        uploads = [step for step in steps if 'uses' in step]
        self.assertEqual(len(uploads), 3)
        for i, step in enumerate(uploads, 1):
            self.assertEqual(step['uses'], 'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02')
            self.assertEqual(step['with']['if-no-files-found'], 'error')
            self.assertIn('github.run_attempt', step['with']['name'])
            self.assertTrue(step['with']['name'].endswith(f'-upload-{i}'))
            self.assertIn('!cancelled()', step['if'])
        self.assertTrue(uploads[0]['continue-on-error'])
        self.assertTrue(uploads[1]['continue-on-error'])
        self.assertNotIn('continue-on-error', uploads[2])
        self.assertIn("steps.first.outcome == 'failure'", uploads[1]['if'])
        self.assertIn("steps.second.outcome == 'failure'", uploads[2]['if'])
        self.assertNotIn('continue-on-error', steps[-1])
        self.assertEqual(steps[-1]['if'], '${{ !cancelled() }}')
        self.assertIn('verify.py', steps[-1]['run'])
        with tempfile.TemporaryDirectory() as d:
            missing = subprocess.run(['bash', '-c', steps[0]['run']],
                                     env=os.environ | {'REPORT_PATH': str(Path(d) / 'missing')})
            self.assertNotEqual(missing.returncode, 0)
        for workflow in ('ci.yml', 'conformance-linux.yml'):
            text = (ROOT / '.github/workflows' / workflow).read_text()
            self.assertEqual(text.count('uses: ./.github/actions/upload-junit'), 1)
            self.assertIn("steps.workspace_tests.outcome == 'success'", text)
            self.assertNotIn('continue-on-error:', text)


if __name__ == '__main__':
    unittest.main()
