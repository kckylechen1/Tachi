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
from ci_scope import classify, exclusions, LOCAL_NPM_CACHE, REMOTE_NPM_CACHE

ROOT = Path(__file__).resolve().parents[2]
PLAN = json.loads((ROOT / '.github/acceptance-plan.json').read_text())
RETIRED_PRESENTATION_PATHS = (
    'packages/tachi-cli/src/utils/ui.ts',
    'packages/tachi-cli/src/utils/i18n.ts',
)
OPERATIONAL_PROSE_PATHS = (
    'docs/engineering/operations/recall-eval.md',
    'docs/engineering/operations/staff-result-testing.md',
)
FULL_SCOPE_PATHS = (
    'AGENTS.md', '.github/scripts/ci_scope.py', '.github/scripts/ci_acceptance.py',
    '.github/scripts/test_ci_scope.py', '.github/scripts/test_ci_acceptance.py',
    '.github/acceptance-plan.json', '.github/workflows/ci.yml', '.config/nextest.toml',
    'crates/tachi-server/src/lib.rs',
    'docs/engineering/architecture/dispatch-lifecycle.md',
    *('docs/engineering/operations/' + name for name in (
        'ci-scheduling.md', 'actions-capacity.md', 'acceptance-runner.md',
        'host-admission-reference.md', 'test-failure-injection.md',
        'current-store-admission-preparation.md', 'portable-version-admission.md',
        'fts-split-repair.md', 'legacy-entrypoints.md',
    )),
)


class ScopeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        for path in ['docs/archive/old.md', 'docs/INSTALL.md', 'README.md',
                     'integrations/openclaw/config.ts', 'Cargo.toml', *RETIRED_PRESENTATION_PATHS,
                     *OPERATIONAL_PROSE_PATHS, *FULL_SCOPE_PATHS]:
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

    def test_archive_narrows_but_retired_presentation_modifications_stay_full(self):
        for path, profile in [('docs/archive/old.md', 'archive_prose'),
                              *[(p, 'full') for p in RETIRED_PRESENTATION_PATHS]]:
            with self.subTest(path=path):
                self.git('reset', '--hard', self.base)
                self.edit(path)
                self.commit()
                scope = self.scope()
                self.assertEqual(scope['profile'], profile)
                self.assertEqual(scope['checkout_sha'], self.git('rev-parse', 'HEAD'))
                self.assertEqual(scope['checkout_tree'], self.git('rev-parse', 'HEAD^{tree}'))

    def test_operational_prose_accepts_only_existing_allowlisted_documents(self):
        for paths in [(path,) for path in OPERATIONAL_PROSE_PATHS] + [OPERATIONAL_PROSE_PATHS]:
            with self.subTest(paths=paths):
                self.git('reset', '--hard', self.base)
                for path in paths:
                    self.edit(path)
                self.commit()
                scope = self.scope()
                self.assertEqual(scope['profile'], 'operational_prose')
                self.assertEqual(scope['checkout_tree'], self.git('rev-parse', 'HEAD^{tree}'))

    def test_operational_prose_does_not_exempt_policy_code_or_mixed_changes(self):
        for path in FULL_SCOPE_PATHS + ('docs/archive/old.md', 'docs/INSTALL.md', 'README.md'):
            for paths in ((path,), (OPERATIONAL_PROSE_PATHS[0], path)):
                with self.subTest(paths=paths):
                    self.git('reset', '--hard', self.base)
                    for changed in paths:
                        self.edit(changed)
                    self.commit()
                    expected = 'archive_prose' if paths == ('docs/archive/old.md',) else 'full'
                    self.assertEqual(self.scope()['profile'], expected)

    def test_new_operational_documents_including_allowlisted_names_stay_full(self):
        for path in OPERATIONAL_PROSE_PATHS:
            with self.subTest(path=path):
                self.git('reset', '--hard', self.base)
                self.git('rm', path)
                without_document = self.commit()
                self.edit(path)
                self.commit()
                self.assertEqual(self.scope(base=without_document)['profile'], 'full')
        for path in ('docs/engineering/operations/cleanup-kill-list.md',
                     'docs/engineering/operations/new.md', 'unknown.md'):
            self.git('reset', '--hard', self.base)
            self.edit(OPERATIONAL_PROSE_PATHS[0])
            self.edit(path)
            self.commit()
            self.assertEqual(self.scope()['profile'], 'full')

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

    def test_cache_only_workflow_diff_and_fail_closed_boundaries(self):
        path = '.github/workflows/ci.yml'
        # A fixed fixture permits either cache endpoint in the real workflow;
        # an unrelated future setup-node upgrade should also remain full scope,
        # rather than failing tests because the live workflow changed shape.
        source = """name: Cache scope fixture
permissions:
  contents: read
jobs:
  node:
    if: github.actor == github.repository_owner
    steps:
""" + LOCAL_NPM_CACHE + """
      - name: Install dependencies
        run: npm ci
  gitleaks:
    steps:
      - run: echo scan
"""
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(source.replace(LOCAL_NPM_CACHE, REMOTE_NPM_CACHE))
        self.base = self.commit()
        target.write_text(source)
        head = self.commit()
        self.assertEqual(self.scope()['profile'], 'node_cache')
        target.write_text(source.replace(LOCAL_NPM_CACHE, REMOTE_NPM_CACHE))
        self.commit()
        self.assertEqual(self.scope(base=head)['profile'], 'node_cache')
        # Recognize only a literal safe leaf, not general shell/YAML edits.
        for edit, expected in [
                (source.replace('/tachi/npm', '/tachi/npm-v2'), 'node_cache'),
                (source.replace('/tachi/npm', '/tachi/../other'), 'full'),
                (source.replace('/tachi/npm', '/tachi/$(id)'), 'full'),
                (source.replace('>> "$GITHUB_ENV"', '>> "$GITHUB_ENV"; false'), 'full'),
                (source.replace('node-version: 20', 'node-version: 22'), 'full'),
                (source.replace('run: npm ci', 'run: npm ci --ignore-scripts'), 'full'),
                (source.replace('package-manager-cache: false', 'package-manager-cache: true'), 'full'),
                (source.replace('a0853c24544627f65ddf259abe73b1d18a591444', '0' * 40), 'full'),
                (source.replace('github.actor == github.repository_owner', 'true'), 'full'),
                (source.replace('contents: read', 'contents: write'), 'full'),
                (source.replace('  node:\n', '  node:\n' + LOCAL_NPM_CACHE), 'full'),
                (source.replace('  node:\n', '  renamed-node:\n'), 'full')]:
            with self.subTest(expected=expected, edit=edit[-100:]):
                self.git('reset', '--hard', self.base)
                target.write_text(edit)
                self.commit()
                self.assertEqual(self.scope()['profile'], expected)
        self.git('reset', '--hard', head)
        self.edit('packages/tachi-cli/src/utils/ui.ts')
        self.commit()
        self.assertEqual(self.scope()['profile'], 'full')
        self.git('reset', '--hard', head)
        target.chmod(0o755)
        self.commit()
        self.assertEqual(self.scope()['profile'], 'full')

    def test_deleted_renamed_executable_and_symlink_entries_remain_full(self):
        for path in ('docs/archive/old.md', *OPERATIONAL_PROSE_PATHS):
            target = self.repo / path
            for action in ('delete', 'rename', 'mode', 'symlink'):
                with self.subTest(path=path, action=action):
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
        for path in ('docs/archive/old.md', OPERATIONAL_PROSE_PATHS[0]):
            self.git('reset', '--hard', self.base)
            self.edit(path)
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

    def acceptance_main(self, event, producer):
        head = self.git('rev-parse', 'HEAD')
        needs = {job: {'result': 'success', 'outputs': {}}
                 for job in PLAN['required_jobs'] + PLAN['observational_jobs']}
        needs['build-seat-setup'] = producer
        output = io.StringIO()
        with patch.object(acceptance, 'current_scope',
                          side_effect=lambda _: classify(self.repo, event, head, self.base)), \
             patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            return acceptance.main(), output.getvalue()

    def test_main_binds_producer_profile_even_when_real_classification_is_full(self):
        self.edit('AGENTS.md')
        self.commit()
        tree = self.git('rev-parse', 'HEAD^{tree}')
        bad_outputs = [None, [], {}, {'profile': None}, {'profile': []},
                       {'profile': 'unknown'}, {'profile': 'operational_prose'},
                       {'profile': 'operational_prose', 'checkout_tree': tree},
                       {'profile': 'operational_prose', 'checkout_tree': '2' * 40}]
        for event in ('pull_request', 'workflow_dispatch'):
            for outputs in bad_outputs:
                with self.subTest(event=event, outputs=outputs):
                    producer = {'result': 'success', 'outputs': outputs}
                    self.assertEqual(self.acceptance_main(event, producer)[0], 1)
            for producer in (None, {}, {'result': 'success'},
                             {'result': 'failure', 'outputs': {'profile': 'full'}}):
                with self.subTest(event=event, producer=producer):
                    self.assertEqual(self.acceptance_main(event, producer)[0], 1)
            producer = {'result': 'success', 'outputs': {'profile': 'full', 'checkout_tree': ''}}
            code, output = self.acceptance_main(event, producer)
            self.assertEqual(code, 0)
            self.assertTrue(json.loads(output)['accepted'])

    def test_main_binds_operational_profile_and_tree_to_real_classification(self):
        self.edit(OPERATIONAL_PROSE_PATHS[0])
        self.commit()
        tree = self.git('rev-parse', 'HEAD^{tree}')
        for outputs, expected in (
                ({'profile': 'operational_prose', 'checkout_tree': tree}, 0),
                ({'profile': 'full', 'checkout_tree': tree}, 1),
                ({'profile': 'operational_prose', 'checkout_tree': '2' * 40}, 1),
                ({'profile': 'operational_prose', 'checkout_tree': ''}, 1),
                ({'profile': 'operational_prose'}, 1)):
            with self.subTest(outputs=outputs):
                producer = {'result': 'success', 'outputs': outputs}
                self.assertEqual(self.acceptance_main('pull_request', producer)[0], expected)
        # A dirty checkout falls back to full and cannot reuse the narrow producer.
        self.edit('README.md')
        producer = {'result': 'success',
                    'outputs': {'profile': 'operational_prose', 'checkout_tree': tree}}
        self.assertEqual(self.acceptance_main('pull_request', producer)[0], 1)


class ApplicabilityAcceptanceTests(unittest.TestCase):
    def needs(self, profile):
        rows = {job: {'result': 'success', 'outputs': {}}
                for job in PLAN['required_jobs'] + PLAN['observational_jobs']}
        for job in exclusions(PLAN, profile):
            rows[job]['result'] = 'skipped'
        return rows

    def test_only_approved_skips_are_not_applicable_not_passed(self):
        for profile in ('archive_prose', 'node_cache', 'post_merge'):
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
                             ('node_cache', 'node'),
                             ('node_cache', 'gitleaks'), ('node_cache', 'build-seat-setup'), ('full', 'rust'),
                             *[('operational_prose', job) for job in
                               PLAN['required_jobs'] + PLAN['observational_jobs']]]:
            plan = json.loads(json.dumps(PLAN))
            plan['applicability_profiles'][profile].append(job)
            with self.assertRaises(acceptance.InvalidEvidence):
                acceptance.evaluate(plan, self.needs('full'))

    def test_operational_prose_retains_every_required_job(self):
        self.assertEqual(exclusions(PLAN, 'operational_prose'), [])
        needs = self.needs('operational_prose')
        ok, verdicts = acceptance.evaluate(PLAN, needs, profile='operational_prose')
        self.assertTrue(ok)
        self.assertEqual({job: verdicts[job] for job in PLAN['required_jobs']},
                         {job: 'passed' for job in PLAN['required_jobs']})
        for job in PLAN['required_jobs']:
            for result in ('failure', 'cancelled', 'skipped', 'neutral', None):
                with self.subTest(job=job, result=result):
                    broken = {name: dict(row) for name, row in needs.items()}
                    broken[job]['result'] = result
                    ok, verdicts = acceptance.evaluate(PLAN, broken, profile='operational_prose')
                    self.assertFalse(ok)
                    self.assertNotEqual(verdicts[job], 'not_applicable')

    def test_retired_presentation_profile_is_rejected(self):
        with self.assertRaises(acceptance.InvalidEvidence):
            acceptance.evaluate(PLAN, self.needs('full'), profile='node_presentation')
        plan = json.loads(json.dumps(PLAN))
        plan['applicability_profiles']['node_presentation'] = ['rust']
        with self.assertRaises(acceptance.InvalidEvidence):
            acceptance.evaluate(plan, self.needs('full'))

    def test_main_requires_matching_successful_scope_producer(self):
        for profile in ('archive_prose', 'operational_prose'):
            scope = {'profile': profile, 'checkout_tree': '1' * 40}
            for producer in (None, {}, {'result': 'failure', 'outputs': {}},
                             {'result': 'success', 'outputs': {'profile': 'full', 'checkout_tree': '1' * 40}},
                             {'result': 'success', 'outputs': {'profile': profile, 'checkout_tree': '2' * 40}}):
                needs = self.needs(profile)
                needs['build-seat-setup'] = producer
                with patch.object(acceptance, 'current_scope', return_value=scope), \
                     patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), \
                     contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(acceptance.main(), 1)
            needs['build-seat-setup'] = {'result': 'success', 'outputs': scope}
            out = io.StringIO()
            with patch.object(acceptance, 'current_scope', return_value=scope), \
                 patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), contextlib.redirect_stdout(out):
                self.assertEqual(acceptance.main(), 0)
            expected = 'not_applicable' if profile == 'archive_prose' else 'passed'
            self.assertEqual(json.loads(out.getvalue())['required_jobs']['rust'], expected)

    def test_main_never_accepts_matching_absent_or_invalid_reduced_tree(self):
        for tree in (None, '', 'not-a-tree', [], '1' * 39):
            with self.subTest(tree=tree):
                scope = {'profile': 'operational_prose', 'checkout_tree': tree}
                needs = self.needs('operational_prose')
                needs['build-seat-setup'] = {'result': 'success', 'outputs': scope}
                with patch.object(acceptance, 'current_scope', return_value=scope), \
                     patch.dict(os.environ, {'CI_NEEDS_JSON': json.dumps(needs)}), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(acceptance.main(), 1)

    def test_workflows_use_same_classifier_and_keep_cross_language_check(self):
        full = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn('python3 .github/scripts/ci_scope.py --github-output', full)
        self.assertIn('fetch-depth: 2', full)
        setup = full.split('  build-seat-setup:\n', 1)[1].split('  rust:\n', 1)[0]
        self.assertIn('python3 scripts/check_release_versions.py', setup)
        self.assertIn('scripts.test_check_release_versions', setup)
        self.assertIn("needs.build-seat-setup.outputs.run_rust == 'true'", full)
        self.assertIn("needs.build-seat-setup.outputs.run_node == 'true'", full)


if __name__ == '__main__':
    unittest.main()
