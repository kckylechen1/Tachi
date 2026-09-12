import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { callTachiTool } from './tachiMcp.js';

interface SeenRequest {
  method: string;
  session?: string;
  message?: Record<string, unknown>;
}

const seen: SeenRequest[] = [];
let mode: 'normal' | 'protocol-error' | 'tool-error' | 'http-error' | 'malformed' = 'normal';
let termination: 'normal' | 'error' | 'hang' = 'normal';
let hangInitialize = false;
let port = 0;
let home = '';

function sendJson(response: ServerResponse, status: number, value?: unknown, session?: string) {
  response.statusCode = status;
  if (session) response.setHeader('mcp-session-id', session);
  if (value === undefined) return response.end();
  response.setHeader('content-type', 'application/json');
  response.end(JSON.stringify(value));
}

function toolPayload(name: string): unknown {
  switch (name) {
    case 'tachi_memory': return {
      status: 'completed', query: 'needle',
      sections: [{ name: 'memory', rows: [{ id: 'memory-1', summary: 'found' }] }],
    };
    case 'list_memories': return [{ id: 'memory-2', path: '/notes' }];
    case 'hub_set_enabled': return { id: 'skill-1', enabled: true };
    case 'hub_discover': return [
      { id: 'skill-1', name: 'Skill', cap_type: 'skill', enabled: true },
      { id: 'mcp:test', cap_type: 'mcp', enabled: true,
        definition: JSON.stringify({ tools: [{ name: 'remote_tool', description: 'Remote' }] }) },
    ];
    default: return [];
  }
}

async function handler(request: IncomingMessage, response: ServerResponse) {
  assert.equal(request.url, '/mcp');
  if (mode === 'http-error') return sendJson(response, 503, { error: 'offline' });
  if (request.method === 'DELETE') {
    seen.push({ method: 'DELETE', session: request.headers['mcp-session-id'] as string | undefined });
    if (termination === 'hang') return;
    if (termination === 'error') return sendJson(response, 500, { error: 'cleanup failed' });
    return sendJson(response, 200);
  }
  if (request.method === 'GET') return sendJson(response, 405);
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.from(chunk));
  if (chunks.length === 0) return sendJson(response, 400);
  const message = JSON.parse(Buffer.concat(chunks).toString()) as Record<string, unknown>;
  const method = String(message.method);
  const session = request.headers['mcp-session-id'] as string | undefined;
  seen.push({ method, session, message });

  if (method === 'initialize') {
    if (hangInitialize) return;
    return sendJson(response, 200, {
      jsonrpc: '2.0', id: message.id,
      result: { protocolVersion: '2025-06-18', capabilities: {}, serverInfo: { name: 'fixture', version: '1' } },
    }, 'fixture-session');
  }
  if (method === 'notifications/initialized') return sendJson(response, 202);
  if (method === 'tools/call') {
    const params = message.params as { name: string };
    if (mode === 'protocol-error') {
      return sendJson(response, 200, { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'tool not found' } });
    }
    const isError = mode === 'tool-error';
    const text = mode === 'malformed' ? 'not-json' : isError ? 'permission denied: admin only' : JSON.stringify(toolPayload(params.name));
    const payload = { jsonrpc: '2.0', id: message.id, result: { isError, content: [{ type: 'text', text }] } };
    if (params.name === 'sse_tool') {
      response.statusCode = 200;
      response.setHeader('content-type', 'text/event-stream');
      return response.end(`event: message\ndata: ${JSON.stringify(payload)}\n\n`);
    }
    return sendJson(response, 200, payload);
  }
  return sendJson(response, 404);
}

const server = createServer((request, response) => void handler(request, response));

before(async () => {
  home = await mkdtemp(join(tmpdir(), 'tachi-cli-test-'));
  process.env.HOME = home;
  await new Promise<void>((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const address = server.address();
  assert(address && typeof address === 'object');
  port = address.port;
  await mkdir(join(home, '.tachi'));
  await writeFile(join(home, '.tachi', 'config.yaml'), `daemon:\n  port: ${port}\n`);
});

after(async () => {
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  await rm(home, { recursive: true, force: true });
});

test('SDK transport performs initialize, initialized, session-bound SSE call, and termination', async () => {
  seen.length = 0;
  mode = 'normal';
  assert.deepEqual(await callTachiTool(port, 'sse_tool', {}, { endpoint: new URL(`http://127.0.0.1:${port}/mcp`) }), []);
  assert.deepEqual(seen.map(item => item.method), ['initialize', 'notifications/initialized', 'tools/call', 'DELETE']);
  assert.equal(seen[0].session, undefined);
  assert.equal(seen[1].session, 'fixture-session');
  assert.equal(seen[2].session, 'fixture-session');
  assert.equal(seen[3].session, 'fixture-session');
});

test('protocol, tool, HTTP, and malformed response failures remain useful', async () => {
  const endpoint = new URL(`http://127.0.0.1:${port}/mcp`);
  mode = 'protocol-error';
  await assert.rejects(callTachiTool(port, 'missing', {}, { endpoint }), /tool not found/);
  mode = 'tool-error';
  await assert.rejects(callTachiTool(port, 'hub_set_enabled', {}, { endpoint }), /permission denied: admin only/);
  mode = 'malformed';
  await assert.rejects(callTachiTool(port, 'bad', {}, { endpoint }), /malformed JSON/);
  mode = 'http-error';
  await assert.rejects(callTachiTool(port, 'bad', {}, { endpoint, timeoutMs: 500 }), /503|HTTP|initialize/i);
  mode = 'normal';
});

test('cleanup failure preserves successful mutations and primary tool errors', async () => {
  termination = 'error';
  try {
    assert.deepEqual(await callTachiTool(port, 'hub_set_enabled', { id: 'skill-1', enabled: true }),
      { id: 'skill-1', enabled: true });
    mode = 'tool-error';
    await assert.rejects(callTachiTool(port, 'hub_set_enabled', {}), /permission denied: admin only/);
  } finally {
    termination = 'normal';
    mode = 'normal';
  }
});

test('hung initialization and termination have bounded waits', async () => {
  hangInitialize = true;
  try {
    await assert.rejects(callTachiTool(port, 'unused', {}, { timeoutMs: 50 }), /initialization.*timed out/);
    hangInitialize = false;
    termination = 'hang';
    assert.deepEqual(await callTachiTool(port, 'sse_tool', {}), []);
  } finally {
    hangInitialize = false;
    termination = 'normal';
    server.closeAllConnections();
  }
});

test('CLI utilities use current tool names, arguments, and result shapes', async () => {
  seen.length = 0;
  mode = 'normal';
  const { searchMemory, listMemories } = await import('./memory.js');
  const { getSkills, toggleSkill } = await import('./skills.js');
  const { discoverMcpTools } = await import('./mcpDiscovery.js');

  assert.equal((await searchMemory('needle', 7))[0].id, 'memory-1');
  assert.equal((await listMemories('/notes', 3))[0].id, 'memory-2');
  assert.equal((await getSkills())[0].id, 'skill-1');
  await toggleSkill('skill-1', true);
  assert.deepEqual(await discoverMcpTools('mcp:test'), [{ name: 'remote_tool', description: 'Remote' }]);

  const calls = seen.filter(item => item.method === 'tools/call')
    .map(item => (item.message?.params as { name: string; arguments: Record<string, unknown> }));
  assert.deepEqual(calls.map(call => call.name), [
    'tachi_memory', 'list_memories', 'hub_discover', 'hub_set_enabled', 'hub_discover',
  ]);
  assert.deepEqual(calls[0].arguments, {
    action: 'search', query: 'needle', top_k: 7, include_archived: false, format: 'json',
  });
  assert.deepEqual(calls[3].arguments, { id: 'skill-1', enabled: true });
  assert.deepEqual(calls[4].arguments, { cap_type: 'mcp', enabled_only: true });
});
