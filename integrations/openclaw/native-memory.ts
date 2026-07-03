import type {
  MemoryProviderStatus,
  MemoryReadResult,
  MemorySearchManager,
  MemorySearchResult,
  OpenClawPluginApi,
} from "openclaw/plugin-sdk";
import type { MemoryEntry } from "./config.js";
import { buildHostContinuityPrompt } from "./host-continuity.js";
import { MemoryMcpClient } from "./mcp-client.js";

type SearchHit = {
  final_score: number;
  entry: MemoryEntry;
};

type NativeMemoryRegistration = {
  api: OpenClawPluginApi;
  topK: number;
  ensureClient: (agentId?: string) => Promise<MemoryMcpClient>;
  resolveAgentId: (agentId?: string) => string;
};

function entryToSearchResult(hit: SearchHit): MemorySearchResult {
  const entry = hit.entry;
  return {
    path: `memory/${entry.id}`,
    id: entry.id,
    title: entry.summary || entry.topic || entry.id,
    kind: entry.category || "memory",
    score: hit.final_score,
    snippet: entry.text,
    source: "tachi",
    provenanceLabel: "Tachi memory",
    sourceType: "memory",
    sourcePath: entry.path,
    updatedAt: entry.timestamp,
    startLine: 1,
    endLine: 1,
  };
}

function entryToReadResult(path: string, entry: MemoryEntry): MemoryReadResult {
  const content = [
    `ID: ${entry.id}`,
    `Topic: ${entry.topic}`,
    `Path: ${entry.path}`,
    `Timestamp: ${entry.timestamp}`,
    "",
    entry.text,
    "",
    `Keywords: ${entry.keywords.join(", ")}`,
    `Entities: ${entry.entities.join(", ")}`,
  ].join("\n");
  return {
    path,
    id: entry.id,
    title: entry.summary || entry.topic || entry.id,
    kind: entry.category || "memory",
    content,
    fromLine: 1,
    lineCount: content.split("\n").length,
    provenanceLabel: "Tachi memory",
    sourceType: "memory",
    sourcePath: entry.path,
    updatedAt: entry.timestamp,
  };
}

class TachiMemorySearchManager implements MemorySearchManager {
  constructor(
    private readonly topK: number,
    private readonly agentId: string,
    private readonly ensureClient: (agentId?: string) => Promise<MemoryMcpClient>,
  ) {}

  async search(query: string, opts?: { maxResults?: number; minScore?: number }): Promise<MemorySearchResult[]> {
    const client = await this.ensureClient(this.agentId);
    const payload = await client.searchMemory(query, undefined, {
      top_k: opts?.maxResults ?? this.topK,
    });
    return payload.docs
      .map((entry) => ({
        entry,
        final_score: payload.scores[entry.id] ?? 0,
      }))
      .filter((hit) => (opts?.minScore == null ? true : hit.final_score >= opts.minScore))
      .map(entryToSearchResult);
  }

  async readFile(params: { relPath: string }): Promise<MemoryReadResult> {
    const client = await this.ensureClient(this.agentId);
    const entryId = params.relPath.replace(/^(?:shadow-store|memory)\//, "");
    const entry = await client.getMemory(entryId);
    if (!entry) {
      return {
        path: params.relPath,
        content: "",
        fromLine: 1,
        lineCount: 0,
        title: "Missing Tachi memory",
        kind: "missing",
      };
    }
    return entryToReadResult(params.relPath, entry);
  }

  status(): MemoryProviderStatus {
    return {
      ok: true,
      provider: "tachi",
      message: "Tachi MCP memory manager registered",
      details: {
        agentId: this.agentId,
      },
    };
  }

  async sync(): Promise<void> {
    const client = await this.ensureClient(this.agentId);
    if (client.hasTool("tachi_event")) {
      await client.tachiEvent({
        action: "metrics",
        adapter: "openclaw",
        domain: "agent_host",
        limit: 20,
      });
    }
  }

  async probeEmbeddingAvailability(): Promise<{ ok: boolean; message?: string }> {
    return { ok: true, message: "Tachi owns embedding/vector probes" };
  }

  async probeVectorAvailability(): Promise<boolean> {
    return true;
  }
}

export function registerNativeTachiMemoryCapability(registration: NativeMemoryRegistration): boolean {
  const { api, topK, ensureClient, resolveAgentId } = registration;
  if (typeof api.registerMemoryCapability !== "function") {
    api.logger.warn("tachi: OpenClaw native memory capability API unavailable; using tool/hook compatibility mode");
    return false;
  }

  api.registerMemoryCapability({
    promptBuilder: () => buildHostContinuityPrompt(),
    runtime: {
      async getMemorySearchManager(params) {
        const agentId = resolveAgentId(params.agentId);
        return {
          manager: new TachiMemorySearchManager(topK, agentId, ensureClient),
        };
      },
      resolveMemoryBackendConfig() {
        return { backend: "builtin" };
      },
      async closeAllMemorySearchManagers() {
        // Individual MCP clients are owned and closed by the plugin service.
      },
    },
  });

  api.logger.info("tachi: registered native OpenClaw memory capability");
  return true;
}
