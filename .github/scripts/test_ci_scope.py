"""Real Git scope boundaries and acceptance refusal fixtures; no Cargo builds."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import ci_acceptance as acceptance
from ci_scope import classify, exclusions, NODE_PRESENTATION

ROOT = Path(__file__).resolve().parents[2]
PLAN = json.loads((ROOT / '.github/acceptance-plan.json').read_text())


class ScopeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        for path in ['docs/archive/old.md', 'docs/INSTALL.md', 'README.md',
                     'integrations/openclaw/config.ts', 'Cargo.toml', *NODE_PRESENTATION]:
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('before\n')
        self.commit()
        self.base = self.git('rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True,
                                       stderr=subprocess.PIPE).strip()

    def commit(self):
        self.git('add', '-A')
        self.git('commit', '-qm', 'fixture')
        return self.git('rev-parse', 'HEAD')

    def scope(self, event='pull_request', base=None):
        return classify(self.repo, event, self.git('rev-parse', 'HEAD'),
                        self.base if base is None else base)

    def edit(self, path):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('after\n')

    def test_only_existing_archive_and_presentation_modifications_narrow(self):
        for path, profile in [('docs/archive/old.md', 'archive_prose'),
                              *[(p, 'node_presentation') for p in NODE_PRESENTATION]]:
            with self.subTest(path=path):
                self.git('reset', '--hard', self.base)
                self.edit(path)
                self.commit()
                scope = self.scope()
                self.assertEqual(scope['profile'], profile)
                self.assertEqual(scope['checkout_sha'], self.git('rev-parse', 'HEAD'))
                self.assertEqual(scope['checkout_tree'], self.git('rev-parse', 'HEAD^{tree}'))

    def test_active_docs_native_security_unknown_and_mixed_remain_full(self):
        cases = [('docs/INSTALL.md',), ('README.md',), ('Cargo.toml',),
                 ('integrations/openclaw/config.ts',), ('crates/memory-node/package.json',),
                 ('docs/archive/new.md',), ('unknown',),
                 ('docs/archive/old.md', 'packages/tachi-cli/src/utils/ui.ts')]
        for paths in cases:
            with self.subTest(paths=paths):
                self.git('reset', '--hard', self.base)
                for path in paths:
                    self.edit(path)
                self.commit()
                self.assertEqual(self.scope()['profile'], 'full')

    def test_deleted_renamed_executable_and_symlink_entries_remain_full(self):
        target = self.repo / 'docs/archive/old.md'
        for action in ('delete', 'rename', 'mode', 'symlink'):
            with self.subTest(action=action):
                self.git('reset', '--hard', self.base)
                if action == 'delete':
                    target.unlink()
                elif action == 'rename':
                    target.rename(target.with_name('renamed.md'))
                elif action == 'mode':
                    target.chmod(0o755)
                else:
                    target.unlink()
                    target.symlink_to('../INSTALL.md')
                self.commit()
                self.assertEqual(self.scope()['profile'], 'full')

    def test_unproven_inputs_and_non_pr_events_never_narrow(self):
        self.assertEqual(self.scope()['profile'], 'full')  # empty diff
        self.edit('docs/archive/old.md')
        self.commit()
        for event in ('push', 'workflow_dispatch', '', 'pull_request_target'):
            self.assertEqual(self.scope(event)['profile'], 'full')
        for base in ('HEAD', self.base[:8], '0' * 40):
            self.assertEqual(self.scope(base=base)['profile'], 'full')
        self.edit('README.md')
        self.assertEqual(self.scope()['profile'], 'full')  # dirty checkout

    def test_depth_two_merge_checkout_and_advanced_base(self):
        self.git('checkout', '-qb', 'candidate')
        self.edit('docs/archive/old.md')
        head = self.commit()
        self.git('checkout', '-qb', 'advanced-base', self.base)
        self.edit('README.md')
        base = self.commit()
        self.git('merge', '--no-ff', '-qm', 'merge fixture', 'candidate')
        with tempfile.TemporaryDirectory() as d:
            clone = Path(d) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth=2', self.repo.as_uri(), str(clone)], check=True)
            self.assertEqual(classify(clone, 'pull_request', head, base)['profile'], 'archive_prose')
            # An advertised older base cannot silently hide the README change.
            self.assertEqual(classify(clone, 'pull_request', head, self.base)['profile'], 'full')

    def test_post_merge_requires_exact_protected_single_merge_push(self):
        self.git('checkout', '-qb', 'candidate')
        self.edit('README.md')
        candidate = self.commit()
        self.git('checkout', '-qb', 'main-base', self.base)
        self.git('merge', '--no-ff', '-qm', 'merge fixture', candidate)
        merged = self.git('rev-parse', 'HEAD')
        args = dict(ref='refs/heads/main', before=self.base, protected=True)
        self.assertEqual(classify(self.repo, 'push', merged, '', **args)['profile'], 'post_merge')
        with tempfile.TemporaryDirectory() as d:
            clone = Path(d) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth=2', self.repo.as_uri(), str(clone)], check=True)
            self.assertEqual(classify(clone, 'push', merged, '', **args)['profile'], 'post_merge')
        for overrides in ({'protected': False}, {'ref': 'refs/heads/topic'},
                          {'before': ''}, {'before': candidate}):
            self.assertEqual(classify(self.repo, 'push', merged, '', **(args | overrides))['profile'], 'full')
        self.assertEqual(classify(self.repo, 'push', candidate, '', **args)['profile'], 'full')
        self.assertEqual(classify(self.repo, 'workflow_dispatch', merged, '', **args)['profile'], 'full')
        (self.repo / 'README.md').write_text('dirty after merge\n')
        self.assertEqual(classify(self.repo, 'push', merged, '', **args)['profile'], 'full')
        self.git('reset', '--hard', candidate)
        self.assertEqual(classify(self.repo, 'push', candidate, '', **args)['profile'], 'full')

    def test_missing_shallow_base_keeps_full(self):
        self.edit('docs/archive/old.md')
        head = self.commit()
        with tempfile.TemporaryDirectory() as d:
            clone = Path(d) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth=1', self.repo.as_uri(), str(clone)], check=True)
            self.assertEqual(classify(clone, 'pull_request', head, self.base)['profile'], 'full')


class ApplicabilityAcceptanceTests(unittest.TestCase):
    def needs(self, profile):
        rows = {job: {'result': 'success', 'outputs': {}}
                for job in PLAN['required_jobs'] + PLAN['observational_jobs']}
        for job in exclusions(PLAN, profile):
            rows[job]['result'] = 'skipped'
        return rows

    def test_only_approved_skips_are_not_applicable_not_passed(self):
        for profile in ('archive_prose', 'node_presentation', 'post_merge'):
            needs = self.needs(profile)
            ok, verdicts = acceptance.evaluate(PLAN, needs, profile=profile)
            self.assertTrue(ok)
            for job in exclusions(PLAN, profile):
                self.assertEqual(verdicts[job], 'not_applicable')
            self.assertFalse(acceptance.evaluate(PLAN, needs)[0])
            for job in PLAN['required_jobs']:
                for result in ('failure', 'cancelled', 'neutral', None):
                    broken = {name: dict(row) for name, row in needs.items()}
                    broken[job]['result'] = result
                    self.assertFalse(acceptance.evaluate(PLAN, broken, profile=profile)[0])
            del needs['node']
            with self.assertRaises(acceptance.InvalidEvidence):
                acceptance.evaluate(PLAN, needs, profile=profile)

    def test_profile_cannot_exempt_setup_secret_scan_or_windows_inventory(self):
        for profile, job in [('archive_prose', 'gitleaks'), ('archive_prose', 'build-seat-setup'),
                             ('node_presentation', 'node'), ('full', 'rust')]:
            plan = json.loads(json.dumps(PLAN))
            plan['applicability_profiles'][profile].append(job)
            with self.assertRaises(acceptance.InvalidEvidence):
                acceptance.evaluate(plan, self.needs('full'))

    def test_main_requires_matching_successful_scope_producer(self):
        scope = {'profile': 'archive_prose', 'checkout_tree': '1' * 40}
        for producer in (None, {}, {'result': 'failure', 'outputs': {}},
                         {'result': 'success', 'outputs': {'profile': 'full', 'checkout_tree': '1' * 40}},
                         {'result': 'success', 'outputs': {'profile': 'archive_prose', 'checkout_tree': '2' * 40}}):
            needs = self.needs('archive_prose')
            needs['build-seat-setup'] = producer
            with patch.object(acceptance, 'current_scope', return_value=scope), \
                 patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), \
                 contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(acceptance.main(), 1)
        needs['build-seat-setup'] = {'result': 'success', 'outputs': {'profile': 'archive_prose', 'checkout_tree': '1' * 40}}
        out = io.StringIO()
        with patch.object(acceptance, 'current_scope', return_value=scope), \
             patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), contextlib.redirect_stdout(out):
            self.assertEqual(acceptance.main(), 0)
        self.assertEqual(json.loads(out.getvalue())['required_jobs']['rust'], 'not_applicable')

    def test_workflows_use_same_classifier_and_keep_cross_language_check(self):
        full = (ROOT / '.github/workflows/ci.yml').read_text()
        linux = (ROOT / '.github/workflows/conformance-linux.yml').read_text()
        for text in (full, linux):
            self.assertIn('python3 .github/scripts/ci_scope.py --github-output', text)
            self.assertIn('fetch-depth: 2', text)
        setup = full.split('  build-seat-setup:\n', 1)[1].split('  rust:\n', 1)[0]
        self.assertIn('python3 scripts/check_release_versions.py', setup)
        self.assertIn('scripts.test_check_release_versions', setup)
        self.assertIn("needs.build-seat-setup.outputs.run_rust == 'true'", full)
        self.assertIn("needs.build-seat-setup.outputs.run_node == 'true'", full)
        self.assertEqual(linux.count("needs.change-scope.outputs.run_rust == 'true'"), 2)


if __name__ == '__main__':
    unittest.main()
