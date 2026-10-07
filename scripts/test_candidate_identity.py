"""Real Git fixtures for source identity (no Cargo or network)."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from scripts.candidate_identity import git, record


class CandidateIdentityTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        git(self.repo, 'init', '-q')
        git(self.repo, 'config', 'user.email', 'fixture@example.invalid')
        git(self.repo, 'config', 'user.name', 'Fixture')
        (self.repo / 'source').write_text('one\n')
        git(self.repo, 'add', 'source')
        git(self.repo, 'commit', '-qm', 'initial')
        self.first = git(self.repo, 'rev-parse', 'HEAD')

    def test_equal_content_different_commit_is_not_acceptance(self):
        git(self.repo, 'commit', '--allow-empty', '-qm', 'metadata only')
        result = record(self.repo, self.first)
        self.assertEqual(result['relation'], 'same_tree')
        self.assertNotEqual(result['requested_head'], result['checkout_sha'])
        self.assertFalse(result['acceptance_granted'])

    def test_changed_source_is_distinguished_from_same_commit(self):
        self.assertEqual(record(self.repo, self.first)['relation'], 'same_commit')
        (self.repo / 'source').write_text('two\n')
        git(self.repo, 'commit', '-qam', 'change')
        result = record(self.repo, self.first, self.first)
        self.assertEqual(result['relation'], 'different_tree')
        self.assertNotEqual(result['requested_tree'], result['checkout_tree'])

    def test_depth_two_merge_checkout_can_resolve_head_and_base(self):
        git(self.repo, 'checkout', '-qb', 'feature')
        (self.repo / 'feature').write_text('feature\n')
        git(self.repo, 'add', 'feature')
        git(self.repo, 'commit', '-qm', 'feature')
        head = git(self.repo, 'rev-parse', 'HEAD')
        git(self.repo, 'checkout', '--detach', self.first)
        git(self.repo, 'merge', '--no-ff', '-m', 'merge', head)
        with tempfile.TemporaryDirectory() as d:
            clone = Path(d) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth=2', self.repo.as_uri(), str(clone)], check=True)
            result = record(clone, head, self.first)
            self.assertEqual(result['relation'], 'same_tree')
            self.assertNotEqual(result['checkout_sha'], head)

    def test_dirty_tracked_and_untracked_sources_refuse(self):
        (self.repo / 'source').write_text('dirty\n')
        with self.assertRaises(ValueError):
            record(self.repo, self.first)
        git(self.repo, 'checkout', '--', 'source')
        (self.repo / 'new').write_text('untracked\n')
        with self.assertRaises(ValueError):
            record(self.repo, self.first)

    def test_refs_tags_missing_objects_and_short_ids_refuse(self):
        git(self.repo, 'tag', '-am', 'tag', 'annotated')
        tag = git(self.repo, 'rev-parse', 'annotated')
        for invalid in ('HEAD', self.first[:8], '-' * 40, '0' * 40, tag):
            with self.subTest(invalid=invalid), self.assertRaises((ValueError, subprocess.CalledProcessError)):
                record(self.repo, invalid)
        with self.assertRaises(subprocess.CalledProcessError):
            record(self.repo, self.first, '0' * 40)

    def test_cli_reports_json_and_rejects_invalid_input_without_echo(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        args = ['python3', str(script), '--repo', str(self.repo), '--requested-head']
        ok = subprocess.run([*args, self.first], capture_output=True, text=True)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertEqual(json.loads(ok.stdout)['relation'], 'same_commit')
        bad = subprocess.run([*args, 'private-invalid-input'], capture_output=True, text=True)
        self.assertNotEqual(bad.returncode, 0)
        self.assertNotIn('private-invalid-input', bad.stderr)


if __name__ == '__main__':
    unittest.main()
