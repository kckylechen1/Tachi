import { loadConfig } from './config.js';
import { callTachiTool, requireArray } from './tachiMcp.js';

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

interface MemorySearchSection {
  rows?: unknown;
  error?: { message?: unknown };
}

function memorySearchRows(value: unknown): MemoryEntry[] {
  if (typeof value !== 'object' || value === null || !Array.isArray((value as { sections?: unknown }).sections)) {
    throw new Error('tachi_memory returned malformed response: expected search sections');
  }
  return (value as { sections: unknown[] }).sections.flatMap(rawSection => {
    if (typeof rawSection !== 'object' || rawSection === null) {
      throw new Error('tachi_memory returned a malformed search section');
    }
    const section = rawSection as MemorySearchSection;
    if (section.error) {
      const message = typeof section.error.message === 'string' ? section.error.message : 'unknown section error';
      throw new Error(`tachi_memory search failed: ${message}`);
    }
    return requireArray<MemoryEntry>(section.rows, 'tachi_memory search section');
  });
}

export async function searchMemory(query: string, topK = 10): Promise<MemoryEntry[]> {
  const port = loadConfig().daemon.port || DEFAULT_DAEMON_PORT;
  return memorySearchRows(await callTachiTool(port, 'tachi_memory', {
    action: 'search', query, top_k: topK, include_archived: false, format: 'json',
  }));
}

export async function listMemories(pathPrefix?: string, limit = 50): Promise<MemoryEntry[]> {
  const port = loadConfig().daemon.port || DEFAULT_DAEMON_PORT;
  return requireArray<MemoryEntry>(await callTachiTool(port, 'list_memories', {
    ...(pathPrefix === undefined ? {} : { path_prefix: pathPrefix }),
    limit,
    include_archived: false,
  }), 'list_memories');
}
