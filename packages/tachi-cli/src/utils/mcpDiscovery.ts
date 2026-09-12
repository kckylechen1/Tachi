import { loadConfig } from './config.js';
import { callTachiTool, requireArray } from './tachiMcp.js';

const DEFAULT_DAEMON_PORT = 6919;

export interface DiscoveredTool {
  name: string;
  description?: string;
}

interface McpCapability {
  id?: string;
  definition?: string;
}

export async function discoverMcpTools(serverId: string): Promise<DiscoveredTool[]> {
  const port = loadConfig().daemon.port || DEFAULT_DAEMON_PORT;
  const capabilities = requireArray<McpCapability>(
    await callTachiTool(port, 'hub_discover', { cap_type: 'mcp', enabled_only: true }),
    'hub_discover',
  );
  const server = capabilities.find(capability => capability.id === serverId);
  if (!server?.definition) throw new Error('Server not found or not enabled');

  let definition: unknown;
  try {
    definition = JSON.parse(server.definition);
  } catch (error) {
    throw new Error(`Server definition is malformed JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  const tools = (definition as { tools?: unknown }).tools;
  if (!Array.isArray(tools)) throw new Error('No tools discovered. Server may need reconnect.');
  return tools.map(tool => {
    if (typeof tool !== 'object' || tool === null) {
      throw new Error('hub_discover returned a malformed tool definition');
    }
    const candidate = tool as { name?: unknown; description?: unknown };
    return {
      name: typeof candidate.name === 'string' ? candidate.name : 'unknown',
      ...(typeof candidate.description === 'string' ? { description: candidate.description } : {}),
    };
  });
}
