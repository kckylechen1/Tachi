import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// Tests the load-bearing security contract of vaultSet: the secret VALUE must
// never appear in argv, and value/password must be delivered over stdin in the
// binary's documented read order (password line first, then value line).
//
// We point TACHI_BINARY at a fake script that dumps its argv + stdin to files,
// so we can assert on exactly what `vaultSet` would have spawned. Run against
// the compiled dist build: `node --test dist/utils/vault.test.js`.

async function loadVaultSet() {
  // Imported lazily so TACHI_BINARY is read at spawn time, not import time.
  const mod = await import('./vault.js');
  return mod.vaultSet;
}

function makeFakeBinary(dir: string, exitCode: number): { bin: string; argvFile: string; stdinFile: string } {
  const argvFile = join(dir, 'argv.json');
  const stdinFile = join(dir, 'stdin.txt');
  const bin = join(dir, 'fake-tachi.mjs');
  // Node script: record argv (minus node + self) and all of stdin, then exit.
  const script = `#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
const args = process.argv.slice(2);
let data = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (c) => { data += c; });
process.stdin.on('end', () => {
  writeFileSync(${JSON.stringify(argvFile)}, JSON.stringify(args));
  writeFileSync(${JSON.stringify(stdinFile)}, data);
  if (${exitCode} !== 0) { process.stderr.write('boom\\n'); }
  process.exit(${exitCode});
});
`;
  writeFileSync(bin, script);
  chmodSync(bin, 0o755);
  return { bin, argvFile, stdinFile };
}

test('vaultSet keeps the secret value off argv and pipes password+value on stdin', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'tachi-vault-test-'));
  const { bin, argvFile, stdinFile } = makeFakeBinary(dir, 0);
  process.env.TACHI_BINARY = bin;

  const vaultSet = await loadVaultSet();
  const secret = 'super-secret-value-123';
  const password = 'master-pw-456';
  const res = await vaultSet('VOYAGE_API_KEY', secret, { password });

  assert.equal(res.success, true, res.error);

  const argv = JSON.parse(readFileSync(argvFile, 'utf8')) as string[];
  // Contract: name is on argv, flags are present, secret/password are NOT.
  assert.deepEqual(argv.slice(0, 3), ['vault', 'set', 'VOYAGE_API_KEY']);
  assert.ok(argv.includes('--value-stdin'), 'missing --value-stdin');
  assert.ok(argv.includes('--stdin-password'), 'missing --stdin-password');
  assert.ok(!argv.includes(secret), 'secret value leaked into argv');
  assert.ok(!argv.includes(password), 'password leaked into argv');

  // Contract: password line first, then value line (binary reads in that order).
  const stdin = readFileSync(stdinFile, 'utf8');
  assert.equal(stdin, `${password}\n${secret}\n`);
});

test('vaultSet without a password omits --stdin-password and pipes only the value', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'tachi-vault-test-'));
  const { bin, argvFile, stdinFile } = makeFakeBinary(dir, 0);
  process.env.TACHI_BINARY = bin;

  const vaultSet = await loadVaultSet();
  const secret = 'value-only-789';
  const res = await vaultSet('SILICONFLOW_API_KEY', secret, {});

  assert.equal(res.success, true, res.error);

  const argv = JSON.parse(readFileSync(argvFile, 'utf8')) as string[];
  assert.ok(argv.includes('--value-stdin'));
  assert.ok(!argv.includes('--stdin-password'), 'unexpected --stdin-password');
  assert.ok(!argv.includes(secret), 'secret value leaked into argv');

  const stdin = readFileSync(stdinFile, 'utf8');
  assert.equal(stdin, `${secret}\n`);
});

test('vaultSet surfaces a non-zero exit as a failure result', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'tachi-vault-test-'));
  const { bin } = makeFakeBinary(dir, 7);
  process.env.TACHI_BINARY = bin;

  const vaultSet = await loadVaultSet();
  const res = await vaultSet('VOYAGE_API_KEY', 'whatever', { password: 'pw' });

  assert.equal(res.success, false);
  assert.ok(res.error && res.error.length > 0, 'expected an error message');
});
