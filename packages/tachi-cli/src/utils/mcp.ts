import { existsSync, readFileSync, writeFileSync } from 'fs';
import { join } from 'path';
import { dump as yamlDump, load as yamlLoad } from 'js-yaml';
import { getDataDir } from './config.js';

const MCP_CONFIG_FILE = 'mcp-servers.yaml';

export interface McpServer {
  id: string;
  name: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
  enabled: boolean;
  description?: string;
}

export interface McpConfig {
  servers: McpServer[];
}

export function loadMcpConfig(): McpConfig {
  try {
    const dataDir = getDataDir();
    const configPath = join(dataDir, MCP_CONFIG_FILE);
    
    if (existsSync(configPath)) {
      const content = readFileSync(configPath, 'utf-8');
      const parsed = yamlLoad(content) as Partial<McpConfig>;
      return { servers: [], ...parsed };
    }
  } catch (error) {
    console.error('Failed to load MCP config:', error);
  }
  
  return { servers: [] };
}

export function saveMcpConfig(config: McpConfig): void {
  try {
    const dataDir = getDataDir();
    const configPath = join(dataDir, MCP_CONFIG_FILE);
    const yamlContent = yamlDump(config);
    writeFileSync(configPath, yamlContent, 'utf-8');
  } catch (error) {
    console.error('Failed to save MCP config:', error);
    throw error;
  }
}

export function addMcpServer(server: Omit<McpServer, 'id'>): McpServer {
  const config = loadMcpConfig();
  const id = `mcp:${server.name.toLowerCase().replace(/\s+/g, '-')}`;
  
  // Check if already exists
  if (config.servers.some(s => s.id === id)) {
    throw new Error(`MCP server "${server.name}" already exists`);
  }
  
  const newServer: McpServer = {
    ...server,
    id,
  };
  
  config.servers.push(newServer);
  saveMcpConfig(config);
  
  return newServer;
}

export function removeMcpServer(id: string): void {
  const config = loadMcpConfig();
  config.servers = config.servers.filter(s => s.id !== id);
  saveMcpConfig(config);
}

export function toggleMcpServer(id: string, enabled: boolean): void {
  const config = loadMcpConfig();
  const server = config.servers.find(s => s.id === id);
  if (server) {
    server.enabled = enabled;
    saveMcpConfig(config);
  }
}

export function getMcpServers(): McpServer[] {
  return loadMcpConfig().servers;
}
