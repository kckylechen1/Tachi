/**
 * #849 — js-yaml major upgrade gate.
 *
 * Config + MCP YAML must survive parse → dump → re-parse without silent
 * semantic loss. This is the discrimination test required before/with the
 * js-yaml 4 → 5 bump under the #841 dependency-freshness loop.
 */
import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { dump as yamlDump, load as yamlLoad } from 'js-yaml';
import type { Config } from './config.js';
import type { McpConfig } from './mcp.js';

function roundTrip<T>(value: T): T {
  const dumped = yamlDump(value);
  return yamlLoad(dumped) as T;
}

describe('js-yaml roundtrip (#849)', () => {
  it('preserves tachi-cli config.yaml shape', () => {
    const config: Config = {
      daemon: { port: 6919, autoStart: true },
      ui: { language: 'zh', theme: 'dark' },
      paths: { dataDir: '/tmp/tachi-test', projectDb: '/tmp/tachi-test/project.db' },
    };
    const again = roundTrip(config);
    assert.deepEqual(again, config);
  });

  it('preserves mcp-servers.yaml shape including env maps', () => {
    const mcp: McpConfig = {
      servers: [
        {
          id: 'mcp:tachi',
          name: 'tachi',
          command: 'tachi',
          args: ['--mcp'],
          env: { TACHI_HOME: '/tmp/tachi-test', EMPTY: '' },
          enabled: true,
          description: 'primary memory MCP',
        },
        {
          id: 'mcp:secondary',
          name: 'secondary',
          command: 'npx',
          args: ['-y', 'some-mcp'],
          enabled: false,
        },
      ],
    };
    const again = roundTrip(mcp);
    assert.deepEqual(again, mcp);
  });

  it('load → dump → load is stable for mixed scalar types used in MCP env', () => {
    const payload = {
      servers: [
        {
          id: 'mcp:x',
          name: 'x',
          command: 'node',
          args: ['server.js', '--port', '8080'],
          env: {
            FLAG: 'true',
            COUNT: '2',
            PATHISH: '/a/b:/c/d',
          },
          enabled: true,
        },
      ],
    };
    const once = roundTrip(payload);
    const twice = roundTrip(once);
    assert.deepEqual(twice, payload);
  });
});
