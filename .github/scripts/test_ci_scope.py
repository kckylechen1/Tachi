"""Real Git scope boundaries and acceptance refusal fixtures; no Cargo builds."""
import contextlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import ci_acceptance as acceptance
from ci_scope import RUST_OR_GOVERNANCE_LOCKED_DOCS, classify, exclusions

ROOT = Path(__file__).resolve().parents[2]
PLAN = json.loads((ROOT / '.github/acceptance-plan.json').read_text())
RETIRED_PRESENTATION_PATHS = (
    'packages/tachi-cli/src/utils/ui.ts',
    'packages/tachi-cli/src/utils/i18n.ts',
)
ORDINARY_PROSE_PATHS = (
    'docs/engineering/operations/recall-eval.md',
    'docs/engineering/operations/staff-result-testing.md',
    'docs/engineering/operations/host-admission-reference.md',
    'docs/engineering/operations/test-failure-injection.md',
    'docs/engineering/operations/current-store-admission-preparation.md',
    'docs/engineering/operations/portable-version-admission.md',
    'docs/engineering/operations/fts-split-repair.md',
    'docs/engineering/operations/legacy-entrypoints.md',
    'docs/UPGRADING.md',
    'wiki/skills/eng-auto-checkpoint.md',
    'cards/sample-lane.md',
)
FULL_SCOPE_PATHS = (
    '.github/scripts/ci_scope.py', '.github/scripts/ci_affected_crates.py',
    '.github/scripts/ci_acceptance.py', '.github/scripts/test_ci_scope.py',
    '.github/scripts/test_ci_affected_crates.py', '.github/scripts/test_ci_acceptance.py',
    '.github/acceptance-plan.json', '.github/workflows/ci.yml', '.config/nextest.toml',
    'crates/tachi-server/src/lib.rs',
    'docs/engineering/architecture/kernel-surface-v1.fixture.json',
    'docs/current-state.agent.yaml',
    *sorted(RUST_OR_GOVERNANCE_LOCKED_DOCS),
)


def _discover_rust_embedded_markdown_paths(repo_root: Path) -> set[str]:
    """Scan crates/** and tools/** for include_str!/include_bytes! referencing .md files."""
    macro_pat = re.compile(r'include_(?:str|bytes)!\s*\(([^;{}]+?)\)', re.S)
    str_pat = re.compile(r'"([^"]+)"')
    discovered: set[str] = set()
    for base in ('crates', 'tools'):
        for rs_file in (repo_root / base).rglob('*.rs'):
            text = rs_file.read_text(encoding='utf-8', errors='replace')
            crate_root = rs_file
            while crate_root != repo_root and not (crate_root / 'Cargo.toml').exists():
                crate_root = crate_root.parent
            for match in macro_pat.finditer(text):
                inner = match.group(1)
                literals = str_pat.findall(inner)
                if not literals:
                    continue
                if 'CARGO_MANIFEST_DIR' in inner:
                    for lit in literals:
                        if lit == 'CARGO_MANIFEST_DIR' or not lit.endswith('.md'):
                            continue
                        target = (crate_root / lit.lstrip('/')).resolve()
                        discovered.add(target.relative_to(repo_root.resolve()).as_posix())
                else:
                    for lit in literals:
                        if not lit.endswith('.md'):
                            continue
                        target = (rs_file.parent / lit).resolve()
                        discovered.add(target.relative_to(repo_root.resolve()).as_posix())
    return discovered


class ScopeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        for path in ['integrations/openclaw/config.ts', 'Cargo.toml',
                     *RETIRED_PRESENTATION_PATHS, *ORDINARY_PROSE_PATHS, *FULL_SCOPE_PATHS]:
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

    def test_ordinary_docs_wiki_cards_narrow_but_retired_presentation_stays_full(self):
        for path, profile in [*[(p, 'docs_prose') for p in ORDINARY_PROSE_PATHS],
                              *[(p, 'full') for p in RETIRED_PRESENTATION_PATHS]]:
            with self.subTest(path=path):
                self.git('reset', '--hard', self.base)
                self.edit(path)
                self.commit()
                scope = self.scope()
                self.assertEqual(scope['profile'], profile)
                self.assertEqual(scope['checkout_sha'], self.git('rev-parse', 'HEAD'))
                self.assertEqual(scope['checkout_tree'], self.git('rev-parse', 'HEAD^{tree}'))

    def test_mixed_ordinary_prose_across_docs_wiki_and_cards_narrows_together(self):
        self.git('reset', '--hard', self.base)
        self.edit('docs/engineering/operations/recall-eval.md')
        self.edit('wiki/skills/eng-auto-checkpoint.md')
        self.edit('cards/sample-lane.md')
        self.edit('docs/new-guide.md')
        self.commit()
        scope = self.scope()
        self.assertEqual(scope['profile'], 'docs_prose')
        self.assertEqual(scope['reason'], 'approved_prose_scope')

    def test_add_delete_and_rename_of_ordinary_prose_narrows_to_docs_prose(self):
        # Add new docs, wiki, and cards markdown files
        for new_path in ('docs/new-guide.md', 'wiki/new-page.md', 'cards/new-card.md'):
            with self.subTest(action='add', path=new_path):
                self.git('reset', '--hard', self.base)
                self.edit(new_path)
                self.commit()
                self.assertEqual(self.scope()['profile'], 'docs_prose')

        # Delete existing non-locked prose files
        for existing in ('docs/UPGRADING.md', 'wiki/skills/eng-auto-checkpoint.md', 'cards/sample-lane.md'):
            with self.subTest(action='delete', path=existing):
                self.git('reset', '--hard', self.base)
                (self.repo / existing).unlink()
                self.commit()
                self.assertEqual(self.scope()['profile'], 'docs_prose')

        # Rename existing non-locked prose file (represented as D + A by --no-renames)
        for src, dst in (
            ('docs/UPGRADING.md', 'docs/UPGRADING-V2.md'),
            ('wiki/skills/eng-auto-checkpoint.md', 'docs/migrated-checkpoint.md'),
        ):
            with self.subTest(action='rename', src=src, dst=dst):
                self.git('reset', '--hard', self.base)
                target_dst = self.repo / dst
                target_dst.parent.mkdir(parents=True, exist_ok=True)
                (self.repo / src).rename(target_dst)
                self.commit()
                self.assertEqual(self.scope()['profile'], 'docs_prose')

    def test_locked_docs_and_full_scope_paths_never_exempt_even_on_add_delete_rename_or_mixed(self):
        for path in FULL_SCOPE_PATHS:
            for paths in ((path,), (ORDINARY_PROSE_PATHS[0], path)):
                with self.subTest(action='modify', paths=paths):
                    self.git('reset', '--hard', self.base)
                    for changed in paths:
                        self.edit(changed)
                    self.commit()
                    self.assertEqual(self.scope()['profile'], 'full')

        for locked_doc in sorted(RUST_OR_GOVERNANCE_LOCKED_DOCS):
            with self.subTest(action='delete_locked', path=locked_doc):
                self.git('reset', '--hard', self.base)
                (self.repo / locked_doc).unlink()
                self.commit()
                self.assertEqual(self.scope()['profile'], 'full')

            with self.subTest(action='rename_to_locked', path=locked_doc):
                self.git('reset', '--hard', self.base)
                (self.repo / locked_doc).unlink()
                without_locked = self.commit()
                self.edit('docs/UPGRADING.md')
                (self.repo / 'docs/UPGRADING.md').rename(self.repo / locked_doc)
                self.commit()
                self.assertEqual(self.scope(base=without_locked)['profile'], 'full')

    def test_all_rust_embedded_and_contract_markdown_files_stay_full(self):
        embedded_md = _discover_rust_embedded_markdown_paths(ROOT)
        self.assertGreaterEqual(len(embedded_md), 6, 'expected Rust-embedded markdown files')
        self.assertIn('README.md', embedded_md)
        self.assertIn('docs/INSTALL.md', embedded_md)
        self.assertIn('docs/engineering/architecture/portable-kernel-split.md', embedded_md)
        self.assertIn('docs/engineering/architecture/downstream-sync-surface.md', embedded_md)

        contract_fixture = json.loads(
            (ROOT / 'docs/engineering/architecture/memory-action-contract-v1.fixture.json').read_text()
        )
        contract_md = {
            path for path in contract_fixture['active_contract_files']
            if path.endswith('.md')
        }
        all_guarded_md = sorted(embedded_md | contract_md)

        for md_path in all_guarded_md:
            with self.subTest(mode='modify', path=md_path):
                self.git('reset', '--hard', self.base)
                self.edit(md_path)
                self.commit()
                self.assertEqual(
                    self.scope()['profile'], 'full',
                    f'Rust-consumed markdown {md_path} must never bypass Rust CI',
                )
            with self.subTest(mode='add', path=md_path):
                self.git('reset', '--hard', self.base)
                target = self.repo / md_path
                if target.exists():
                    target.unlink()
                    without_file = self.commit()
                else:
                    without_file = self.base
                self.edit(md_path)
                self.commit()
                self.assertEqual(
                    self.scope(base=without_file)['profile'], 'full',
                    f'Added Rust-consumed markdown {md_path} must never bypass Rust CI',
                )

    def test_non_markdown_and_outside_roots_remain_full(self):
        cases = [
            ('Cargo.toml',),
            ('integrations/openclaw/config.ts',),
            ('docs/new.json',),
            ('docs/new.yaml',),
            ('wiki/script.py',),
            ('cards/schema.toml',),
            ('unknown.md',),
            ('unknown',),
            ('docs/UPGRADING.md', 'packages/tachi-cli/src/utils/ui.ts'),
        ]
        for paths in cases:
            with self.subTest(paths=paths):
                self.git('reset', '--hard', self.base)
                for path in paths:
                    self.edit(path)
                self.commit()
                self.assertEqual(self.scope()['profile'], 'full')

    def test_retired_node_cache_workflow_changes_stay_full(self):
        path = '.github/workflows/ci.yml'
        source = """name: Retired cache fixture
jobs:
  node:
    steps:
      - name: Select Tachi local npm cache
        # Resolve runner paths inside a step; job-level env cannot use runner context.
        shell: bash
        run: printf 'NPM_CONFIG_CACHE=%s/tachi/npm\\n' "${RUNNER_TOOL_CACHE:?}" >> "$GITHUB_ENV"

      - uses: actions/setup-node@a0853c24544627f65ddf259abe73b1d18a591444 # v5.0.0
        with:
          node-version: 20
          package-manager-cache: false
  gitleaks:
    steps:
      - run: echo scan
"""
        target = self.repo / path
        target.write_text(source)
        self.base = self.commit()
        target.write_text(source.replace('/tachi/npm', '/tachi/npm-v2'))
        self.commit()
        self.assertEqual(self.scope()['profile'], 'full')

    def test_executable_mode_and_symlink_entries_remain_full(self):
        for path in ('docs/UPGRADING.md', 'wiki/skills/eng-auto-checkpoint.md', 'cards/sample-lane.md'):
            target = self.repo / path
            for action in ('mode', 'symlink', 'add_exec', 'add_symlink'):
                with self.subTest(path=path, action=action):
                    self.git('reset', '--hard', self.base)
                    if action == 'mode':
                        target.chmod(0o755)
                    elif action == 'symlink':
                        target.unlink()
                        target.symlink_to('../INSTALL.md')
                    elif action == 'add_exec':
                        extra = target.with_name('exec-doc.md')
                        extra.write_text('exec\n')
                        extra.chmod(0o755)
                    else:
                        extra = target.with_name('sym-doc.md')
                        extra.symlink_to('../INSTALL.md')
                    self.commit()
                    self.assertEqual(self.scope()['profile'], 'full')

    def test_unproven_inputs_and_non_pr_events_never_narrow(self):
        self.assertEqual(self.scope()['profile'], 'full')  # empty diff
        for path in ('docs/UPGRADING.md', ORDINARY_PROSE_PATHS[0]):
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
        self.edit('docs/UPGRADING.md')
        head = self.commit()
        self.git('checkout', '-qb', 'advanced-base', self.base)
        self.edit('README.md')
        base = self.commit()
        self.git('merge', '--no-ff', '-qm', 'merge fixture', 'candidate')
        with tempfile.TemporaryDirectory() as d:
            clone = Path(d) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth=2', self.repo.as_uri(), str(clone)], check=True)
            self.assertEqual(classify(clone, 'pull_request', head, base)['profile'], 'docs_prose')
            # An advertised older base cannot silently hide the README change.
            self.assertEqual(classify(clone, 'pull_request', head, self.base)['profile'], 'full')

    def test_main_merge_push_and_manual_runs_take_full_checks(self):
        # PRs need not be up to date with main, so every merge on main is
        # integration-tested in full, even when the merged change is prose.
        self.git('checkout', '-qb', 'candidate')
        self.edit('docs/UPGRADING.md')
        candidate = self.commit()
        self.git('checkout', '-qb', 'main-base', self.base)
        self.git('merge', '--no-ff', '-qm', 'merge fixture', candidate)
        merged = self.git('rev-parse', 'HEAD')
        for event in ('push', 'workflow_dispatch'):
            scope = classify(self.repo, event, merged, '')
            self.assertEqual((scope['profile'], scope['reason']), ('full', 'non_pr_event'))
        self.git('reset', '--hard', candidate)

    def test_missing_shallow_base_keeps_full(self):
        self.edit('docs/UPGRADING.md')
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
                       {'profile': 'unknown'}, {'profile': 'docs_prose'},
                       {'profile': 'docs_prose', 'checkout_tree': tree},
                       {'profile': 'docs_prose', 'checkout_tree': '2' * 40}]
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

    def test_main_binds_prose_profile_and_tree_to_real_classification(self):
        self.edit(ORDINARY_PROSE_PATHS[0])
        self.commit()
        tree = self.git('rev-parse', 'HEAD^{tree}')
        for outputs, expected in (
                ({'profile': 'docs_prose', 'checkout_tree': tree}, 0),
                ({'profile': 'full', 'checkout_tree': tree}, 1),
                ({'profile': 'docs_prose', 'checkout_tree': '2' * 40}, 1),
                ({'profile': 'docs_prose', 'checkout_tree': ''}, 1),
                ({'profile': 'docs_prose'}, 1)):
            with self.subTest(outputs=outputs):
                producer = {'result': 'success', 'outputs': outputs}
                self.assertEqual(self.acceptance_main('pull_request', producer)[0], expected)
        # A dirty checkout falls back to full and cannot reuse the narrow producer.
        self.edit('README.md')
        producer = {'result': 'success',
                    'outputs': {'profile': 'docs_prose', 'checkout_tree': tree}}
        self.assertEqual(self.acceptance_main('pull_request', producer)[0], 1)


class ApplicabilityAcceptanceTests(unittest.TestCase):
    def needs(self, profile):
        rows = {job: {'result': 'success', 'outputs': {}}
                for job in PLAN['required_jobs'] + PLAN['observational_jobs']}
        for job in exclusions(PLAN, profile):
            rows[job]['result'] = 'skipped'
        return rows

    def test_only_approved_skips_are_not_applicable_not_passed(self):
        for profile in ('archive_prose', 'operational_prose', 'docs_prose'):
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
            del needs['gitleaks']
            with self.assertRaises(acceptance.InvalidEvidence):
                acceptance.evaluate(PLAN, needs, profile=profile)

    def test_profile_cannot_exempt_setup_secret_scan_or_windows_inventory(self):
        forbidden = [(profile, job)
                     for profile in ('archive_prose', 'operational_prose', 'docs_prose')
                     for job in ('gitleaks', 'build-seat-setup', 'physical-db-identity-windows')]
        for profile, job in forbidden + [('full', 'rust')]:
            plan = json.loads(json.dumps(PLAN))
            plan['applicability_profiles'][profile].append(job)
            with self.assertRaises(acceptance.InvalidEvidence):
                acceptance.evaluate(plan, self.needs('full'))

    def test_retired_profiles_are_rejected(self):
        for profile in ('node_presentation', 'node_cache'):
            with self.subTest(profile=profile):
                with self.assertRaises(acceptance.InvalidEvidence):
                    acceptance.evaluate(PLAN, self.needs('full'), profile=profile)
                plan = json.loads(json.dumps(PLAN))
                plan['applicability_profiles'][profile] = ['rust']
                with self.assertRaises(acceptance.InvalidEvidence):
                    acceptance.evaluate(plan, self.needs('full'))

    def test_main_requires_matching_successful_scope_producer(self):
        for profile in ('archive_prose', 'operational_prose', 'docs_prose'):
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
            self.assertEqual(json.loads(out.getvalue())['required_jobs']['rust'], 'not_applicable')

    def test_main_never_accepts_matching_absent_or_invalid_reduced_tree(self):
        for tree in (None, '', 'not-a-tree', [], '1' * 39):
            with self.subTest(tree=tree):
                scope = {'profile': 'docs_prose', 'checkout_tree': tree}
                needs = self.needs('docs_prose')
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
        self.assertIn('test_ci_affected_crates.py', setup)
        self.assertIn("needs.build-seat-setup.outputs.run_rust == 'true'", full)


if __name__ == '__main__':
    unittest.main()
