import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, test } from 'node:test';

const originalHome = process.env.HOME;
const home = mkdtempSync(join(tmpdir(), 'tachi-config-test-'));
process.env.HOME = home;

const { loadConfig, saveConfig } = await import('./config.js');
const configDir = join(home, '.tachi');
const configPath = join(configDir, 'config.yaml');

function writeConfig(contents: string): void {
  mkdirSync(configDir, { recursive: true });
  writeFileSync(configPath, contents, 'utf8');
}

function removeConfig(): void {
  try {
    unlinkSync(configPath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
  }
}

after(() => {
  if (originalHome === undefined) delete process.env.HOME;
  else process.env.HOME = originalHome;
  rmSync(home, { recursive: true, force: true });
});

test('loads a partial daemon section without dropping daemon defaults', () => {
  writeConfig('daemon:\n  autoStart: false\n');

  const config = loadConfig();

  assert.deepEqual(config.daemon, { port: 6919, autoStart: false });
  assert.deepEqual(config.ui, { language: 'en', theme: 'dark' });
});

test('loads partial ui values and rejects unknown enum values', () => {
  writeConfig('ui:\n  language: zh\n  theme: sepia\n  extra: ignored\n');

  assert.deepEqual(loadConfig().ui, { language: 'zh', theme: 'dark' });
});

test('loads partial paths and preserves optional projectDb', () => {
  writeConfig('paths:\n  projectDb: /tmp/project.db\n');

  assert.deepEqual(loadConfig().paths, {
    dataDir: join(home, '.tachi'),
    projectDb: '/tmp/project.db',
  });
});

test('round-trips a full config through saveConfig and loadConfig', () => {
  const expected = {
    daemon: { port: 12345, autoStart: false },
    ui: { language: 'zh' as const, theme: 'light' as const },
    paths: { dataDir: '/tmp/data', projectDb: '/tmp/project.db' },
  };

  saveConfig(expected);

  assert.deepEqual(loadConfig(), expected);
});

test('returns defaults when the config file is missing', () => {
  removeConfig();

  assert.deepEqual(loadConfig(), {
    daemon: { port: 6919, autoStart: true },
    ui: { language: 'en', theme: 'dark' },
    paths: { dataDir: join(home, '.tachi') },
  });
});

test('returns defaults for a non-object root', () => {
  writeConfig('- daemon\n- ui\n');

  assert.deepEqual(loadConfig(), {
    daemon: { port: 6919, autoStart: true },
    ui: { language: 'en', theme: 'dark' },
    paths: { dataDir: join(home, '.tachi') },
  });
});

test('uses section defaults when a section is null', () => {
  writeConfig('daemon: null\nui: null\npaths: null\n');

  assert.deepEqual(loadConfig(), {
    daemon: { port: 6919, autoStart: true },
    ui: { language: 'en', theme: 'dark' },
    paths: { dataDir: join(home, '.tachi') },
  });
});

test('rejects wrong field types and out-of-range ports without retaining unknown keys', () => {
  writeConfig(`
daemon:
  port: 65536
  autoStart: "false"
  extra: poison
ui:
  language: 1
  theme: blue
paths:
  dataDir: false
  projectDb: 42
unknown: poison
`);

  assert.deepEqual(loadConfig(), {
    daemon: { port: 6919, autoStart: true },
    ui: { language: 'en', theme: 'dark' },
    paths: { dataDir: join(home, '.tachi') },
  });

  writeConfig('daemon:\n  port: 1.5\n');
  assert.equal(loadConfig().daemon.port, 6919);
  writeConfig('daemon:\n  port: 0\n');
  assert.equal(loadConfig().daemon.port, 6919);
});

test('returns independent defaults across calls', () => {
  removeConfig();
  const first = loadConfig();
  first.daemon.port = 1;
  first.paths.dataDir = '/mutated';

  const second = loadConfig();
  first.daemon.port = 6919;
  first.paths.dataDir = join(home, '.tachi');

  assert.equal(second.daemon.port, 6919);
  assert.equal(second.paths.dataDir, join(home, '.tachi'));
  assert.notEqual(first.daemon, second.daemon);
  assert.notEqual(first.paths, second.paths);
});
