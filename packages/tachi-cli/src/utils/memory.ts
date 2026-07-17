import { loadConfig } from './config.js';
import { TACHI_CLI_VERSION } from '../version.js';

const DEFAULT_DAEMON_PORT = 6919;

export interface MemoryEntry {
  id: string;
  summary?: string;
  text?: string;
  category?: string;
  path?: string;
  scope?: string;
  timestamp?: string;
  score?: number;
}

export async function searchMemory(query: string, topK = 10): Promise<MemoryEntry[]> {
  try {
    const config = loadConfig();
    const port = config.daemon.port || DEFAULT_DAEMON_PORT;
    
    // Initialize
    const initResponse = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2024-11-05',
          capabilities: {},
          clientInfo: { name: 'tachi-cli', version: TACHI_CLI_VERSION },
        },
      }),
    });

    if (!initResponse.ok) {
      throw new Error('Daemon not responding');
    }

    const sessionId = initResponse.headers.get('mcp-session-id');

    // Search memory
    const response = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: { 
        'Content-Type': 'application/json',
        'mcp-session-id': sessionId || '',
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 2,
        method: 'tools/call',
        params: {
          name: 'search_memory',
          arguments: {
            query,
            top_k: topK,
            include_archived: false,
            // tachi#1201 k3: search_memory now defaults to a markdown digest
            // when `format` is omitted; this caller JSON.parses the raw text
            // body below, so pin the pre-k3 contract explicitly.
            format: 'json',
          },
        },
      }),
    });

    if (!response.ok) {
      throw new Error('Search failed');
    }

    const data = await response.json() as { result?: { content?: { text?: string }[] } };
    const content = data.result?.content?.[0]?.text;
    
    if (content) {
      const parsed = JSON.parse(content);
      if (Array.isArray(parsed)) {
        return parsed.map((item: MemoryEntry) => ({
          ...item,
        }));
      }
    }

    return [];
  } catch (error) {
    console.error('Failed to search memory:', error);
    return [];
  }
}

export async function listMemories(pathPrefix?: string, limit = 50): Promise<MemoryEntry[]> {
  try {
    const config = loadConfig();
    const port = config.daemon.port || DEFAULT_DAEMON_PORT;
    
    // Initialize
    const initResponse = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2024-11-05',
          capabilities: {},
          clientInfo: { name: 'tachi-cli', version: TACHI_CLI_VERSION },
        },
      }),
    });

    if (!initResponse.ok) {
      throw new Error('Daemon not responding');
    }

    const sessionId = initResponse.headers.get('mcp-session-id');

    // List memories
    const response = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: { 
        'Content-Type': 'application/json',
        'mcp-session-id': sessionId || '',
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 2,
        method: 'tools/call',
        params: {
          name: 'list_memories',
          arguments: {
            path_prefix: pathPrefix,
            limit,
            include_archived: false,
          },
        },
      }),
    });

    if (!response.ok) {
      throw new Error('Failed to list memories');
    }

    const data = await response.json() as { result?: { content?: { text?: string }[] } };
    const content = data.result?.content?.[0]?.text;
    
    if (content) {
      const parsed = JSON.parse(content);
      if (Array.isArray(parsed)) {
        return parsed.map((item: MemoryEntry) => ({
          ...item,
        }));
      }
    }

    return [];
  } catch (error) {
    console.error('Failed to list memories:', error);
    return [];
  }
}
