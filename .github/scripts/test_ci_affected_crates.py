"""Real Git + synthetic cargo-metadata tests for fail-closed PR crate scoping."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import ci_affected_crates as affected
from ci_scope import RUST_OR_GOVERNANCE_LOCKED_DOCS

ROOT = Path(__file__).resolve().parents[2]


def _make_metadata(repo: Path) -> dict:
    """Build realistic workspace metadata mirroring Tachi normal/dev/build edges."""
    specs = {
        'memcore': ('crates/memcore', [('vault-kit', None)]),
        'vault-kit': ('crates/vault-kit', []),
        'memory-server-runtime': ('crates/memory-server-runtime', [('memcore', None)]),
        'tachi-params': ('crates/tachi-params', [('memcore', None)]),
        'tachi-dispatch': ('crates/tachi-dispatch', [('tachi-params', None)]),
        'tachi-llm': ('crates/tachi-llm', [('memcore', None), ('memcore', 'dev')]),
        'tachi-runtime-core': ('crates/tachi-runtime-core', [('memcore', None)]),
        'tachi-clean': ('tools/cleaner', [('memcore', None), ('tachi-runtime-core', None)]),
        'tachi-build-gen': ('crates/tachi-build-gen', []),
        'tachi-server': (
            'crates/tachi-server',
            [
                ('memcore', None),
                ('memory-server-runtime', None),
                ('tachi-params', None),
                ('tachi-dispatch', None),
                ('tachi-llm', None),
                ('tachi-clean', None),
                ('tachi-build-gen', 'build'),
            ],
        ),
        'portable-kernel': ('crates/portable-kernel', [('memcore', None)]),
        'portable-server': ('crates/portable-server', [('portable-kernel', None)]),
        'tachi-contract-tests': ('crates/tachi-contract-tests', [('tachi-params', 'dev')]),
        'tachi-bootstrap-tests': (
            'crates/tachi-bootstrap-tests',
            [('memcore', 'dev'), ('tachi-server', 'dev')],
        ),
        'tachi-delivery-tests': (
            'crates/tachi-delivery-tests',
            [('memcore', 'dev'), ('tachi-server', 'dev')],
        ),
        'tachi-vault-tests': (
            'crates/tachi-vault-tests',
            [('memcore', 'dev'), ('tachi-server', 'dev'), ('vault-kit', 'dev')],
        ),
    }
    packages = []
    workspace_members = []
    for name, (rel_dir, deps) in specs.items():
        pkg_id = f'path+file://{repo / rel_dir}#{name}@0.1.0'
        workspace_members.append(pkg_id)
        packages.append({
            'id': pkg_id,
            'name': name,
            'manifest_path': str(repo / rel_dir / 'Cargo.toml'),
            'dependencies': [
                {'name': dep_name, 'kind': kind}
                for dep_name, kind in deps
            ],
        })
    return {'packages': packages, 'workspace_members': workspace_members}


class AffectedCratesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'user.name', 'Fixture')
        self.metadata = _make_metadata(self.repo)
        seed_files = [
            'Cargo.toml',
            'Cargo.lock',
            'rust-toolchain.toml',
            '.cargo/config.toml',
            '.config/nextest.toml',
            '.github/workflows/ci.yml',
            'scripts/dogfood_e2e.py',
            'docs/INSTALL.md',
            'docs/engineering/operations/ci-scheduling.md',
            'docs/engineering/operations/recall-eval.md',
            'docs/engineering/architecture/kernel-surface-v1.fixture.json',
            'wiki/notes.md',
            'cards/sample.md',
            'README.md',
            'crates/memcore/Cargo.toml',
            'crates/memcore/src/lib.rs',
            'crates/vault-kit/Cargo.toml',
            'crates/vault-kit/src/lib.rs',
            'crates/memory-server-runtime/Cargo.toml',
            'crates/memory-server-runtime/src/lib.rs',
            'crates/tachi-params/Cargo.toml',
            'crates/tachi-params/src/lib.rs',
            'crates/tachi-dispatch/Cargo.toml',
            'crates/tachi-dispatch/src/lib.rs',
            'crates/tachi-llm/Cargo.toml',
            'crates/tachi-llm/src/lib.rs',
            'crates/tachi-runtime-core/Cargo.toml',
            'crates/tachi-runtime-core/src/lib.rs',
            'tools/cleaner/Cargo.toml',
            'tools/cleaner/src/lib.rs',
            'crates/tachi-build-gen/Cargo.toml',
            'crates/tachi-build-gen/src/lib.rs',
            'crates/tachi-server/Cargo.toml',
            'crates/tachi-server/src/lib.rs',
            'crates/portable-kernel/Cargo.toml',
            'crates/portable-kernel/src/lib.rs',
            'crates/portable-server/Cargo.toml',
            'crates/portable-server/src/lib.rs',
            'crates/tachi-contract-tests/Cargo.toml',
            'crates/tachi-contract-tests/src/lib.rs',
            'crates/tachi-bootstrap-tests/Cargo.toml',
            'crates/tachi-bootstrap-tests/src/lib.rs',
            'crates/tachi-delivery-tests/Cargo.toml',
            'crates/tachi-delivery-tests/src/lib.rs',
            'crates/tachi-vault-tests/Cargo.toml',
            'crates/tachi-vault-tests/src/lib.rs',
        ]
        for rel in seed_files:
            target = self.repo / rel
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

    def edit(self, rel, content='after\n'):
        target = self.repo / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)

    def select(self, event='pull_request', base=None, metadata=...):
        return affected.select_crates(
            self.repo,
            event,
            self.git('rev-parse', 'HEAD'),
            self.base if base is None else base,
            metadata=self.metadata if metadata is ... else metadata,
        )

    def test_tachi_server_edit_selects_reverse_dev_deps_and_skips_unrelated(self):
        self.edit('crates/tachi-server/src/lib.rs')
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertEqual(res['direct_crates'], ['tachi-server'])
        self.assertEqual(
            res['crates'],
            [
                'tachi-bootstrap-tests',
                'tachi-contract-tests',
                'tachi-delivery-tests',
                'tachi-server',
                'tachi-vault-tests',
            ],
        )
        self.assertEqual(
            res['nextest_args'],
            [
                '-p', 'tachi-bootstrap-tests',
                '-p', 'tachi-contract-tests',
                '-p', 'tachi-delivery-tests',
                '-p', 'tachi-server',
                '-p', 'tachi-vault-tests',
            ],
        )

    def test_build_dependency_reverse_edge_propagates(self):
        self.edit('crates/tachi-build-gen/src/lib.rs')
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertIn('tachi-server', res['crates'])
        self.assertIn('tachi-delivery-tests', res['crates'])
        self.assertNotIn('memcore', res['crates'])

    def test_portable_server_cross_crate_include_edge_selects_tachi_server(self):
        self.edit('crates/portable-server/src/lib.rs')
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertIn('portable-server', res['crates'])
        self.assertIn('tachi-server', res['crates'])
        self.assertIn('tachi-vault-tests', res['crates'])
        self.assertNotIn('portable-kernel', res['crates'])
        self.assertNotIn('memcore', res['crates'])

    def test_portable_kernel_edit_selects_portable_server_and_contract_tests(self):
        self.edit('crates/portable-kernel/src/lib.rs')
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertEqual(
            res['crates'],
            [
                'portable-kernel',
                'portable-server',
                'tachi-bootstrap-tests',
                'tachi-contract-tests',
                'tachi-delivery-tests',
                'tachi-server',
                'tachi-vault-tests',
            ],
        )
        self.assertNotIn('memcore', res['crates'])

    def test_ordinary_prose_alongside_crate_change_does_not_force_workspace(self):
        self.edit('crates/vault-kit/src/lib.rs')
        self.edit('docs/engineering/operations/recall-eval.md')
        self.edit('wiki/new-note.md')
        (self.repo / 'cards/sample.md').unlink()
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertEqual(res['prose_ignored'], 3)
        self.assertEqual(
            res['crates'],
            [
                'memcore',
                'memory-server-runtime',
                'portable-kernel',
                'portable-server',
                'tachi-bootstrap-tests',
                'tachi-clean',
                'tachi-contract-tests',
                'tachi-delivery-tests',
                'tachi-dispatch',
                'tachi-llm',
                'tachi-params',
                'tachi-runtime-core',
                'tachi-server',
                'tachi-vault-tests',
                'vault-kit',
            ],
        )

    def test_locked_docs_fixtures_and_non_member_files_fail_closed_to_workspace(self):
        fail_closed_paths = [
            'Cargo.toml',
            'Cargo.lock',
            'rust-toolchain.toml',
            '.cargo/config.toml',
            '.config/nextest.toml',
            '.github/workflows/ci.yml',
            'scripts/dogfood_e2e.py',
            'docs/engineering/architecture/kernel-surface-v1.fixture.json',
            *sorted(RUST_OR_GOVERNANCE_LOCKED_DOCS),
        ]
        for path in fail_closed_paths:
            with self.subTest(path=path):
                self.git('reset', '--hard', self.base)
                self.edit('crates/tachi-llm/src/lib.rs')
                self.edit(path)
                self.commit()
                res = self.select()
                self.assertEqual(res['mode'], 'workspace')
                self.assertEqual(res['nextest_args'], ['--workspace'])
                self.assertEqual(res['reason'], 'non_member_or_locked_path')

    def test_symlinks_and_executable_prose_fail_closed_but_executable_member_file_scopes(self):
        # Executable script inside a crate member stays scoped to that crate
        self.edit('tools/cleaner/helper.sh', '#!/bin/sh\n')
        (self.repo / 'tools/cleaner/helper.sh').chmod(0o755)
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'scoped')
        self.assertIn('tachi-clean', res['crates'])
        self.assertNotIn('memcore', res['crates'])

        # Symlink inside a crate member fails closed
        self.git('reset', '--hard', self.base)
        (self.repo / 'tools/cleaner/sym.rs').symlink_to('src/lib.rs')
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'workspace')
        self.assertEqual(res['reason'], 'structural_change')

        # Executable prose doc alongside a crate edit fails closed
        self.git('reset', '--hard', self.base)
        self.edit('crates/tachi-llm/src/lib.rs')
        (self.repo / 'wiki/notes.md').chmod(0o755)
        self.commit()
        res = self.select()
        self.assertEqual(res['mode'], 'workspace')
        self.assertEqual(res['reason'], 'non_member_or_locked_path')

    def test_non_pr_unproven_and_metadata_failures_fail_closed_to_workspace(self):
        self.edit('crates/tachi-llm/src/lib.rs')
        self.commit()
        for event in ('push', 'workflow_dispatch', '', 'pull_request_target'):
            res = self.select(event=event)
            self.assertEqual((res['mode'], res['reason']), ('workspace', 'non_pr_event'))

        for bad_base in ('HEAD', self.base[:8], '0' * 40):
            res = self.select(base=bad_base)
            self.assertEqual(res['mode'], 'workspace')

        for bad_meta in ({}, {'packages': [], 'workspace_members': []}):
            res = self.select(metadata=bad_meta)
            self.assertEqual((res['mode'], res['reason']), ('workspace', 'metadata_error'))

        # Dirty worktree forces workspace
        self.edit('crates/tachi-llm/src/lib.rs', 'dirty\n')
        res = self.select()
        self.assertEqual((res['mode'], res['reason']), ('workspace', 'dirty_source'))

    def test_cli_nextest_args_and_includes_modes(self):
        scoped_result = {
            'mode': 'scoped',
            'reason': 'reverse_dependency_closure',
            'direct_crates': ['tachi-server'],
            'crates': ['tachi-contract-tests', 'tachi-server'],
            'nextest_args': ['-p', 'tachi-contract-tests', '-p', 'tachi-server'],
            'prose_ignored': 0,
        }
        stdout = io.StringIO()
        stderr = io.StringIO()
        with patch.object(affected, 'current_selection', return_value=scoped_result), \
             patch('sys.argv', ['ci_affected_crates.py', '--nextest-args']), \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            rc = affected.main()
        self.assertEqual(rc, 0)
        self.assertEqual(stdout.getvalue().splitlines(), ['-p', 'tachi-contract-tests', '-p', 'tachi-server'])
        self.assertIn('"mode": "scoped"', stderr.getvalue())

        for target, expected_rc in (('tachi-server', 0), ('portable-kernel', 1)):
            stderr = io.StringIO()
            with patch.object(affected, 'current_selection', return_value=scoped_result), \
                 patch('sys.argv', ['ci_affected_crates.py', '--includes', target]), \
                 contextlib.redirect_stderr(stderr):
                self.assertEqual(affected.main(), expected_rc)

        # Unhandled internal errors still fail closed (exit 0 on --includes, --workspace on --nextest-args)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with patch.object(affected, 'current_selection', side_effect=RuntimeError('boom')), \
             patch('sys.argv', ['ci_affected_crates.py', '--nextest-args']), \
             contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.assertEqual(affected.main(), 0)
        self.assertEqual(stdout.getvalue().splitlines(), ['--workspace'])

        with patch.object(affected, 'current_selection', side_effect=RuntimeError('boom')), \
             patch('sys.argv', ['ci_affected_crates.py', '--includes', 'portable-kernel']), \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(affected.main(), 0)

    def test_real_workspace_toml_members_match_explicit_cross_crate_edges(self):
        root_toml = tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))
        member_names = set()
        for member_rel in root_toml['workspace']['members']:
            pkg_toml = tomllib.loads((ROOT / member_rel / 'Cargo.toml').read_text(encoding='utf-8'))
            member_names.add(pkg_toml['package']['name'])
        for pkg in affected.ALWAYS_INCLUDED_CRATES:
            self.assertIn(pkg, member_names)
        for src_pkg, dependents in affected.EXTRA_REVERSE_DEPS.items():
            self.assertIn(src_pkg, member_names)
            for dep_pkg in dependents:
                self.assertIn(dep_pkg, member_names)


if __name__ == '__main__':
    unittest.main()
