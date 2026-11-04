import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { MemoryEntry } from "./config.js";

export type HybridScore = {
  vector: number;
  fts: number;
  symbolic: number;
  decay: number;
  final: number;
};

type SearchPayload = {
  docs: MemoryEntry[];
  scores: Record<string, number>;
  scoreBreakdowns: Record<string, HybridScore>;
};

type LoggerLike = {
  info?: (message: string) => void;
  warn?: (message: string) => void;
};

type RuntimeInfoPayload = {
  runtime?: {
    name?: string;
    version?: string;
    binary?: string | null;
    tool_profile?: string;
    requested_profile?: string | null;
    derivative_identity?: string;
  };
  databases?: {
    global?: {
      path?: string;
      vec_available?: boolean;
    };
    project?: {
      path?: string;
      vec_available?: boolean;
    } | null;
    single_db_mode?: boolean;
  };
};

type SearchOptions = {
  top_k?: number;
  candidates?: number;
  path_prefix?: string;
  weights?: { semantic: number; fts: number; symbolic: number; decay: number };
};

type RecallContextOptions = {
  top_k?: number;
  candidate_multiplier?: number;
  path_prefix?: string;
  agent_id?: string;
  exclude_topics?: string[];
  min_score?: number;
};

type CompactContextParams = {
  agent_id: string;
  conversation_id: string;
  window_id: string;
  trigger?: string;
  messages: Array<{ role: string; content: string }>;
  current_summary?: string;
  path_prefix?: string;
  target_tokens?: number;
  max_output_tokens?: number;
  persist?: boolean;
};

type RawToolResult = CallToolResult & {
  structuredContent?: unknown;
  content?: Array<Record<string, unknown>>;
  isError?: boolean;
};

function asFiniteNumber(value: unknown): number {
  const n = typeof value === "number" ? value : Number(value);
  return Number.isFinite(n) ? n : 0;
}

function asString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function asStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((v): v is string => typeof v === "string");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isSourceRef(value: unknown): value is MemoryEntry["metadata"]["source_refs"][number] {
  return (
    isRecord(value) &&
    typeof value.ref_type === "string" &&
    typeof value.ref_id === "string"
  );
}

function ensureMetadata(value: unknown): MemoryEntry["metadata"] {
  if (!isRecord(value)) {
    return { source_refs: [] };
  }
  const sourceRefsRaw = value.source_refs;
  const sourceRefs = Array.isArray(sourceRefsRaw)
    ? sourceRefsRaw.filter((item): item is MemoryEntry["metadata"]["source_refs"][number] =>
        isSourceRef(item),
      )
    : [];
  return {
    ...value,
    source_refs: sourceRefs,
  };
}

function extractTextBlocks(content: unknown): string[] {
  if (!Array.isArray(content)) {
    return [];
  }
  const out: string[] = [];
  for (const block of content) {
    if (!isRecord(block)) {
      continue;
    }
    if (block.type === "text" && typeof block.text === "string") {
      out.push(block.text);
    }
  }
  return out;
}

function pushEntityName(entities: string[], name: string): string[] {
  const trimmed = name.trim();
  if (!trimmed) {
    return entities;
  }
  if (entities.some((entry) => entry.toLowerCase() === trimmed.toLowerCase())) {
    return entities;
  }
  const next = [...entities, trimmed];
  if (trimmed.toLowerCase() === "kyle" && !next.some((entry) => entry.toLowerCase() === "user")) {
    next.push("user");
  }
  return next;
}

function entitiesForSave(entry: MemoryEntry): string[] {
  let entities = [...(entry.entities ?? [])];
  for (const person of entry.persons ?? []) {
    entities = pushEntityName(entities, person);
  }
  return entities;
}

function coerceMemoryEntry(raw: unknown): MemoryEntry | undefined {
  if (!isRecord(raw)) {
    return undefined;
  }
  const id = asString(raw.id);
  if (!id) {
    return undefined;
  }
  const legacyPersons = asStringArray(raw.persons);
  let entities = asStringArray(raw.entities);
  for (const person of legacyPersons) {
    entities = pushEntityName(entities, person);
  }
  const importance = typeof raw.importance === "number" ||
    (typeof raw.importance === "string" && raw.importance.trim())
    ? Number(raw.importance)
    : NaN;
  return {
    id,
    text: asString(raw.text) || asString(raw.excerpt),
    summary: asString(raw.summary),
    keywords: asStringArray(raw.keywords),
    timestamp: asString(raw.timestamp),
    location: asString(raw.location),
    persons: [],
    entities,
    topic: asString(raw.topic),
    scope: asString(raw.scope) || "general",
    path: asString(raw.path) || "/",
    category: (asString(raw.category) || "other") as MemoryEntry["category"],
    importance: Number.isFinite(importance) ? importance : 0.7,
    access_count: asFiniteNumber(raw.access_count),
    last_access: typeof raw.last_access === "string" ? raw.last_access : null,
    vector: Array.isArray(raw.vector)
      ? raw.vector
          .map((v) => asFiniteNumber(v))
          .filter((v) => Number.isFinite(v))
      : undefined,
    metadata: ensureMetadata(raw.metadata),
  };
}

function extractMemoryRows(payload: unknown): unknown[] {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (!isRecord(payload)) {
    return [];
  }

  if (Array.isArray(payload.results)) {
    return payload.results;
  }
  if (Array.isArray(payload.docs)) {
    return payload.docs;
  }

  if (!Array.isArray(payload.sections)) {
    return [];
  }
  const rows: unknown[] = [];
  for (const section of payload.sections) {
    if (isRecord(section) && Array.isArray(section.rows)) {
      rows.push(...section.rows);
    }
  }
  return rows;
}

function extractErrorMessage(result: RawToolResult, toolName: string): string {
  const text = extractTextBlocks(result.content).find(Boolean);
  return text || `MCP tool "${toolName}" returned an error`;
}

// Branch #7 — tolerant JSON parser for MCP tool payloads.
// Sigil's tachi-server occasionally returns text blocks with a UTF-8 BOM,
// trailing whitespace/newlines, or a stray log line prepended. The MCP
// SDK hands us those blocks verbatim. Try strict parse first, then fall
// back to BOM/whitespace strip + braces/brackets substring recovery so a
// single noisy block doesn't poison an otherwise valid response.
function tryParseJson<T>(raw: string): T | undefined {
  if (!raw) return undefined;
  try {
    return JSON.parse(raw) as T;
  } catch {
    /* fall through */
  }
  let cleaned = raw;
  if (cleaned.charCodeAt(0) === 0xfeff) {
    cleaned = cleaned.slice(1);
  }
  cleaned = cleaned.trim();
  if (cleaned !== raw) {
    try {
      return JSON.parse(cleaned) as T;
    } catch {
      /* fall through */
    }
  }
  // Last resort: locate the first balanced JSON value (object or array).
  const firstObj = cleaned.indexOf("{");
  const firstArr = cleaned.indexOf("[");
  const candidates: Array<[number, string]> = [];
  if (firstObj >= 0) candidates.push([firstObj, "}"]);
  if (firstArr >= 0) candidates.push([firstArr, "]"]);
  candidates.sort((a, b) => a[0] - b[0]);
  for (const [start, closer] of candidates) {
    const end = cleaned.lastIndexOf(closer);
    if (end > start) {
      try {
        return JSON.parse(cleaned.slice(start, end + 1)) as T;
      } catch {
        continue;
      }
    }
  }
  return undefined;
}

function extractJsonPayload<T>(result: RawToolResult, toolName: string): T {
  if (result.structuredContent !== undefined) {
    if (typeof result.structuredContent === "string") {
      const parsed = tryParseJson<T>(result.structuredContent);
      if (parsed !== undefined) return parsed;
      throw new Error(`MCP tool "${toolName}" returned unparseable structuredContent string`);
    }
    return result.structuredContent as T;
  }

  const textBlocks = extractTextBlocks(result.content).filter((text) => text.trim().length > 0);

  for (const text of textBlocks) {
    const parsed = tryParseJson<T>(text);
    if (parsed !== undefined) return parsed;
  }

  throw new Error(`MCP tool "${toolName}" returned non-JSON content`);
}

export class MemoryMcpClient {
  private client: { callTool: (request: { name: string; arguments: Record<string, unknown> }) => Promise<unknown>; close: () => Promise<void> } | null = null;
  private availableTools = new Set<string>();
  private runtimeInfo: RuntimeInfoPayload | null = null;


  constructor(
    private readonly globalDbPath: string,
    private readonly projectDbPath: string,
    private readonly logger?: LoggerLike,
  ) {}

  private async getClient() {
    if (this.client) return this.client;
    throw new Error("The legacy OpenClaw Tachi adapter is retired: it requires process-owned DB paths, the privileged operate profile and legacy MCP tools that public HTTP clients cannot select. It will not spawn Tachi or send an HTTP request. Start `tachi --daemon --port 6919 serve` and configure a supported native HTTP client with `python3 scripts/setup_agent_mcp.py --agent claude-code --url http://127.0.0.1:6919/mcp --apply`. This plugin's memory integration requires a separate standard-facade migration.");
  }

  private async resetConnection(): Promise<void> {
    const client = this.client;
    this.client = null;
    this.runtimeInfo = null;
    this.availableTools.clear();
    await client?.close().catch(() => {});
  }

  async close(): Promise<void> {
    await this.resetConnection();
  }

  private async callJson<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
    const client = await this.getClient();
    let result: RawToolResult;
    try {
      result = (await client.callTool({ name, arguments: args })) as RawToolResult;
    } catch (error) {
      await this.resetConnection();
      throw error;
    }

    if (result.isError) {
      throw new Error(extractErrorMessage(result, name));
    }
    return extractJsonPayload<T>(result, name);
  }

  async getRuntimeInfo(): Promise<RuntimeInfoPayload> {
    await this.getClient();
    return this.runtimeInfo ?? {};
  }

  async saveMemory(entry: MemoryEntry): Promise<void> {
    await this.callJson<Record<string, unknown>>("save_memory", {
      id: entry.id,
      text: entry.text,
      summary: entry.summary,
      path: entry.path,
      importance: entry.importance,
      category: entry.category,
      topic: entry.topic,
      keywords: entry.keywords,
      persons: [],
      entities: entitiesForSave(entry),
      location: entry.location,
      scope: entry.scope,
      vector: entry.vector,
      force: true,
      auto_link: false,
      timestamp: entry.timestamp,
      metadata: entry.metadata,
    });
  }

  async getMemory(id: string): Promise<MemoryEntry | undefined> {
    const payload = await this.callJson<unknown>("get_memory", {
      id,
      include_archived: false,
    });
    if (isRecord(payload) && typeof payload.error === "string") {
      return undefined;
    }
    return coerceMemoryEntry(payload);
  }

  async listMemories(limit: number): Promise<MemoryEntry[]> {
    const payload = await this.callJson<unknown>("list_memories", {
      path_prefix: "/",
      limit,
      include_archived: false,
    });
    if (!Array.isArray(payload)) {
      return [];
    }
    return payload.map((row) => coerceMemoryEntry(row)).filter((entry): entry is MemoryEntry => Boolean(entry));
  }

  async searchMemory(query: string, queryVec?: number[], opts?: SearchOptions): Promise<SearchPayload> {
    const topK = opts?.top_k;
    // Both tool calls and the native memory manager use this boundary. Rust
    // accepts usize and normalizes 0 / large counts, but cannot decode fractions.
    if (topK !== undefined && (!Number.isSafeInteger(topK) || topK < 0)) {
      throw new TypeError("top_k must be a non-negative safe integer");
    }
    const payload = await this.callJson<unknown>("search_memory", {
      query,
      top_k: topK,
      path_prefix: opts?.path_prefix,
      include_archived: false,
      candidates_per_channel: opts?.candidates,
      graph_expand_hops: 0,
      graph_relation_filter: null,
      // tachi#1201 k3: search_memory now defaults to a markdown digest when
      // `format` is omitted; this client parses the JSON row shape below, so
      // pin the pre-k3 contract explicitly.
      format: "json",
      ...(queryVec && queryVec.length > 0 ? { query_vec: queryVec } : {}),
      ...(opts?.weights ? { weights: opts.weights } : {}),
    });

    const docs: MemoryEntry[] = [];
    const scores: Record<string, number> = {};
    const scoreBreakdowns: Record<string, HybridScore> = {};

    for (const row of extractMemoryRows(payload)) {
      const entry = coerceMemoryEntry(row);
      if (!entry) {
        continue;
      }
      const scoreRecord = isRecord(row) && isRecord(row.score) ? row.score : null;
      const finalScore = asFiniteNumber(
        scoreRecord?.final ?? scoreRecord?.final_score ?? (isRecord(row) ? row.relevance : undefined),
      );
      const breakdown: HybridScore = {
        vector: asFiniteNumber(scoreRecord?.vector),
        fts: asFiniteNumber(scoreRecord?.fts),
        symbolic: asFiniteNumber(scoreRecord?.symbolic),
        decay: asFiniteNumber(scoreRecord?.decay),
        final: finalScore,
      };
      docs.push(entry);
      scores[entry.id] = finalScore;
      scoreBreakdowns[entry.id] = breakdown;
    }

    return { docs, scores, scoreBreakdowns };
  }

  async recallContext(
    query: string,
    opts?: RecallContextOptions,
  ): Promise<{
    prependContext: string;
    results: Array<{ entry: MemoryEntry; final_score: number }>;
  }> {
    const payload = await this.callJson<unknown>("recall_context", {
      query,
      top_k: opts?.top_k,
      candidate_multiplier: opts?.candidate_multiplier,
      path_prefix: opts?.path_prefix,
      agent_id: opts?.agent_id,
      exclude_topics: opts?.exclude_topics,
      min_score: opts?.min_score,
    });

    let prependContext = "";
    const results: Array<{ entry: MemoryEntry; final_score: number }> = [];

    if (isRecord(payload) && typeof payload.prepend_context === "string") {
      prependContext = payload.prepend_context;
    }

    const rows = isRecord(payload) && Array.isArray(payload.results) ? payload.results : [];
    for (const row of rows) {
      const entry = coerceMemoryEntry(row);
      if (!entry) {
        continue;
      }
      const finalScore = asFiniteNumber(
        (isRecord(row) ? row.relevance : undefined) ??
          (isRecord(row) && isRecord(row.score) ? row.score.final : undefined),
      );
      results.push({ entry, final_score: finalScore });
    }

    return { prependContext, results };
  }

  async captureSession(params: {
    conversation_id: string;
    turn_id: string;
    agent_id: string;
    messages: Array<{ role: string; content: string }>;
    path_prefix?: string;
    scope?: string;
    force?: boolean;
  }): Promise<unknown> {
    return await this.callJson<unknown>("capture_session", params);
  }

  async compactContext(
    params: CompactContextParams,
  ): Promise<{
    status: string;
    compacted_text: string;
    estimated_tokens: number;
  }> {
    if (!this.availableTools.has("compact_context")) {
      throw new Error("compact_context tool is unavailable");
    }
    return await this.callJson("compact_context", params);
  }

  async findSimilarMemory(
    queryVec: number[],
    topK: number,
  ): Promise<Array<{ entry: MemoryEntry; similarity: number }>> {
    if (!this.availableTools.has("find_similar_memory")) {
      throw new Error("find_similar_memory tool is unavailable");
    }

    const payload = await this.callJson<unknown>("find_similar_memory", {
      query_vec: queryVec,
      top_k: topK,
      candidates_per_channel: Math.max(topK, 20),
      include_archived: false,
    });

    if (!Array.isArray(payload)) {
      return [];
    }

    const out: Array<{ entry: MemoryEntry; similarity: number }> = [];
    for (const row of payload) {
      const entry = coerceMemoryEntry(row);
      if (!entry) {
        continue;
      }
      const similarity = asFiniteNumber(
        (isRecord(row) ? row.similarity : undefined) ??
          (isRecord(row) && isRecord(row.score) ? row.score.vector : undefined),
      );
      if (similarity > 0) {
        out.push({ entry, similarity });
      }
    }
    return out;
  }

  async memoryStats(): Promise<unknown> {
    return await this.callJson<unknown>("memory_stats", {});
  }

  hasTool(toolName: string): boolean {
    return this.availableTools.has(toolName);
  }

  async tachiEvent(params: Record<string, unknown>): Promise<unknown> {
    await this.getClient();
    if (!this.availableTools.has("tachi_event")) {
      throw new Error("tachi_event tool is unavailable");
    }
    return await this.callJson<unknown>("tachi_event", params);
  }

  async runtimeInfoPayload(): Promise<RuntimeInfoPayload> {
    return await this.callJson<RuntimeInfoPayload>("runtime_info", {});
  }

  async callTool(toolName: string, args: Record<string, unknown>): Promise<unknown> {
    return await this.callJson<unknown>(toolName, args);
  }
}
