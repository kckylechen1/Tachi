"""Root installer regression tests; fake Cargo, fake binaries, disposable HOME."""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FAKE_BUILD_ROOT'])
(root / 'cargo-args').write_text(json.dumps(sys.argv[1:]))
if os.environ.get('FAKE_MODE') == 'cargo_failed': sys.exit(1)
out = pathlib.Path(os.environ['FAKE_OUTPUT'])
out.mkdir(parents=True, exist_ok=True)
for name in ('tachi-server', 'tachi-clean'):
    binary = out / name
    body = '#!/bin/sh\necho new-' + name + '\n'
    if os.environ.get('FAKE_MODE') == 'bad_stage' and name == 'tachi-clean': body += 'exit 1\n'
    if os.environ.get('FAKE_MODE') == 'bad_installed' and name == 'tachi-clean':
        body += 'case "$0" in */.tachi-clean.new.*) exit 0;; *) exit 1;; esac\n'
    binary.write_text(body); binary.chmod(0o755)
    if os.environ.get('FAKE_MODE') == 'missing_artifact' and name == 'tachi-clean': continue
    if '--message-format=json' in sys.argv:
        print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': name, 'kind': ['bin']},
                          'profile': {'test': False}, 'executable': str(binary), 'fresh': True}))
if '--message-format=json' in sys.argv:
    print(json.dumps({'reason': 'build-finished', 'success': True}))
'''


def executable(path: Path, body: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body); path.chmod(0o755)


class LegacyBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir(); (self.repo / 'scripts').mkdir()
        shutil.copyfile(ROOT / 'build.sh', self.repo / 'build.sh')
        helper = ROOT / 'scripts/install_legacy_binaries.py'
        if helper.exists(): shutil.copyfile(helper, self.repo / 'scripts' / helper.name)
        self.home = self.root / 'home'; self.home.mkdir()
        self.fake_bin = self.root / 'fake-bin'; self.fake_bin.mkdir()
        executable(self.fake_bin / 'cargo', FAKE_CARGO)
        # These stale files catch accidental fallback to repo/target/release.
        for name in ('tachi-server', 'tachi-clean'):
            executable(self.repo / 'target/release' / name, '#!/bin/sh\necho stale-' + name + '\n')
        self.env = os.environ | {
            'HOME': str(self.home), 'PATH': str(self.fake_bin) + os.pathsep + os.environ['PATH'],
            'CARGO_TARGET_DIR': str(self.root / 'target from env'),
            'FAKE_BUILD_ROOT': str(self.root), 'FAKE_OUTPUT': str(self.root / 'configured target/triple/release'),
        }
        for key in ('INSTALL_BIN', 'INSTALL_CLEAN_BIN', 'TACHI_LINK', 'CLEAN_LINK', 'SKIP_LINK'):
            self.env.pop(key, None)
        self.binary = self.home / '.cargo/bin/tachi-server'
        self.cleaner = self.home / '.cargo/bin/tachi-clean'
        self.link = self.home / 'bin/tachi'
        self.clean_link = self.home / 'bin/tachi-clean'

    def tearDown(self): self.temp.cleanup()

    def run_build(self, mode='ready'):
        return subprocess.run(['bash', str(self.repo / 'build.sh')], cwd=self.root,
                              env=self.env | {'FAKE_MODE': mode}, text=True,
                              capture_output=True, timeout=15)

    def old_entries(self):
        self.binary.parent.mkdir(parents=True)
        self.binary.write_text('old-server'); self.cleaner.write_text('old-cleaner')
        self.link.parent.mkdir(parents=True)
        self.referent = self.root / 'legacy'
        self.referent.write_text('legacy-referent')
        self.link.symlink_to(self.referent)
        self.clean_link.write_text('old-clean-link')

    def assert_old_entries(self):
        self.assertEqual(self.binary.read_text(), 'old-server')
        self.assertEqual(self.cleaner.read_text(), 'old-cleaner')
        self.assertTrue(self.link.is_symlink())
        self.assertEqual(os.readlink(self.link), str(self.referent))
        self.assertEqual(self.referent.read_text(), 'legacy-referent')
        self.assertEqual(self.clean_link.read_text(), 'old-clean-link')
        self.assertEqual(list(self.home.rglob('*.backup.*')), [])
        self.assertEqual(list(self.home.rglob('.*.new.*')), [])

    def test_uses_cargo_artifacts_even_with_env_config_triple_and_stale_outputs(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('new-tachi-server', self.binary.read_text())
        self.assertIn('new-tachi-clean', self.cleaner.read_text())
        self.assertEqual(Path(os.readlink(self.link)).resolve(), self.binary.resolve())
        self.assertEqual(Path(os.readlink(self.clean_link)).resolve(), self.cleaner.resolve())
        self.assertIn('--message-format=json', json.loads((self.root / 'cargo-args').read_text()))

    def test_success_preserves_original_symlink_and_real_files_as_backups(self):
        self.old_entries()
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        backups = list(self.link.parent.glob('tachi.backup.*'))
        self.assertEqual(len(backups), 1)
        self.assertTrue(backups[0].is_symlink())
        self.assertEqual(os.readlink(backups[0]), str(self.referent))
        self.assertEqual(self.referent.read_text(), 'legacy-referent')
        self.assertEqual(list(self.binary.parent.glob('tachi-server.backup.*'))[0].read_text(), 'old-server')

    def test_cargo_failure_or_missing_artifact_never_falls_back(self):
        self.old_entries()
        for mode in ('cargo_failed', 'missing_artifact'):
            with self.subTest(mode=mode):
                result = self.run_build(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assert_old_entries()

    def test_stage_check_failure_preserves_every_old_entry(self):
        self.old_entries()
        result = self.run_build('bad_stage')
        self.assertNotEqual(result.returncode, 0)
        self.assert_old_entries()

    def test_installed_check_failure_rolls_back_both_binaries_and_links(self):
        self.old_entries()
        result = self.run_build('bad_installed')
        self.assertNotEqual(result.returncode, 0)
        self.assert_old_entries()

    def test_failed_fresh_install_removes_new_entries(self):
        result = self.run_build('bad_installed')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.binary.exists()); self.assertFalse(self.cleaner.exists())
        self.assertFalse(self.link.is_symlink()); self.assertFalse(self.clean_link.is_symlink())
        self.assertEqual(list(self.home.rglob('.*.new.*')), [])

    def test_custom_paths_and_skip_link_compatibility(self):
        self.env.update(INSTALL_BIN=str(self.home / 'custom/server'),
                        INSTALL_CLEAN_BIN=str(self.home / 'custom/cleaner'), SKIP_LINK='1')
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('new-tachi-server', (self.home / 'custom/server').read_text())
        self.assertIn('new-tachi-clean', (self.home / 'custom/cleaner').read_text())
        self.assertFalse(self.link.exists()); self.assertFalse(self.binary.exists())

    def test_relative_install_paths_remain_relative_to_repository(self):
        self.env.update(INSTALL_BIN='installed/server', INSTALL_CLEAN_BIN='installed/cleaner', SKIP_LINK='1')
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('new-tachi-server', (self.repo / 'installed/server').read_text())
        self.assertIn('new-tachi-clean', (self.repo / 'installed/cleaner').read_text())

    def test_custom_link_paths(self):
        self.env.update(TACHI_LINK=str(self.home / 'custom/tachi'), CLEAN_LINK=str(self.home / 'custom/clean'))
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(os.readlink(self.home / 'custom/tachi')).resolve(), self.binary.resolve())
        self.assertEqual(Path(os.readlink(self.home / 'custom/clean')).resolve(), self.cleaner.resolve())

    def test_directory_and_overlapping_targets_refuse_before_cargo(self):
        for target in (self.home, self.cleaner, self.home / '.cargo'):
            with self.subTest(target=target):
                self.env['INSTALL_BIN'] = str(target)
                result = self.run_build()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / 'cargo-args').exists())

    def test_parent_symlink_alias_collision_is_rejected(self):
        real = self.home / 'real'; real.mkdir()
        (self.home / 'alias').symlink_to(real)
        self.env.update(INSTALL_BIN=str(real / 'same'), INSTALL_CLEAN_BIN=str(self.home / 'alias/same'))
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'cargo-args').exists())

    def test_existing_compatibility_links_do_not_alias_their_binary_entries(self):
        first = self.run_build()
        self.assertEqual(first.returncode, 0, first.stderr)
        second = self.run_build()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(Path(os.readlink(self.link)).resolve(), self.binary.resolve())
        self.assertEqual(Path(os.readlink(self.clean_link)).resolve(), self.cleaner.resolve())
        self.assertIn('new-tachi-server', self.binary.read_text())
        self.assertIn('new-tachi-clean', self.cleaner.read_text())

    def test_case_and_unicode_aliases_refuse_before_cargo(self):
        # Deliberately run on both case-sensitive Linux and macOS: the installer
        # conservatively refuses these names without assuming host semantics.
        for first, second in (('tool', 'TOOL'), ('caf\u00e9', 'cafe\u0301')):
            for existing in (False, True):
                with self.subTest(first=first, second=second, existing=existing):
                    target = self.home / 'ambiguous' / first
                    if existing:
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_text('existing entry')
                    self.env.update(TACHI_LINK=str(target),
                                    CLEAN_LINK=str(target.with_name(second)))
                    result = self.run_build()
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn('ambiguous install targets', result.stderr)
                    self.assertFalse((self.root / 'cargo-args').exists())
                    self.assertFalse(self.binary.exists())
                    self.assertFalse(self.cleaner.exists())
                    if existing:
                        self.assertEqual(target.read_text(), 'existing entry')
                        target.unlink()
                    self.assertEqual(list(self.home.rglob('*.backup.*')), [])

    def test_casefolded_parent_and_ancestor_aliases_refuse_before_cargo(self):
        for first, second in (('Parent/tool', 'parent/TOOL'),
                              ('Parent/tool', 'parent/TOOL/child'),
                              ('caf\u00e9/tool', 'cafe\u0301/TOOL')):
            with self.subTest(first=first, second=second):
                self.env.update(TACHI_LINK=str(self.home / first), CLEAN_LINK=str(self.home / second))
                result = self.run_build()
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn('ambiguous install targets', result.stderr)
                self.assertFalse((self.root / 'cargo-args').exists())
                self.assertFalse(self.binary.exists())
                self.assertFalse(self.cleaner.exists())

    def test_install_target_cannot_be_a_cargo_output(self):
        self.env['INSTALL_BIN'] = str(Path(self.env['FAKE_OUTPUT']) / 'tachi-server')
        result = self.run_build()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('overlaps a Cargo executable', result.stderr)
        self.assertFalse(self.cleaner.exists())

    def test_replace_failure_restores_moved_symlink_without_touching_referent(self):
        self.old_entries()
        spec = importlib.util.spec_from_file_location('legacy_installer', ROOT / 'scripts/install_legacy_binaries.py')
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        artifacts = {}
        for name in ('tachi-server', 'tachi-clean'):
            path = self.root / 'artifacts' / name
            executable(path, '#!/bin/sh\necho good\n'); artifacts[name] = path
        real_replace = os.replace

        def fail_final_link(source, target):
            if Path(target) == self.clean_link and '.new.' in Path(source).name:
                raise OSError('injected final link replacement failure')
            return real_replace(source, target)

        with patch.object(module.os, 'replace', side_effect=fail_final_link):
            with self.assertRaisesRegex(OSError, 'injected'):
                module.install(artifacts, [self.binary, self.cleaner], [self.link, self.clean_link])
        self.assert_old_entries()


if __name__ == '__main__': unittest.main()
