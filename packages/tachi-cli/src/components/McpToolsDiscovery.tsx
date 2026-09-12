import React, { useState, useEffect } from 'react';
import { Box, Text, useInput } from 'ink';
import { colors } from '../utils/ui.js';
import { getMcpServers, type McpServer } from '../utils/mcp.js';
import { discoverMcpTools, type DiscoveredTool } from '../utils/mcpDiscovery.js';

interface McpToolsDiscoveryProps {
  onBack: () => void;
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
      setTools(await discoverMcpTools(serverId));
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
