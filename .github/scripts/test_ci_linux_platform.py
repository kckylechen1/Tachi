"""Coverage omission, retry and receipt provenance refusal at the real parser boundary."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

import ci_linux_platform as platform

ROOT = Path(__file__).resolve().parents[2]
RAW = (ROOT / '.github/linux-platform-tests.json').read_text()
GROUPS = platform.inventory(RAW)
DIGEST = hashlib.sha256(RAW.encode()).hexdigest()
IDENTITY = {'sha': '1' * 40, 'tree': '2' * 40, 'run_id': '42', 'attempt': 2}


def report():
    root = ET.Element('testsuites', failures='0', errors='0')
    for row in GROUPS.values():
        suite = ET.SubElement(root, 'testsuite', name=row['suite'])
        for name in row['tests']:
            ET.SubElement(suite, 'testcase', name=name, classname=row['suite'], time='0.1')
    return root


class LinuxPlatformTests(unittest.TestCase):
    def check(self, root):
        return platform.coverage(ET.tostring(root), GROUPS)

    def test_all_five_groups_are_present_and_successful(self):
        self.assertEqual(self.check(report()), {'seccomp': 1, 'no_clobber': 3,
                                               'anchored': 3, 'receipt': 1, 'exchange': 10})
        root = report()
        for test in root.iter('testcase'):
            ET.SubElement(test, 'system-out').text = 'ordinary output'
        self.assertEqual(sum(self.check(root).values()), 18)

    def test_every_required_case_is_individually_required_once(self):
        for index in range(18):
            for mutation in ('remove', 'rename', 'duplicate', 'wrong_suite'):
                root = report()
                pairs = [(s, t) for s in root for t in s]
                suite, test = pairs[index]
                if mutation == 'remove':
                    suite.remove(test)
                elif mutation == 'rename':
                    test.set('name', 'unexpected_test')
                elif mutation == 'duplicate':
                    suite.append(copy.deepcopy(test))
                else:
                    suite.set('name', 'wrong-package')
                with self.subTest(index=index, mutation=mutation), self.assertRaises(ValueError):
                    self.check(root)

    def test_skips_errors_and_all_nextest_retry_annotations_fail(self):
        for tag in ('skipped', 'failure', 'error', 'flakyFailure', 'flakyError',
                    'rerunFailure', 'rerunError', 'unknown'):
            for index in range(18):
                root = report()
                ET.SubElement(list(root.iter('testcase'))[index], tag)
                with self.subTest(tag=tag, index=index), self.assertRaises(ValueError):
                    self.check(root)
        for field in ('failures', 'errors'):
            root = report()
            root.set(field, '1')
            with self.assertRaises(ValueError):
                self.check(root)

    def test_new_matching_tests_also_must_pass_without_retry(self):
        root = report()
        row = GROUPS['exchange']
        suite = ET.SubElement(root, 'testsuite', name=row['suite'])
        test = ET.SubElement(suite, 'testcase', name='new::sticky_cutover_test', classname=row['suite'])
        self.assertEqual(self.check(root)['exchange'], 11)
        ET.SubElement(test, 'flakyFailure')
        with self.assertRaises(ValueError):
            self.check(root)

    def receipt(self):
        return {'schema': 1, 'identity': dict(IDENTITY), 'inventory_sha256': DIGEST,
                'report_sha256': '3' * 64, 'counts': self.check(report())}

    def verify(self, receipt, producer='success'):
        platform.verify_receipt(receipt, IDENTITY, DIGEST, GROUPS, producer)

    def test_receipt_requires_successful_same_run_same_source_producer(self):
        self.verify(self.receipt())
        old_attempt = self.receipt()
        old_attempt['identity']['attempt'] = 1
        self.verify(old_attempt)  # failed-job rerun can reuse successful unchanged producer
        for producer in ('failure', 'cancelled', 'skipped', '', None):
            with self.assertRaises(ValueError):
                self.verify(self.receipt(), producer)
        for key in IDENTITY:
            bad = self.receipt()
            bad['identity'][key] = 3 if key == 'attempt' else 'wrong'
            with self.assertRaises(ValueError):
                self.verify(bad)
        for key, value in [('schema', True), ('counts', {}), ('counts', {'seccomp': 1}),
                           ('inventory_sha256', '0' * 64), ('report_sha256', ''), ('identity', {})]:
            bad = self.receipt() | {key: value}
            with self.assertRaises(ValueError):
                self.verify(bad)
        for group in GROUPS:
            bad = self.receipt()
            bad['counts'][group] = 0
            with self.assertRaises(ValueError):
                self.verify(bad)

    def test_missing_report_cannot_publish_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '.github').mkdir()
            (root / '.github/linux-platform-tests.json').write_text(RAW)
            output = root / 'output'
            with patch.object(platform, 'ROOT', root), patch.object(platform, 'identity', return_value=IDENTITY), \
                 patch('sys.argv', ['ci_linux_platform.py', 'produce']), \
                 patch.dict('os.environ', {'GITHUB_OUTPUT': str(output)}):
                self.assertEqual(platform.main(), 1)
            self.assertFalse(output.exists())

    def test_production_wiring_prevents_stale_report_or_failed_step_pass(self):
        # Parse actual workflow with repository's existing Ruby/Psych dependency.
        import subprocess
        source = subprocess.check_output(['ruby', '-rjson', '-ryaml', '-e',
            'puts JSON.generate(YAML.load_file(ARGV[0]))', str(ROOT / '.github/workflows/conformance-linux.yml')], text=True)
        jobs = json.loads(source)['jobs']
        rust = jobs['rust-gate']
        steps = {s.get('id'): s for s in rust['steps'] if s.get('id')}
        run = steps['workspace_tests']['run']
        self.assertLess(run.index('unlink(missing_ok=True)'), run.index('nextest run'))
        self.assertIn("steps.workspace_tests.outcome == 'success'", steps['platform_evidence']['if'])
        self.assertNotIn('continue-on-error', steps['platform_evidence'])
        self.assertEqual(rust['outputs']['platform_receipt'], '${{ steps.platform_evidence.outputs.receipt }}')
        gate = jobs['linux-platform']
        self.assertEqual(gate['needs'], ['change-scope', 'rust-gate'])
        consumer = next(s for s in gate['steps'] if s.get('run') == 'python3 .github/scripts/ci_linux_platform.py verify')
        self.assertEqual(consumer['env']['PLATFORM_RECEIPT'], '${{ needs.rust-gate.outputs.platform_receipt }}')
        self.assertEqual(consumer['env']['WORKSPACE_RESULT'], '${{ needs.rust-gate.result }}')
        self.assertNotIn('continue-on-error', consumer)
        self.assertFalse(any('nextest run' in s.get('run', '') for s in gate['steps']))


if __name__ == '__main__':
    unittest.main()
