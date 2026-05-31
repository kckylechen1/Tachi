import { loadConfig } from './config.js';
import { TACHI_CLI_VERSION } from '../version.js';

const DEFAULT_DAEMON_PORT = 6919;

export interface Skill {
  id: string;
  name: string;
  cap_type: string;
  description?: string;
  enabled: boolean;
  version?: number;
  uses?: number;
  successes?: number;
  failures?: number;
}

export async function getSkills(): Promise<Skill[]> {
  try {
    const config = loadConfig();
    const port = config.daemon.port || DEFAULT_DAEMON_PORT;
    
    const response = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
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

    if (!response.ok) {
      throw new Error('Daemon not responding');
    }

    // Initialize session
    const sessionId = response.headers.get('mcp-session-id');
    
    // Call hub_discover to get all capabilities
    const discoverResponse = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
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
          name: 'hub_discover',
          arguments: { enabled_only: false },
        },
      }),
    });

    if (!discoverResponse.ok) {
      throw new Error('Failed to fetch skills');
    }

    const data = await discoverResponse.json() as { result?: { content?: { text?: string }[] } };
    const content = data.result?.content?.[0]?.text;
    
    if (content) {
      const parsed = JSON.parse(content);
      if (Array.isArray(parsed)) {
        return parsed
          .filter((item: { cap_type?: string }) => item.cap_type === 'skill')
          .map((item: Skill) => ({
            ...item,
            enabled: item.enabled !== false,
          }));
      }
    }

    return [];
  } catch (error) {
    console.error('Failed to fetch skills:', error);
    return [];
  }
}

export async function toggleSkill(id: string, enabled: boolean): Promise<void> {
  try {
    const config = loadConfig();
    const port = config.daemon.port || DEFAULT_DAEMON_PORT;
    
    // First initialize
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

    // Call hub_disable or hub_enable
    const action = enabled ? 'hub_enable' : 'hub_disable';
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
          name: action,
          arguments: { id },
        },
      }),
    });

    if (!response.ok) {
      throw new Error(`Failed to ${enabled ? 'enable' : 'disable'} skill`);
    }
  } catch (error) {
    console.error(`Failed to toggle skill ${id}:`, error);
    throw error;
  }
}
