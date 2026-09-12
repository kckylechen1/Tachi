import React, { useState, useEffect } from 'react';
import { Box, Text, useInput } from 'ink';
import { t } from '../utils/i18n.js';
import { colors } from '../utils/ui.js';
import { searchMemory, listMemories, type MemoryEntry } from '../utils/memory.js';

interface MemoryManagerProps {
  onBack: () => void;
}

type ViewMode = 'menu' | 'search' | 'list';

export function MemoryManager({ onBack }: MemoryManagerProps) {
  const [viewMode, setViewMode] = useState<ViewMode>('menu');
  const [memories, setMemories] = useState<MemoryEntry[]>([]);
  const [query, setQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  const handleSearch = async () => {
    if (!query.trim()) return;
    setLoading(true);
    setError('');
    try {
      const results = await searchMemory(query, 10);
      setMemories(results);
      setViewMode('list');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Search failed');
    }
    setLoading(false);
  };

  const handleListAll = async () => {
    setLoading(true);
    setError('');
    try {
      const results = await listMemories(undefined, 20);
      setMemories(results);
      setViewMode('list');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to list memories');
    }
    setLoading(false);
  };

  const handleListKanban = async () => {
    setLoading(true);
    setError('');
    try {
      const results = await listMemories('/kanban', 20);
      setMemories(results);
      setViewMode('list');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to list kanban');
    }
    setLoading(false);
  };

  useInput((input, key) => {
    if (viewMode === 'menu') {
      if (input === 's') {
        setViewMode('search');
      } else if (input === 'a') {
        handleListAll();
      } else if (input === 'k') {
        handleListKanban();
      } else if (input === 'q' || key.escape) {
        onBack();
      }
    } else if (viewMode === 'search') {
      if (key.return) {
        handleSearch();
      } else if (key.backspace || key.delete) {
        setQuery(prev => prev.slice(0, -1));
      } else if (input) {
        setQuery(prev => prev + input);
      } else if (key.escape) {
        setViewMode('menu');
      }
    } else if (viewMode === 'list') {
      if (key.upArrow) {
        setSelectedIndex(prev => (prev > 0 ? prev - 1 : memories.length - 1));
      } else if (key.downArrow) {
        setSelectedIndex(prev => (prev < memories.length - 1 ? prev + 1 : 0));
      } else if (input === 'q' || key.escape) {
        setViewMode('menu');
      }
    }
  });

  if (viewMode === 'search') {
    return (
      <Box flexDirection="column" padding={1}>
        <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
          <Text bold>Search Memory</Text>
          
          <Box marginTop={1}>
            <Text>
              Query: {query}
              <Text color="cyan">▌</Text>
            </Text>
          </Box>

          {loading && <Text dimColor>Searching...</Text>}

          <Box flexDirection="column" marginTop={2}>
            <Text dimColor>Enter - Search | Esc - Back</Text>
          </Box>
        </Box>
      </Box>
    );
  }

  if (viewMode === 'list') {
    return (
      <Box flexDirection="column" padding={1}>
        <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
          <Text bold>{memories.length} Memories Found</Text>
          
          {error && (
            <Box marginTop={1}>
              <Text>{colors.error(`✗ ${error}`)}</Text>
            </Box>
          )}

          <Box flexDirection="column" marginTop={1}>
            {memories.length === 0 ? (
              <Text dimColor>No memories found.</Text>
            ) : (
              memories.map((memory, index) => (
                <Box key={memory.id} flexDirection="column">
                  <Text>
                    {index === selectedIndex ? colors.primary('❯ ') : '  '}
                    {memory.summary || memory.text?.substring(0, 50) || 'Untitled'}
                  </Text>
                  {index === selectedIndex && (
                    <Box flexDirection="column">
                      {memory.category && <Text dimColor>    Category: {memory.category}</Text>}
                      {memory.path && <Text dimColor>    Path: {memory.path}</Text>}
                      {memory.timestamp && <Text dimColor>    Time: {memory.timestamp}</Text>}
                      {memory.score !== undefined && <Text dimColor>    Score: {memory.score.toFixed(3)}</Text>}
                    </Box>
                  )}
                </Box>
              ))
            )}
          </Box>

          <Box flexDirection="column" marginTop={2}>
            <Text dimColor>↑↓ Navigate | q - Back</Text>
          </Box>
        </Box>
      </Box>
    );
  }

  // Menu view
  return (
    <Box flexDirection="column" padding={1}>
      <Box flexDirection="column" borderStyle="round" borderColor="cyan" paddingX={2} paddingY={1}>
        <Text bold>{t('memory.title')}</Text>

        {error && (
          <Box marginTop={1}>
            <Text>{colors.error(`✗ ${error}`)}</Text>
          </Box>
        )}
        
        <Box flexDirection="column" marginTop={1}>
          <Text>{`s - ${t('memory.search')}`}</Text>
          <Text>{`a - List All Memories`}</Text>
          <Text>{`k - List Kanban Cards`}</Text>
          <Text>{`q - ${t('daemon.back')}`}</Text>
        </Box>
      </Box>
    </Box>
  );
}
