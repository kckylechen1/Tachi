"""Run the legacy registrar against a fake MCP and disposable client config."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
FAKE_MCP = r'''#!/usr/bin/env python3
import json, os, sys, time
mode = os.environ['FAKE_MODE']
if mode.startswith('hidden_'):
    with open(os.environ['FAKE_PROJECT_STATE'], 'w') as f:
        json.dump({'id': 'mcp:context7', 'enabled': True}, f)
with open(os.environ['FAKE_START'], 'w') as f:
    f.write(os.environ.get('TACHI_PROFILE', ''))
for line in sys.stdin:
    q = json.loads(line)
    if 'id' not in q:
        continue
    method = q['method']
    name = q.get('params', {}).get('name')
    args = q.get('params', {}).get('arguments', {})
    with open(os.environ['FAKE_REQUESTS'], 'a') as f:
        f.write(json.dumps(q) + '\n')
    if mode == 'partial_reply':
        sys.stdout.write('{'); sys.stdout.flush(); time.sleep(5); continue
    if method == 'initialize':
        result = {'protocolVersion': '2024-11-05', 'capabilities': {}}
    elif method == 'tools/list':
        names = ['hub_register', 'hub_discover', 'hub_set_enabled']
        if mode == 'standard': names = ['tachi_memory']
        if mode == 'hidden_no_set_tool': names.remove('hub_set_enabled')
        result = {'tools': [{'name': n} for n in names]}
    else:
        if name == 'hub_register':
            data = {'id': args['id'], 'db': 'global', 'enabled': True, 'callable': True}
            if mode == 'pending' or mode in ('hidden_shadow', 'hidden_no_set_tool'):
                data.update(enabled=False, callable=False, review_status='pending')
            if mode == 'hidden_missing_disabled': data.pop('enabled')
            if mode == 'hidden_nonboolean_disabled': data['enabled'] = 0
            if mode == 'registration_mismatch': data['id'] = 'mcp:other'
            if mode == 'registration_error':
                print(json.dumps({'jsonrpc': '2.0', 'id': q['id'], 'error': {'code': -1, 'message': 'blocked'}}), flush=True); continue
        elif name == 'hub_discover':
            data = [{'id': 'mcp:' + key, 'cap_type': 'mcp', 'db': 'global',
                     'callable': True, 'enabled': True, 'review_status': 'approved'}
                    for key in ('context7', 'exa', 'longbridge', 'unrelated')]
            if mode == 'discover_error': data = {'error': 'discovery failed'}
            if mode == 'empty': data = []
            if mode == 'nonlist': data = {}
            if mode == 'malformed_item': data[0] = None
            if mode == 'duplicate': data.append(data[0])
            if mode == 'missing_id': data.pop(0)
            if mode == 'pending': data[0]['callable'] = False
            if mode.startswith('missing_'): data[0].pop(mode.removeprefix('missing_'), None)
            if mode == 'project_shadow': data[0]['db'] = 'project'
            if mode == 'false_enabled': data[0]['enabled'] = False
            if mode == 'integer_callable': data[0]['callable'] = 1
            if mode == 'unapproved': data[0]['review_status'] = 'pending'
        else:
            data = {'enabled': False, 'db': 'project'}
            if name == 'hub_set_enabled' and mode.startswith('hidden_'):
                with open(os.environ['FAKE_PROJECT_STATE'], 'w') as f:
                    json.dump({'id': args['id'], 'enabled': False}, f)
        result = {'content': [{'type': 'text', 'text': json.dumps(data)}]}
        if mode == 'tool_error' and name == 'hub_discover': result['isError'] = True
        if mode == 'bad_content' and name == 'hub_discover': result['content'] = []
        if mode == 'invalid_json' and name == 'hub_discover': result['content'][0]['text'] = '!'
    print(json.dumps({'jsonrpc': '2.0', 'id': q['id'] + (1 if mode == 'wrong_reply' else 0), 'result': result}), flush=True)
'''


class RegistrarTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.bin = self.root / 'bin'
        self.scripts = self.root / 'scripts'
        self.bin.mkdir(); self.scripts.mkdir()
        self.config = self.home / '.cursor/mcp.json'
        self.config.parent.mkdir(parents=True)
        self.original = json.dumps({'mcpServers': {k: {'command': k} for k in
                                    ['context7', 'exa', 'longbridge', 'unrelated']}})
        self.config.write_text(self.original)
        for name in ('register_mcps_to_hub.py', 'setup_agent_mcp.py'):
            shutil.copyfile(ROOT / 'scripts' / name, self.scripts / name)
        fake = self.bin / 'tachi'
        fake.write_text(FAKE_MCP)
        fake.chmod(0o755)

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, mode='ready', *args, profile='admin', extra_env=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith('TACHI_')}
        env.update(HOME=str(self.home), PATH=str(self.bin) + os.pathsep + env['PATH'],
                   FAKE_MODE=mode, FAKE_START=str(self.root / 'started'),
                   FAKE_REQUESTS=str(self.root / 'requests'),
                   FAKE_PROJECT_STATE=str(self.root / 'project-state'),
                   TACHI_REQUEST_TIMEOUT_SECS='0.3' if mode == 'partial_reply' else '5')
        env.update(extra_env or {})
        if profile is not None: env['TACHI_PROFILE'] = profile
        return subprocess.run([sys.executable, str(self.scripts / 'register_mcps_to_hub.py'),
                               '--agent', 'cursor', *args], env=env, capture_output=True, text=True, timeout=10)

    def assert_preserved(self):
        self.assertEqual(self.config.read_text(), self.original)
        self.assertEqual(list(self.config.parent.glob('*.bak.*')), [])

    def test_unset_profile_refuses_before_starting(self):
        result = self.run_script(profile=None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Explicit operator', result.stderr)
        self.assertFalse((self.root / 'started').exists())
        self.assert_preserved()

    def test_wrong_surface_refuses_without_elevation(self):
        result = self.run_script('standard', profile='standard')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / 'started').read_text(), 'standard')
        self.assert_preserved()

    def test_failures_never_modify_client_config(self):
        for mode in ('registration_error', 'registration_mismatch', 'discover_error',
                     'empty', 'nonlist', 'malformed_item', 'duplicate', 'missing_id',
                     'missing_callable', 'missing_enabled', 'missing_review_status',
                     'missing_cap_type', 'missing_db', 'project_shadow', 'false_enabled',
                     'integer_callable', 'unapproved', 'pending', 'tool_error',
                     'bad_content', 'invalid_json', 'wrong_reply', 'partial_reply'):
            with self.subTest(mode=mode):
                result = self.run_script(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assert_preserved()

    def test_pending_registration_without_sync_is_successful(self):
        result = self.run_script('pending', '--no-sync-agent-config')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('review/enablement is separate', result.stdout)
        self.assert_preserved()

    def test_disable_hidden_uses_scoped_registration_without_touching_project_shadow(self):
        for mode in ('hidden_shadow', 'hidden_no_set_tool'):
            with self.subTest(mode=mode):
                (self.root / 'requests').unlink(missing_ok=True)
                result = self.run_script(mode, '--disable-hidden', '--no-sync-agent-config',
                                         extra_env={'TACHI_CONTEXT7_CAP_VISIBILITY': 'hidden'})
                self.assertEqual(result.returncode, 0, result.stderr)
                requests = [json.loads(line) for line in (self.root / 'requests').read_text().splitlines()]
                names = [q.get('params', {}).get('name') for q in requests]
                self.assertNotIn('hub_set_enabled', names)
                registrations = [q['params']['arguments'] for q in requests
                                 if q.get('params', {}).get('name') == 'hub_register']
                self.assertEqual([q['id'] for q in registrations],
                                 ['mcp:context7', 'mcp:exa', 'mcp:longbridge'])
                self.assertTrue(all(q['scope'] == 'global' for q in registrations))
                self.assertEqual(json.loads((self.root / 'project-state').read_text()),
                                 {'id': 'mcp:context7', 'enabled': True})
                self.assertIn('hidden registration confirmed disabled', result.stdout)
                self.assert_preserved()

    def test_disable_hidden_missing_or_false_shaped_evidence_stops_without_unscoped_write(self):
        for mode in ('hidden_enabled', 'hidden_missing_disabled', 'hidden_nonboolean_disabled'):
            with self.subTest(mode=mode):
                (self.root / 'requests').unlink(missing_ok=True)
                result = self.run_script(mode, '--disable-hidden', '--no-sync-agent-config',
                                         extra_env={'TACHI_CONTEXT7_CAP_VISIBILITY': 'hidden'})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Disabled registration not confirmed', result.stderr)
                requests = [json.loads(line) for line in (self.root / 'requests').read_text().splitlines()]
                names = [q.get('params', {}).get('name') for q in requests]
                self.assertNotIn('hub_set_enabled', names)
                self.assertEqual(names.count('hub_register'), 1)
                self.assertEqual(json.loads((self.root / 'project-state').read_text()),
                                 {'id': 'mcp:context7', 'enabled': True})
                self.assert_preserved()

    def test_sync_only_does_not_reregister_and_preserves_direct_entries(self):
        result = self.run_script('ready', '--sync-only', profile='ops')
        self.assertEqual(result.returncode, 0, result.stderr)
        requests = [json.loads(line) for line in (self.root / 'requests').read_text().splitlines()]
        self.assertNotIn('hub_register', [q.get('params', {}).get('name') for q in requests])
        config = json.loads(self.config.read_text())['mcpServers']
        self.assertEqual(set(config), {'tachi', 'context7', 'exa', 'longbridge', 'unrelated'})
        for key, value in json.loads(self.original)['mcpServers'].items():
            self.assertEqual(config.get(key), value)
        self.assertEqual(config['tachi']['env']['TACHI_PROFILE'], 'standard')
        self.assertEqual(len(list(self.config.parent.glob('*.bak.*'))), 1)

    def test_registration_and_positive_discovery_permit_sync(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(self.config.read_text())['mcpServers']
        self.assertIn('tachi', config)
        for key, value in json.loads(self.original)['mcpServers'].items():
            self.assertEqual(config.get(key), value)

    def test_project_evidence_cannot_migrate_global_client_config(self):
        result = self.run_script('ready', '--scope', 'project')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'started').exists())
        self.assert_preserved()

    def test_sync_only_cannot_be_combined_with_no_sync(self):
        result = self.run_script('ready', '--sync-only', '--no-sync-agent-config')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'started').exists())
        self.assert_preserved()


if __name__ == '__main__':
    unittest.main()
