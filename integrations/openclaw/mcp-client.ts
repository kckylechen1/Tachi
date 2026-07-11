import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
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

type LaunchConfig = {
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
};

type RawToolResult = CallToolResult & {
  structuredContent?: unknown;
  content?: Array<Record<string, unknown>>;
  isError?: boolean;
};

const REQUIRED_TOOLS = [
  "runtime_info",
  "recall_context",
  "capture_session",
  "save_memory",
  "search_memory",
  "get_memory",
  // memory_graph removed from MCP surface (#757); graph is internal-only
  "memory_stats",
  "list_memories",
] as const;

function normalizePathForCompare(value: string): string {
  return path.resolve(value.replace(/^~/, os.homedir()));
}

function pathsEqualForRouting(left: string | null | undefined, right: string | null | undefined): boolean {
  if (!left || !right) return false;
  return normalizePathForCompare(left) === normalizePathForCompare(right);
}

function runtimeInfoSummary(info: RuntimeInfoPayload): string {
  return JSON.stringify({
    runtime: info.runtime,
    databases: info.databases,
  });
}

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
    source_refs: sourceRefs,
    ...value,
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
    importance: asFiniteNumber(raw.importance) || 0.7,
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
  private client: Client | null = null;
  private transport: StdioClientTransport | null = null;
  private connecting: Promise<Client> | null = null;
  private availableTools = new Set<string>();
  private runtimeInfo: RuntimeInfoPayload | null = null;

  private static readonly CLIENT_VERSION = "1.2.0";

  constructor(
    private readonly globalDbPath: string,
    private readonly projectDbPath: string,
    private readonly logger?: LoggerLike,
  ) {}

  private logInfo(message: string): void {
    this.logger?.info?.(`tachi[mcp]: ${message}`);
  }

  private logWarn(message: string): void {
    this.logger?.warn?.(`tachi[mcp]: ${message}`);
  }

  private resolveServerCommand(): string {
    // Priority: TACHI_BIN > OPENCLAW_MEMORY_SERVER_BIN > user install > local build > Homebrew > PATH.
    // Homebrew can lag behind the active development binary, so prefer ~/bin/tachi
    // when present. Operators can still pin a different binary with TACHI_BIN.
    const fromEnv = (process.env.TACHI_BIN || process.env.OPENCLAW_MEMORY_SERVER_BIN)?.trim();
    if (fromEnv) {
      return fromEnv;
    }

    const userBinary = path.join(os.homedir(), "bin", "tachi");
    if (fs.existsSync(userBinary)) {
      return userBinary;
    }

    const moduleDir = path.dirname(fileURLToPath(import.meta.url));
    const localBinary = path.resolve(moduleDir, "../../target/release/tachi-server");
    if (fs.existsSync(localBinary)) {
      return localBinary;
    }

    const packagedCandidates = process.platform === "darwin"
      ? ["/opt/homebrew/opt/tachi/bin/tachi", "/usr/local/opt/tachi/bin/tachi"]
      : [];
    for (const candidate of packagedCandidates) {
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }

    // Prefer "tachi" (brew install name) over "tachi-server" (dev name)
    return "tachi";
  }

  private buildLaunchCandidates(): LaunchConfig[] {
    const command = this.resolveServerCommand();
    const env = {
      ...process.env,
      TACHI_GLOBAL_DB_PATH: this.globalDbPath,
      // Rust still treats MEMORY_DB_PATH as the legacy global DB env alias.
      MEMORY_DB_PATH: this.globalDbPath,
      TACHI_PROFILE: process.env.TACHI_PROFILE || "openclaw",
      TACHI_DERIVATIVE_IDENTITY: process.env.TACHI_DERIVATIVE_IDENTITY || "openclaw-tachi",
      TACHI_EMBEDDED_MCP: process.env.TACHI_EMBEDDED_MCP || "1",
    } as Record<string, string>;
    const candidates: LaunchConfig[] = [
      // First candidate: explicit global + OpenClaw agent/workspace project DB.
      {
        command,
        args: ["--global-db", this.globalDbPath, "--project-db", this.projectDbPath],
        env,
        cwd: os.tmpdir(),
      },
      // Second candidate: compatibility launch. Runtime self-check below
      // rejects accidental cwd/project mismatches.
      {
        command,
        args: ["--project-db", this.projectDbPath],
        env,
        cwd: process.cwd(),
      },
    ];
    // If primary command is "tachi", also try "tachi-server" as last resort
    if (command === "tachi") {
      candidates.push({
        command: "tachi-server",
        args: ["--global-db", this.globalDbPath, "--project-db", this.projectDbPath],
        env,
        cwd: os.tmpdir(),
      });
    }
    return candidates;
  }

  private async connectWith(launch: LaunchConfig): Promise<Client> {
    const transport = new StdioClientTransport({
      command: launch.command,
      args: launch.args,
      env: launch.env,
      cwd: launch.cwd,
      stderr: "pipe",
    });

    const client = new Client(
      {
        name: "tachi-openclaw",
        version: MemoryMcpClient.CLIENT_VERSION,
      },
      {},
    );

    try {
      await client.connect(transport);
      const listed = await client.listTools();
      const names = new Set(listed.tools.map((tool) => tool.name));
      for (const required of REQUIRED_TOOLS) {
        if (!names.has(required)) {
          throw new Error(`required MCP tool missing: ${required}`);
        }
      }

      this.client = client;
      this.transport = transport;
      this.availableTools = names;
      this.runtimeInfo = await this.verifyRuntimeRouting(client, launch);
      return client;
    } catch (error) {
      await client.close().catch(() => {});
      await transport.close().catch(() => {});
      throw error;
    }
  }

  private async getClient(): Promise<Client> {
    if (this.client) {
      return this.client;
    }
    if (!this.connecting) {
      this.connecting = (async () => {
        const attempts = this.buildLaunchCandidates();
        let lastError: unknown = null;

        for (let i = 0; i < attempts.length; i++) {
          const launch = attempts[i];
          try {
            const client = await this.connectWith(launch);
            if (i > 0) {
              this.logWarn("connected via compatibility launch");
            } else {
              this.logInfo(`connected to ${launch.command}`);
            }
            return client;
          } catch (error) {
            lastError = error;
            this.client = null;
            this.transport = null;
            this.availableTools.clear();
            continue;
          }
        }

        throw new Error(`failed to connect memory MCP server: ${String(lastError)}`);
      })().finally(() => {
        this.connecting = null;
      });
    }
    return await this.connecting;
  }

  private async resetConnection(): Promise<void> {
    const client = this.client;
    const transport = this.transport;
    this.client = null;
    this.transport = null;
    this.runtimeInfo = null;
    this.availableTools.clear();
    await client?.close().catch(() => {});
    await transport?.close().catch(() => {});
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

  private async verifyRuntimeRouting(client: Client, launch: LaunchConfig): Promise<RuntimeInfoPayload> {
    let result: RawToolResult;
    try {
      result = (await client.callTool({ name: "runtime_info", arguments: {} })) as RawToolResult;
    } catch (error) {
      throw new Error(`runtime_info self-check failed for ${launch.command}: ${String(error)}`);
    }
    if (result.isError) {
      throw new Error(`runtime_info self-check rejected for ${launch.command}: ${extractErrorMessage(result, "runtime_info")}`);
    }
    const info = extractJsonPayload<RuntimeInfoPayload>(result, "runtime_info");
    const globalPath = info.databases?.global?.path;
    const projectPath = info.databases?.project?.path;
    const profile = info.runtime?.tool_profile;
    const requestedProfile = info.runtime?.requested_profile;
    const identity = info.runtime?.derivative_identity;
    const errors: string[] = [];
    if (!pathsEqualForRouting(globalPath, this.globalDbPath)) {
      errors.push(`global DB mismatch: expected ${this.globalDbPath}, got ${globalPath || "<missing>"}`);
    }
    if (!pathsEqualForRouting(projectPath, this.projectDbPath)) {
      errors.push(`project DB mismatch: expected ${this.projectDbPath}, got ${projectPath || "<missing>"}`);
    }
    if (requestedProfile !== "openclaw") {
      errors.push(`requested profile mismatch: expected openclaw, got ${requestedProfile || "<missing>"}`);
    }
    if (identity !== "openclaw-tachi") {
      errors.push(`derivative identity mismatch: expected openclaw-tachi, got ${identity || "<missing>"}`);
    }
    if (errors.length > 0) {
      throw new Error(`Tachi runtime routing self-check failed: ${errors.join("; ")}; info=${runtimeInfoSummary(info)}`);
    }
    this.logInfo(
      `runtime verified profile=${profile || "<missing>"} requested=${requestedProfile} identity=${identity} global=${normalizePathForCompare(this.globalDbPath)} project=${normalizePathForCompare(this.projectDbPath)}`,
    );
    return info;
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
    const payload = await this.callJson<unknown>("search_memory", {
      query,
      top_k: opts?.top_k,
      path_prefix: opts?.path_prefix,
      include_archived: false,
      candidates_per_channel: opts?.candidates,
      graph_expand_hops: 0,
      graph_relation_filter: null,
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
    queued_job_ids?: string[];
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

  async deleteMemory(id: string): Promise<boolean> {
    if (!this.availableTools.has("tachi_memory")) {
      throw new Error("tachi_memory tool is unavailable");
    }
    const payload = await this.callJson<unknown>("tachi_memory", {
      action: "delete",
      id,
    });
    if (!isRecord(payload)) {
      return false;
    }
    return payload.deleted === true;
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
