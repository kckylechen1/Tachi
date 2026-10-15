"""The project consumes pinned host code; project budget/policy remain local."""
import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HostBuildVendorTests(unittest.TestCase):
    def test_exported_runtime_matches_pinned_upstream_manifest(self):
        manifest = json.loads((ROOT / '.github/runner-config/host-build-upstream.json').read_text())
        self.assertEqual(manifest['schema'], 1)
        self.assertEqual(manifest['repository'], 'kckylechen1/agent-control-plane')
        self.assertRegex(manifest['revision'], r'^[0-9a-f]{40}$')
        self.assertEqual(set(manifest['files']), {'cargo', 'cargo-gate-hold', 'cargo-gate-ci-start',
                                                'cargo-gate-ci-end', 'host-build'})
        for name, expected in manifest['files'].items():
            self.assertEqual(hashlib.sha256((ROOT / '.github/scripts' / name).read_bytes()).hexdigest(), expected, name)

    def test_light_job_policy_is_owned_by_exact_tachi_workflow(self):
        policy = json.loads((ROOT / '.github/runner-config/cargo-gate-policy.json').read_text())
        self.assertEqual(policy, {'schema':1, 'light_jobs':[{
            'repository':'kckylechen1/Tachi', 'workflow':'Full CI',
            'jobs':['node','gitleaks','acceptance','build-seat-setup']}]})

    def test_linux_heavy_invocations_use_installed_host_entry(self):
        text = (ROOT / '.github/workflows/conformance-linux.yml').read_text()
        heavy = [line.strip() for line in text.splitlines()
                 if line.strip().startswith(('run: cargo test ', 'run: cargo clippy ',
                    'run: "$HOME/bin/host-build"', '"${AUDITED_NEXTEST:?}" nextest run ',
                    '"$HOME/bin/host-build" -- '))]
        self.assertGreaterEqual(len(heavy), 8)
        for line in heavy:
            self.assertIn('"$HOME/bin/host-build" -- ', line)


if __name__ == '__main__':
    unittest.main()
