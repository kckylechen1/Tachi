"""The project consumes pinned host code; project budget/policy remain local."""
import hashlib
import json
import subprocess
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

    def workflows(self):
        return {name: json.loads(subprocess.check_output([
            'ruby', '-rjson', '-ryaml', '-e',
            'puts JSON.generate(YAML.load_file(ARGV[0]))',
            str(ROOT / '.github/workflows' / name)], text=True))
            for name in ('ci.yml', 'conformance-linux.yml')}

    def test_required_workflows_have_no_shared_host_dependency(self):
        expected = {
            'ci.yml': {'build-seat-setup': 'ubuntu-24.04', 'rust': 'macos-15',
                       'node': 'ubuntu-24.04', 'gitleaks': 'ubuntu-24.04',
                       'physical-db-identity-windows': 'windows-latest',
                       'acceptance': 'ubuntu-24.04'},
            'conformance-linux.yml': {job: 'ubuntu-24.04' for job in
                                     ('change-scope', 'rust-gate', 'linux-platform', 'linux-acceptance')},
        }
        for name, workflow in self.workflows().items():
            jobs = workflow['jobs']
            self.assertEqual({job: data['runs-on'] for job, data in jobs.items()}, expected[name])
            for job, data in jobs.items():
                for step in data['steps']:
                    command = step.get('run', '')
                    for host_only in ('runner_hygiene.sh', 'cargo_gate_step.py', '$HOME/bin/host-build'):
                        self.assertNotIn(host_only, command, (name, job, host_only))

    def test_linux_heavy_invocations_use_isolated_hosted_workspace(self):
        workflow = self.workflows()['conformance-linux.yml']
        rust = workflow['jobs']['rust-gate']
        commands = [line.strip() for step in rust['steps']
                    for line in step.get('run', '').splitlines()]
        # Only the execution seat changes. The full gate commands and audited
        # executable stay required, including portable features and doctests.
        for command in (
            'cargo clippy --workspace --all-targets --locked -- -D warnings',
            '\"${AUDITED_NEXTEST:?}\" nextest run --workspace --locked --profile ci',
            'cargo test -p portable-kernel --features portable-contract-test --locked',
            'cargo test --workspace --locked --doc',
        ):
            self.assertEqual(commands.count(command), 1, command)
        self.assertEqual(rust['env']['CARGO_TARGET_DIR'], '${{ github.workspace }}/target')

    def test_hosted_rust_jobs_bound_parallelism_and_preserve_assertions(self):
        for name, workflow in self.workflows().items():
            rust = workflow['jobs']['rust' if name == 'ci.yml' else 'rust-gate']
            env = rust['env']
            self.assertEqual(env['CARGO_BUILD_JOBS'], '2')
            self.assertEqual(env['NEXTEST_TEST_THREADS'], '4')
            self.assertEqual(env['RUST_TEST_THREADS'], '4')
            self.assertEqual(env['CARGO_PROFILE_DEV_DEBUG'], '0')
            self.assertEqual(env['CARGO_PROFILE_TEST_DEBUG'], '0')
            self.assertNotIn('RUSTFLAGS', env)
            self.assertNotIn('CARGO_PROFILE_TEST_DEBUG_ASSERTIONS', env)
            self.assertNotIn('CARGO_PROFILE_DEV_DEBUG_ASSERTIONS', env)


if __name__ == '__main__':
    unittest.main()
