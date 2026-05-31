import React, { useState, useEffect } from 'react';
import { Box, Text, useInput } from 'ink';
import { t } from '../utils/i18n.js';
import { colors } from '../utils/ui.js';
import { getMcpServers, loadMcpConfig, type McpServer } from '../utils/mcp.js';
import { loadConfig } from '../utils/config.js';
import { TACHI_CLI_VERSION } from '../version.js';

interface McpToolsDiscoveryProps {
  onBack: () => void;
}

interface DiscoveredTool {
  name: string;
  description?: string;
}

export function McpToolsDiscovery({ onBack }: McpToolsDiscoveryProps) {
  const [servers, setServers] = useState<McpServer[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [tools, setTools] = useState<DiscoveredTool[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [selectedServer, setSelectedServer] = useState<string | null>(null);

  useEffect(() => {
    const serverList = getMcpServers();
    setServers(serverList);
  }, []);

  const discoverTools = async (serverId: string) => {
    setLoading(true);
    setError('');
    setSelectedServer(serverId);
    
    try {
      const config = loadConfig();
      const port = config.daemon.port || 6919;
      
      // Initialize session
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

      // Call hub_discover for this server
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
            name: 'hub_discover',
            arguments: {
              cap_type: 'mcp',
              enabled_only: true,
            },
          },
        }),
      });

      if (!response.ok) {
        throw new Error('Failed to discover tools');
      }

      const data = await response.json() as { result?: { content?: { text?: string }[] } };
      const content = data.result?.content?.[0]?.text;
      
      if (content) {
        const parsed = JSON.parse(content);
        if (Array.isArray(parsed)) {
          const server = parsed.find((s: { id?: string }) => s.id === serverId);
          if (server?.definition) {
            const def = JSON.parse(server.definition);
            if (def.tools) {
              setTools(def.tools.map((t: { name?: string; description?: string }) => ({
                name: t.name || 'unknown',
                description: t.description,
              })));
            } else {
              setError('No tools discovered. Server may need reconnect.');
            }
          } else {
            setError('Server not found or not enabled');
          }
        }
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Discovery failed');
    }
    
    setLoading(false);
  };

  useInput((input, key) => {
    if (tools.length > 0) {
      // Tool list view
      if (input === 'q' || key.escape) {
        setTools([]);
        setSelectedServer(null);
      } else if (key.upArrow) {
        setSelectedIndex(prev => (prev > 0 ? prev - 1 : tools.length - 1));
      } else if (key.downArrow) {
        setSelectedIndex(prev => (prev < tools.length - 1 ? prev + 1 : 0));
      }
    } else {
      // Server list view
      if (key.upArrow) {
        setSelectedIndex(prev => (prev > 0 ? prev - 1 : servers.length - 1));
      } else if (key.downArrow) {
        setSelectedIndex(prev => (prev < servers.length - 1 ? prev + 1 : 0));
      } else if (key.return && servers.length > 0) {
        discoverTools(servers[selectedIndex].id);
      } else if (input === 'q' || key.escape) {
        onBack();
      }
    }
  });

  if (tools.length > 0) {
    return (
      <Box flexDirection="column" padding={1}>
        <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
          <Text bold>Tools for {selectedServer}</Text>
          
          <Box flexDirection="column" marginTop={1}>
            {tools.map((tool, index) => (
              <Box key={tool.name} flexDirection="column">
                <Text>
                  {index === selectedIndex ? colors.primary('❯ ') : '  '}
                  {tool.name}
                </Text>
                {tool.description && index === selectedIndex && (
                  <Text dimColor>    {tool.description}</Text>
                )}
              </Box>
            ))}
          </Box>

          <Box marginTop={1}>
            <Text dimColor>Use these exact tool names in hub_call</Text>
          </Box>
        </Box>

        <Box marginTop={1}>
          <Text dimColor>↑↓ Navigate | q - Back</Text>
        </Box>
      </Box>
    );
  }

  return (
    <Box flexDirection="column" padding={1}>
      <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
        <Text bold>Discover MCP Tools</Text>
        
        {error && (
          <Box marginTop={1}>
            <Text>{colors.error(`✗ ${error}`)}</Text>
          </Box>
        )}

        {loading ? (
          <Box marginTop={1}>
            <Text dimColor>Discovering tools...</Text>
          </Box>
        ) : servers.length === 0 ? (
          <Box marginTop={1}>
            <Text dimColor>No MCP servers configured.</Text>
          </Box>
        ) : (
          <Box flexDirection="column" marginTop={1}>
            <Text dimColor>Select a server to discover tools:</Text>
            {servers.map((server, index) => (
              <Text key={server.id}>
                {index === selectedIndex ? colors.primary('❯ ') : '  '}
                {server.name} {server.enabled ? colors.success('●') : colors.error('○')}
              </Text>
            ))}
          </Box>
        )}
      </Box>

      <Box marginTop={1}>
        <Text dimColor>↑↓ Navigate | Enter - Discover | q - Back</Text>
      </Box>
    </Box>
  );
}
