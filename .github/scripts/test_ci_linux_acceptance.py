"""Linux aggregate refuses missing/failing jobs and unproven skips."""
import json
from pathlib import Path
import unittest

from ci_acceptance import InvalidEvidence
from ci_linux_acceptance import evaluate, JOBS

ROOT = Path(__file__).resolve().parents[2]
PLAN = json.loads((ROOT / '.github/acceptance-plan.json').read_text())


class LinuxAcceptanceTests(unittest.TestCase):
    def fixture(self, profile='full'):
        scope = {'profile': profile, 'checkout_tree': '1' * 40}
        needs = {job: {'result': 'success'} for job in JOBS}
        needs['change-scope']['outputs'] = dict(scope)
        return scope, needs

    def test_full_refuses_skips_and_all_profiles_refuse_failures(self):
        for profile in ('full', 'archive_prose', 'node_presentation', 'post_merge'):
            scope, needs = self.fixture(profile)
            self.assertTrue(evaluate(PLAN, needs, scope)[0])
            for job in ('rust-gate', 'linux-platform'):
                for result in ('failure', 'cancelled', 'neutral', '', None):
                    bad = needs | {job: {'result': result}}
                    self.assertFalse(evaluate(PLAN, bad, scope)[0])
            skipped = needs | {job: {'result': 'skipped'} for job in ('rust-gate', 'linux-platform')}
            ok, rows = evaluate(PLAN, skipped, scope)
            self.assertEqual(ok, profile != 'full')
            if ok:
                self.assertEqual(rows['rust-gate'], 'not_applicable')
                self.assertEqual(rows['linux-platform'], 'not_applicable')

    def test_closed_inventory_and_successful_matching_producer_required(self):
        scope, needs = self.fixture('post_merge')
        for job in JOBS:
            with self.assertRaises(InvalidEvidence):
                evaluate(PLAN, {k: v for k, v in needs.items() if k != job}, scope)
        with self.assertRaises(InvalidEvidence):
            evaluate(PLAN, needs | {'extra': {'result': 'success'}}, scope)
        for producer in (None, {}, {'result': 'failure'}, {'result': 'success'},
                         {'result': 'success', 'outputs': []},
                         {'result': 'success', 'outputs': {'profile': 'full', 'checkout_tree': '1' * 40}},
                         {'result': 'success', 'outputs': {'profile': 'post_merge', 'checkout_tree': '2' * 40}}):
            with self.assertRaises(InvalidEvidence):
                evaluate(PLAN, needs | {'change-scope': producer}, scope)

    def test_aggregate_and_dispatch_are_wired(self):
        text = (ROOT / '.github/workflows/conformance-linux.yml').read_text()
        aggregate = text.split('  linux-acceptance:\n')[1]
        self.assertIn('needs: [change-scope, rust-gate, linux-platform]', aggregate)
        self.assertIn('always() && (', aggregate)
        self.assertIn('github.actor == github.repository_owner', aggregate)
        self.assertIn('github.triggering_actor == github.repository_owner', aggregate)
        self.assertIn('ci_linux_acceptance.py', aggregate)
        for workflow in ('ci.yml', 'conformance-linux.yml'):
            source = (ROOT / '.github/workflows' / workflow).read_text()
            self.assertIn('  workflow_dispatch:', source)
            self.assertIn("PUSH_BEFORE: ${{ github.event.before || '' }}", source)


if __name__ == '__main__':
    unittest.main()
