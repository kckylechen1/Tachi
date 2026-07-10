import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import fsSync from "node:fs";
import os from "node:os";
import path from "node:path";
import { Type } from "@sinclair/typebox";
import type { OpenClawPluginApi } from "openclaw/plugin-sdk";
import { bridgeConfigSchema, type MemoryEntry } from "./config.js";
import {
  emitHostContinuityEvent,
  loadContinuityBoard,
  resolveHostRole,
} from "./host-continuity.js";
import { MemoryMcpClient } from "./mcp-client.js";
import { registerNativeTachiMemoryCapability } from "./native-memory.js";

// ---------------------------------------------------------------------------
// Branch #7 — Tachi manifest-aware DB routing
// ---------------------------------------------------------------------------
// Sigil's tachi-server now publishes ~/.tachi/manifest.json (see
// crates/tachi-server/src/manifest.rs). Each entry tags an owned DB with
// {role, owner, allow_write, scope_hint}. OpenClaw agents have entries like:
//   { role: "agent", owner: "openclaw-agent:<id>", allow_write: true, ... }
// We consult the manifest before falling back to the legacy
// `<baseDir>/agents/<id>/<dbName>` layout so that agents whose DB lives
// outside `appHome` (e.g. ~/.openclaw/agents/<id>/memory/memory.db) still
// route correctly. Read-only entries are skipped — capture would just be
// rejected by the server-side capture_gate anyway.
type ManifestDbEntry = {
  path?: string;
  role?: string;
  owner?: string;
  allow_write?: boolean;
  scope_hint?: string;
};
type ManifestFile = {
  schema_version?: number;
  dbs?: ManifestDbEntry[];
};
type ManifestSnapshot = {
  mtimeMs: number;
  byAgent: Map<string, string>;
};

function tachiManifestPath(): string {
  const home = process.env.TACHI_HOME || process.env.SIGIL_HOME;
  const base = home ? home.replace(/^~/, os.homedir()) : path.join(os.homedir(), ".tachi");
  return path.resolve(base, "manifest.json");
}

let manifestCache: ManifestSnapshot | null = null;

function loadManifestSnapshot(): ManifestSnapshot | null {
  const manifestPath = tachiManifestPath();
  let stat: fsSync.Stats;
  try {
    stat = fsSync.statSync(manifestPath);
  } catch {
    manifestCache = null;
    return null;
  }
  if (manifestCache && manifestCache.mtimeMs === stat.mtimeMs) {
    return manifestCache;
  }
  let parsed: ManifestFile;
  try {
    const raw = fsSync.readFileSync(manifestPath, "utf8");
    parsed = JSON.parse(raw) as ManifestFile;
  } catch {
    return manifestCache; // fall back to last known good snapshot
  }
  const byAgent = new Map<string, string>();
  for (const entry of parsed.dbs ?? []) {
    if (!entry || typeof entry.path !== "string") continue;
    if (entry.allow_write === false) continue;
    if (entry.role !== "agent") continue;
    const owner = (entry.owner ?? "").toLowerCase();
    const match = /^openclaw-agent:(.+)$/.exec(owner);
    if (!match) continue;
    const agentId = match[1].trim();
    if (!agentId) continue;
    if (!byAgent.has(agentId)) {
      byAgent.set(agentId, entry.path);
    }
  }
  manifestCache = { mtimeMs: stat.mtimeMs, byAgent };
  return manifestCache;
}

function lookupAgentDbInManifest(agentId: string): string | null {
  const snap = loadManifestSnapshot();
  if (!snap) return null;
  return snap.byAgent.get(agentId.toLowerCase()) ?? null;
}

type SearchHit = {
  final_score: number;
  entry: MemoryEntry;
};

type SearchResult =
  | { available: true; hits: SearchHit[] }
  | { available: false; message: string };

function resolveConfigPath(api: OpenClawPluginApi, configuredPath: string): string {
  return path.isAbsolute(configuredPath) ? configuredPath : api.resolvePath(configuredPath);
}

function textResult(text: string, details?: Record<string, unknown>) {
  return {
    content: [{ type: "text" as const, text }],
    ...(details ? { details } : {}),
  };
}

function makeMemoryId(): string {
  return `m_${Date.now()}_${Math.random().toString(16).slice(2, 10)}`;
}

/**
 * Strip model reasoning blocks that "thinking" models (Qwen3.x, GLM, DeepSeek-R*)
 * emit inline. They carry no recall value and have polluted captured OpenClaw
 * memories (observed ~200 leaked `<think>` blocks in a busy agent DB). Mirrors
 * the server-side noise.rs strip so capture and distill agree.
 */
function stripThinkBlocks(text: string): string {
  if (!text || text.indexOf("<think") === -1) {
    return text;
  }
  return text
    // paired <think>…</think> / <thinking> / <reasoning>
    .replace(/<(think|thinking|reasoning)\b[^>]*>[\s\S]*?<\/\1>/gi, "")
    // unterminated reasoning block (model truncated before the closing tag)
    .replace(/<(think|thinking|reasoning)\b[^>]*>[\s\S]*$/gi, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function messageToText(message: any): string {
  if (!message) {
    return "";
  }
  let raw = "";
  if (typeof message.content === "string") {
    raw = message.content;
  } else if (Array.isArray(message.content)) {
    raw = message.content
      .map((block) => {
        if (typeof block === "string") {
          return block;
        }
        if (block && typeof block.text === "string") {
          return block.text;
        }
        return "";
      })
      .filter(Boolean)
      .join("\n");
  }
  return stripThinkBlocks(raw);
}

function normalizeCaptureMessage(role: string, content: string): { role: string; content: string } | null {
  const normalizedRole = role === "assistant" ? "assistant" : "user";
  const text = String(content || "").trim();
  if (!text || text === "[OpenClaw heartbeat poll]" || text === "HEARTBEAT_OK") {
    return null;
  }
  return { role: normalizedRole, content: text };
}

async function readJsonlRows(filePath: string): Promise<any[]> {
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf8");
  } catch {
    return [];
  }
  const rows: any[] = [];
  for (const line of raw.split("\n")) {
    if (!line.trim()) continue;
    try {
      rows.push(JSON.parse(line));
    } catch {
      continue;
    }
  }
  return rows;
}

type SessionIndexEntry = {
  sessionId?: string;
  sessionFile?: string;
};

function addSessionCandidate(files: string[], candidate: string | null | undefined): void {
  if (!candidate) return;
  const resolved = path.isAbsolute(candidate) ? candidate : path.resolve(candidate);
  if (!files.includes(resolved)) {
    files.push(resolved);
  }
}

function addSessionIdCandidates(files: string[], sessionDir: string, sessionId: string | null | undefined): void {
  if (!sessionId) return;
  addSessionCandidate(files, path.join(sessionDir, `${sessionId}.jsonl`));
  addSessionCandidate(files, path.join(sessionDir, `${sessionId}.trajectory.jsonl`));
}

function addSessionFileCandidates(files: string[], sessionFile: string | null | undefined): void {
  if (!sessionFile) return;
  addSessionCandidate(files, sessionFile);
  if (sessionFile.endsWith(".jsonl") && !sessionFile.endsWith(".trajectory.jsonl")) {
    addSessionCandidate(files, sessionFile.replace(/\.jsonl$/, ".trajectory.jsonl"));
  }
}

async function resolveSessionFiles(agentId: string, sessionId: string | null | undefined, sessionKey: string | null | undefined): Promise<string[]> {
  const sessionDir = path.join(os.homedir(), ".openclaw", "agents", agentId, "sessions");
  const files: string[] = [];
  const refs = [sessionId, sessionKey].filter((value): value is string => Boolean(value));
  for (const ref of refs) {
    addSessionIdCandidates(files, sessionDir, ref);
  }

  const sessionIndex = await readJsonFile<Record<string, SessionIndexEntry>>(
    path.join(sessionDir, "sessions.json"),
    {},
  );
  for (const ref of refs) {
    const indexed = sessionIndex[ref];
    if (indexed) {
      addSessionFileCandidates(files, indexed.sessionFile);
      addSessionIdCandidates(files, sessionDir, indexed.sessionId);
    }
    for (const entry of Object.values(sessionIndex)) {
      if (entry?.sessionId !== ref && entry?.sessionFile !== ref) continue;
      addSessionFileCandidates(files, entry.sessionFile);
      addSessionIdCandidates(files, sessionDir, entry.sessionId);
    }
  }
  return files;
}

function captureMessagesFromRows(rows: any[]): Array<{ role: string; content: string }> {
  const messages: Array<{ role: string; content: string }> = [];
  for (const row of rows) {
    if (row?.type === "message" && row.message) {
      const role = typeof row.message.role === "string" ? row.message.role : "unknown";
      if (role !== "user" && role !== "assistant") continue;
      const message = normalizeCaptureMessage(role, messageToText(row.message));
      if (message) messages.push(message);
      continue;
    }

    if (row?.type === "prompt.submitted" && typeof row?.data?.prompt === "string") {
      const message = normalizeCaptureMessage("user", row.data.prompt);
      if (message) messages.push(message);
      continue;
    }

    if (row?.type === "model.completed") {
      if (Array.isArray(row?.data?.messagesSnapshot)) {
        messages.push(
          ...captureMessagesFromRows(row.data.messagesSnapshot.map((message: any) => ({ type: "message", message }))),
        );
        continue;
      }
      const texts = Array.isArray(row?.data?.assistantTexts) ? row.data.assistantTexts : [];
      for (const text of texts) {
        if (typeof text !== "string") continue;
        const message = normalizeCaptureMessage("assistant", text);
        if (message) messages.push(message);
      }
    }
  }
  return messages;
}

async function readSessionMessages(
  agentId: string,
  sessionId: string | null | undefined,
  sessionKey: string | null | undefined,
): Promise<Array<{ role: string; content: string }>> {
  const sessionFiles = await resolveSessionFiles(agentId, sessionId, sessionKey);
  for (const sessionFile of sessionFiles) {
    const messages = captureMessagesFromRows(await readJsonlRows(sessionFile));
    if (messages.length > 0) {
      return messages.slice(-8);
    }
  }
  return [];
}

type SelfEvolutionInsight = {
  note: string;
  messageIndex: number;
  anchored: boolean;
};

function normalizeBracketInsight(text: unknown): string {
  return String(text || "").replace(/\s+/g, " ").trim();
}

function stripCoreRuleAnchor(text: string): string {
  return normalizeBracketInsight(text).replace(/^\[核心法则\]\s*/i, "").trim();
}

function summarizeInsight(text: string): string {
  const normalized = stripCoreRuleAnchor(text);
  return normalized.length <= 28 ? normalized : `${normalized.slice(0, 27)}…`;
}

function isSelfEvolutionInsight(text: string): boolean {
  const normalized = normalizeBracketInsight(text);
  if (!normalized) {
    return false;
  }
  if (normalized.includes("[核心法则]")) {
    return true;
  }
  return /(原来.{0,20}(喜欢|不喜欢)|记住了|下次我要|下次我会|以后我要|以后我会|雷区|更吃这一套|不吃这一套|这样更有效|这种方式有用|策略失败|无效)/i.test(
    normalized,
  );
}

function classifySelfEvolution(text: string): MemoryEntry["category"] {
  if (/(记住了|下次我要|下次我会|以后我要|以后我会|策略失败|无效)/i.test(text)) {
    return "decision";
  }
  if (/(喜欢|不喜欢|雷区|偏好|讨厌|更吃|不吃)/i.test(text)) {
    return "preference";
  }
  return "other";
}

function buildSelfEvolutionId(agentId: string, note: string): string {
  const seed = `${agentId.trim()}:${stripCoreRuleAnchor(note)}`;
  return `self-evo-${createHash("sha1").update(seed).digest("hex").slice(0, 16)}`;
}

function extractSelfEvolutionInsights(messages: any[]): SelfEvolutionInsight[] {
  const insights: SelfEvolutionInsight[] = [];
  const seen = new Set<string>();

  for (let messageIndex = 0; messageIndex < messages.length; messageIndex++) {
    const message = messages[messageIndex];
    if (message?.role !== "assistant") {
      continue;
    }
    const text = messageToText(message);
    const matches = text.matchAll(/[（(]([^()（）\n]{4,240})[)）]/g);
    for (const match of matches) {
      const raw = normalizeBracketInsight(match[1]);
      const note = stripCoreRuleAnchor(raw);
      const anchored = raw.includes("[核心法则]");
      if (!note || note.length < 4 || !isSelfEvolutionInsight(raw)) {
        continue;
      }
      const dedupeKey = note.toLowerCase();
      if (seen.has(dedupeKey)) {
        continue;
      }
      seen.add(dedupeKey);
      insights.push({ note, messageIndex, anchored });
    }
  }

  return insights;
}

function buildSelfEvolutionMemory(
  agentId: string,
  memoryNamespaceAgentId: string,
  sessionKey: string,
  insight: SelfEvolutionInsight,
  insightIndex: number,
  timestamp: string,
): MemoryEntry {
  const category = classifySelfEvolution(insight.note);
  const isJayne = agentId === "jayne";
  const userFacing = isJayne && (category === "preference" || category === "decision");
  return {
    id: buildSelfEvolutionId(agentId, insight.note),
    text: insight.note,
    summary: summarizeInsight(insight.note),
    keywords: [
      agentId,
      "self-evolution",
      "bracket-note",
      insight.anchored ? "core-rule" : "",
      category === "decision" ? "strategy" : "",
      userFacing ? "kyle-preference" : "",
    ].filter(Boolean),
    timestamp,
    location: "agent_end",
    persons: [],
    entities: userFacing ? ["user", "Kyle"] : [agentId],
    topic: isJayne ? "jayne_self_evolution" : "agent_self_evolution",
    scope: userFacing ? "user" : "project",
    path: `/openclaw/agent-${memoryNamespaceAgentId}/self-evolution`,
    category,
    importance: insight.anchored ? 0.92 : 0.88,
    access_count: 0,
    last_access: null,
    metadata: {
      source_refs: [
        {
          ref_type: "message",
          ref_id: `${sessionKey}:assistant:${insight.messageIndex}`,
        },
      ],
      bracket_note: true,
      self_evolution: true,
      ...(userFacing ? { subject: "user", subject_aliases: ["Kyle"] } : {}),
      extracted_by: insight.anchored ? "agent_end_anchor_capture" : "agent_end_bracket_capture",
      insight_index: insightIndex,
    },
  };
}

function hasCaptureTrigger(messages: Array<{ content: string }>, keywords: string[]): boolean {
  if (keywords.length === 0) {
    return false;
  }
  const haystack = messages.map((message) => message.content).join("\n").toLowerCase();
  return keywords.some((keyword) => keyword.trim() && haystack.includes(keyword.toLowerCase()));
}

function formatJsonTextResult(value: unknown) {
  return textResult(JSON.stringify(value, null, 2));
}

type TodoItem = {
  content: string;
  status: "pending" | "in_progress" | "completed" | "cancelled";
  priority: "low" | "medium" | "high";
};

type RunAuditRecord = {
  startedAt: number;
  prompt?: string;
};

type AgentLikeContext = {
  agentId?: string;
  sessionKey?: string;
  sessionId?: string;
};

type EventLike = {
  conversationId?: string;
  sessionId?: string;
  sessionKey?: string;
  runId?: string;
  turnId?: string;
  success?: boolean;
  messages?: unknown[];
  prompt?: string;
  model?: string;
  provider?: string;
  usage?: unknown;
};

function sanitizeScopeKey(value: string): string {
  return value.replace(/[^a-zA-Z0-9._-]+/g, "_").slice(0, 120) || "default";
}

async function ensureParentDir(filePath: string): Promise<void> {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
}

async function appendJsonLine(filePath: string, payload: Record<string, unknown>): Promise<void> {
  await ensureParentDir(filePath);
  await fs.appendFile(
    filePath,
    `${JSON.stringify({ ts: new Date().toISOString(), ...payload })}\n`,
    "utf8",
  );
}

async function readJsonFile<T>(filePath: string, fallback: T): Promise<T> {
  try {
    return JSON.parse(await fs.readFile(filePath, "utf8")) as T;
  } catch {
    return fallback;
  }
}

async function writeJsonFile(filePath: string, payload: unknown): Promise<void> {
  await ensureParentDir(filePath);
  await fs.writeFile(filePath, JSON.stringify(payload, null, 2), "utf8");
}

function normalizeTodoItem(item: Partial<TodoItem>): TodoItem | null {
  const content = String(item.content || "").trim();
  if (!content) {
    return null;
  }
  const status =
    item.status === "in_progress" || item.status === "completed" || item.status === "cancelled"
      ? item.status
      : "pending";
  const priority = item.priority === "high" || item.priority === "low" ? item.priority : "medium";
  return { content, status, priority };
}

function formatTodoItems(items: TodoItem[]): string {
  if (items.length === 0) {
    return "No todo items.";
  }
  const icons: Record<TodoItem["status"], string> = {
    pending: "[ ]",
    in_progress: "[•]",
    completed: "[x]",
    cancelled: "[-]",
  };
  const priorities: Record<TodoItem["priority"], string> = {
    high: "🔴",
    medium: "🟡",
    low: "🟢",
  };
  return items
    .map((item) => `${icons[item.status]} ${priorities[item.priority]} ${item.content}`)
    .join("\n");
}

// ============================================================================
// Plugin Definition
// ============================================================================

export const memoryHybridBridgePlugin = {
  id: "tachi",
  name: "Memory Hybrid Bridge",
  kind: "memory" as const,
  description:
    "Advanced structured memory with LLM extraction and hybrid retrieval (vector/lexical/symbolic)",

  register(api: OpenClawPluginApi) {
    const runtimeApi = api;
    const config = bridgeConfigSchema.parse(api.pluginConfig);
    const configuredGlobalDbPath = resolveConfigPath(api, config.globalDbPath);
    const configuredDbPath = resolveConfigPath(api, config.dbPath);
    const pluginDataDir = path.dirname(configuredDbPath);
    const clientCache = new Map<string, Promise<MemoryMcpClient>>();
    const agentRuns = new Map<string, RunAuditRecord>();
    const subagentRuns = new Map<string, RunAuditRecord>();
    const spawnCounts = new Map<string, number>();
    let nativeMemoryCapabilityRegistered = false;

    function resolveAgentId(agentId?: string): string {
      return (agentId || "main").trim().toLowerCase() || "main";
    }

    function resolveMemoryAgentId(agentId?: string): string {
      const normalizedAgentId = resolveAgentId(agentId);
      return config.sharedMemoryAliases[normalizedAgentId] || normalizedAgentId;
    }

    function openClawPathRoot(agentId?: string): string {
      return `/openclaw/agent-${resolveMemoryAgentId(agentId)}`;
    }

    function resolveScope(context: AgentLikeContext | undefined, event?: EventLike): string {
      return (
        context?.sessionKey ||
        context?.sessionId ||
        event?.conversationId ||
        event?.sessionId ||
        event?.runId ||
        "default"
      );
    }

    function auditPaths(scope: string) {
      const safeScope = sanitizeScopeKey(scope);
      return {
        audit: config.auditLogPath,
        runAudit: path.resolve(pluginDataDir, "run-audit.jsonl"),
        usage: path.resolve(pluginDataDir, "usage-log.jsonl"),
        tooluse: path.resolve(pluginDataDir, "tooluse-log.jsonl"),
        compaction: path.resolve(pluginDataDir, "compaction-log.jsonl"),
        todo: path.resolve(pluginDataDir, "todos", `${safeScope}.json`),
      };
    }

    async function appendAudit(scope: string, payload: Record<string, unknown>) {
      await appendJsonLine(auditPaths(scope).audit, payload);
    }

    async function appendRunAudit(scope: string, payload: Record<string, unknown>) {
      await appendJsonLine(auditPaths(scope).runAudit, payload);
    }

    async function appendUsage(scope: string, payload: Record<string, unknown>) {
      await appendJsonLine(auditPaths(scope).usage, payload);
    }

    async function appendTooluse(scope: string, payload: Record<string, unknown>) {
      await appendJsonLine(auditPaths(scope).tooluse, payload);
    }

    async function appendCompaction(scope: string, payload: Record<string, unknown>) {
      await appendJsonLine(auditPaths(scope).compaction, payload);
    }

    async function readTodos(scope: string): Promise<TodoItem[]> {
      return await readJsonFile<TodoItem[]>(auditPaths(scope).todo, []);
    }

    async function writeTodos(scope: string, todos: TodoItem[]): Promise<void> {
      await writeJsonFile(auditPaths(scope).todo, todos);
    }

    function resolveAgentDbPath(agentId?: string): string {
      const normalizedAgentId = resolveMemoryAgentId(agentId);
      // Branch #7: prefer Tachi manifest assignment when present.
      const manifestHit = lookupAgentDbInManifest(normalizedAgentId);
      if (manifestHit) {
        return path.resolve(manifestHit);
      }
      const baseDir = path.dirname(configuredDbPath);
      const dbName = path.basename(configuredDbPath) || "memory.db";
      return path.resolve(baseDir, `agents/${normalizedAgentId}/${dbName}`);
    }

    function ensureClient(agentId?: string): Promise<MemoryMcpClient> {
      const dbPath = resolveAgentDbPath(agentId);
      let initClient = clientCache.get(dbPath);
      if (!initClient) {
        initClient = Promise.resolve(new MemoryMcpClient(configuredGlobalDbPath, dbPath, api.logger));
        clientCache.set(dbPath, initClient);
      }
      return initClient;
    }

    async function runWithClient<T>(
      operation: string,
      run: (client: MemoryMcpClient) => Promise<T>,
      agentId?: string,
    ): Promise<{ ok: true; value: T } | { ok: false; error: unknown }> {
      try {
        const client = await ensureClient(agentId);
        return { ok: true, value: await run(client) };
      } catch (error) {
        api.logger.warn(`tachi: ${operation} unavailable: ${String(error)}`);
        return { ok: false, error };
      }
    }

    async function emitTachiEvent(params: Record<string, unknown>, agentId?: string): Promise<unknown | null> {
      const result = await runWithClient(
        "tachi_event",
        async (client) => await client.tachiEvent(params),
        agentId,
      );
      return result.ok ? result.value : null;
    }

    async function performSearch(
      query: string,
      searchTopK?: number,
      agentId?: string,
    ): Promise<SearchResult> {
      const topK = searchTopK ?? config.topK;
      const result = await runWithClient("search_memory", async (client) => {
        const { docs, scores } = await client.searchMemory(query, undefined, {
          top_k: topK,
          weights: config.weights,
        });
        return docs.map((entry) => ({
          final_score: scores[entry.id] ?? 0,
          entry,
        }));
      }, agentId);

      if (!result.ok) {
        return { available: false, message: "Tachi MCP client unavailable." };
      }

      return { available: true, hits: result.value };
    }

    async function performRecall(query: string, agentId?: string) {
      const memoryAgentId = resolveMemoryAgentId(agentId);
      const result = await runWithClient("recall_context", async (client) =>
        await client.recallContext(query, {
          top_k: config.topK,
          candidate_multiplier: 1,
          agent_id: memoryAgentId,
          exclude_topics: ["imsg_conversation"],
        }),
        agentId,
      );

      return result.ok ? result.value : null;
    }

    function registerTachiPassthrough(
      openClawToolName: string,
      tachiToolName: string,
      description: string,
    ) {
      api.registerTool({
        name: openClawToolName,
        label: openClawToolName,
        description,
        parameters: Type.Object(
          {},
          {
            additionalProperties: true,
            description: "Arguments forwarded directly to the underlying Tachi MCP tool.",
          },
        ),
        async execute(_toolCallId, params, _signal, context) {
          const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
          const result = await runWithClient(
            tachiToolName,
            async (client) => await client.callTool(tachiToolName, (params as Record<string, unknown>) || {}),
            agentId,
          );

          return result.ok
            ? formatJsonTextResult(result.value)
            : textResult("Tachi MCP client unavailable.");
        },
      });
    }

    function formatSearchResults(result: SearchResult) {
      if (!result.available) {
        return textResult(result.message, {
          available: false,
          count: 0,
          results: [],
        });
      }

      if (result.hits.length === 0) {
        return textResult("No relevant memories found.", {
          available: true,
          count: 0,
          results: [],
        });
      }

      const results = result.hits.map((hit) => {
        const entry = hit.entry;
        return {
          path: `memory/${entry.id}`,
          startLine: 1,
          endLine: 1,
          score: hit.final_score,
          snippet: [
            `[${entry.topic}] ${entry.text}`,
            `Keywords: ${entry.keywords.join(", ")}`,
            `Persons: ${entry.persons.join(", ")}`,
            entry.entities.length ? `Entities: ${entry.entities.join(", ")}` : "",
            `Timestamp: ${entry.timestamp}`,
          ]
            .filter(Boolean)
            .join("\n"),
        };
      });

      return textResult(JSON.stringify({ results }), {
        available: true,
        count: result.hits.length,
        results,
      });
    }

    api.logger.info("tachi: registered (MCP compatibility mode)");
    nativeMemoryCapabilityRegistered = registerNativeTachiMemoryCapability({
      api,
      topK: config.topK,
      ensureClient,
      resolveAgentId,
    });

    // ========================================================================
    // Tools — compatibility wrappers that forward directly to Tachi MCP.
    // ========================================================================

    api.registerTool({
      name: "memory_search",
      label: "Memory Search",
      description:
        "Mandatory recall step: semantically search long-term structured memory before answering questions about prior work, decisions, dates, people, preferences, or todos; returns top snippets with relevance scores.",
      parameters: Type.Object({
        query: Type.String({ description: "Natural language search query" }),
        maxResults: Type.Optional(Type.Number({ description: "Max results (default: 6)" })),
        minScore: Type.Optional(Type.Number({ description: "Min score threshold (default: 0)" })),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const { query, maxResults, minScore } = params as {
          query: string;
          maxResults?: number;
          minScore?: number;
        };

        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const result = await performSearch(query, maxResults ?? config.topK, agentId);
        if (!result.available) {
          return formatSearchResults(result);
        }

        const hits =
          typeof minScore === "number" && minScore > 0
            ? result.hits.filter((hit) => hit.final_score >= minScore)
            : result.hits;

        return formatSearchResults({ available: true, hits });
      },
    });

    api.registerTool({
      name: "memory_get",
      label: "Memory Get",
      description:
        "Retrieve a specific memory entry by id; use after memory_search to get full details.",
      parameters: Type.Object({
        path: Type.String({
          description: "Entry id (e.g. memory/m_1234) or raw id (m_1234)",
        }),
        from: Type.Optional(Type.Number({ description: "Ignored (compat)" })),
        lines: Type.Optional(Type.Number({ description: "Ignored (compat)" })),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const rawPath = (params as { path: string }).path;
        const entryId = rawPath.replace(/^(?:shadow-store|memory)\//, "");
        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const result = await runWithClient(
          "get_memory",
          async (client) => await client.getMemory(entryId),
          agentId,
        );

        if (!result.ok) {
          return textResult("Tachi MCP client unavailable.", {
            available: false,
            found: false,
          });
        }

        const found = result.value;
        if (!found) {
          return textResult(
            JSON.stringify({
              path: rawPath,
              text: "",
              error: `Memory entry not found: ${entryId}`,
            }),
            { available: true, found: false },
          );
        }

        const text = [
          `ID: ${found.id}`,
          `Topic: ${found.topic}`,
          `Timestamp: ${found.timestamp}`,
          `Fact: ${found.text}`,
          `Keywords: ${found.keywords.join(", ")}`,
          `Persons: ${found.persons.join(", ")}`,
          `Entities: ${found.entities.join(", ")}`,
        ].join("\n");

        return textResult(JSON.stringify({ path: rawPath, text }), {
          available: true,
          found: true,
        });
      },
    });

    api.registerTool({
      name: "memory_runtime_info",
      label: "Memory Runtime Info",
      description:
        "Return the verified Tachi runtime identity and DB routing for this OpenClaw agent bridge.",
      parameters: Type.Object({}),
      async execute(_toolCallId, _params, _signal, context) {
        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const result = await runWithClient(
          "runtime_info",
          async (client) => await client.runtimeInfoPayload(),
          agentId,
        );
        return result.ok
          ? formatJsonTextResult({
              ...result.value,
              openclaw_bridge: {
                version: "1.6.2",
                adapter: "openclaw",
                native_memory_capability: nativeMemoryCapabilityRegistered,
                continuity_board: true,
                role: resolveHostRole(agentId),
                memory_agent_id: resolveMemoryAgentId(agentId),
                agent_db_path: resolveAgentDbPath(agentId),
              },
            })
          : textResult("Tachi MCP client unavailable.");
      },
    });

    api.registerTool({
      name: "continuity_board",
      label: "Continuity Board",
      description:
        "Read the Tachi continuity board for current parallel work lanes, owners, blockers, artifacts, and handoff context.",
      parameters: Type.Object({
        project: Type.Optional(Type.String({ description: "Named Tachi project DB selector" })),
        sessionId: Type.Optional(Type.String({ description: "Optional session/run filter" })),
        limit: Type.Optional(Type.Number({ description: "Maximum projected rows/events to return" })),
        writeFeedback: Type.Optional(Type.Boolean({ description: "Record pattern-seen feedback (default false)" })),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const payload = params as {
          project?: string;
          sessionId?: string;
          limit?: number;
          writeFeedback?: boolean;
        };
        const board = await loadContinuityBoard(
          (eventParams) => emitTachiEvent(eventParams, agentId),
          {
            project: payload.project,
            sessionId: payload.sessionId,
            limit: payload.limit,
            readOnly: payload.writeFeedback !== true,
          },
        );
        return formatJsonTextResult(board);
      },
    });

    api.registerTool({
      name: "memory_save",
      label: "Memory Save",
      description: "Save a durable memory into Tachi for future recall.",
      parameters: Type.Object({
        text: Type.String({ description: "Memory text content" }),
        summary: Type.Optional(Type.String({ description: "Optional short summary" })),
        topic: Type.Optional(Type.String({ description: "Memory topic" })),
        path: Type.Optional(Type.String({ description: "Hierarchical path (defaults to agent root)" })),
        importance: Type.Optional(Type.Number({ description: "0.0-1.0 importance score" })),
        keywords: Type.Optional(Type.Array(Type.String(), { description: "Keyword tags" })),
        category: Type.Optional(
          Type.String({ description: "Category: fact | decision | preference | entity | other" }),
        ),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const { text: rawText, summary: rawSummary, topic, path: memoryPath, importance, keywords, category } = params as {
          text: string;
          summary?: string;
          topic?: string;
          path?: string;
          importance?: number;
          keywords?: string[];
          category?: string;
        };
        const text = stripThinkBlocks(rawText);
        const summary = rawSummary != null ? stripThinkBlocks(rawSummary) : undefined;
        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const result = await runWithClient("save_memory", async (client) => {
          await client.saveMemory({
            id: makeMemoryId(),
            text,
            summary: summary ?? text.slice(0, 96),
            path: memoryPath || openClawPathRoot(agentId),
            importance: importance ?? 0.7,
            category: (category || "fact") as MemoryEntry["category"],
            topic: topic || "manual_memory",
            keywords: keywords || [],
            persons: [],
            entities: [],
            location: "",
            timestamp: new Date().toISOString(),
            scope: "project",
            access_count: 0,
            last_access: null,
            metadata: { source_refs: [] },
          });
          return { ok: true };
        }, agentId);

        return result.ok
          ? textResult(JSON.stringify(result.value))
          : textResult("Tachi MCP client unavailable.");
      },
    });

    api.registerTool({
      name: "memory_graph",
      label: "Memory Graph",
      description: "Inspect a read-only neighborhood in Tachi's memory graph by memory id or query.",
      parameters: Type.Object({
        memory_id: Type.Optional(Type.String({ description: "Seed memory id" })),
        query: Type.Optional(Type.String({ description: "Natural language graph lookup query" })),
        top_k: Type.Optional(Type.Number({ description: "Query seed count (default: 5)" })),
        depth: Type.Optional(Type.Number({ description: "Traversal depth (default: 1)" })),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const { memory_id, query, top_k, depth } = params as {
          memory_id?: string;
          query?: string;
          top_k?: number;
          depth?: number;
        };
        const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
        const result = await runWithClient(
          "memory_graph",
          async (client) =>
            await client.memoryGraph({
              memory_id,
              query,
              top_k,
              depth,
            }),
          agentId,
        );

        return result.ok
          ? textResult(JSON.stringify(result.value))
          : textResult("Tachi MCP client unavailable.");
      },
    });

    if (config.exposeExperimentalTachiTools) {
      api.registerTool({
        name: "memory_delete",
        label: "Memory Delete",
        description: "Delete a specific memory entry by id from Tachi.",
        parameters: Type.Object({
          path: Type.String({
            description: "Entry id (e.g. memory/m_1234) or raw id (m_1234)",
          }),
        }),
        async execute(_toolCallId, params, _signal, context) {
          const rawPath = (params as { path: string }).path;
          const entryId = rawPath.replace(/^(?:shadow-store|memory)\//, "");
          const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
          const result = await runWithClient(
            "memory_delete",
            async (client) => await client.deleteMemory(entryId),
            agentId,
          );

          return result.ok
            ? formatJsonTextResult({ deleted: result.value, id: entryId })
            : textResult("Tachi MCP client unavailable.");
        },
      });

      api.registerTool({
        name: "compact_context",
        label: "Compact Context",
        description:
          "Compact the current session window via Tachi MCP and return a reusable summary block.",
        parameters: Type.Object({
          conversation_id: Type.String({ description: "Conversation identifier" }),
          window_id: Type.String({ description: "Compaction window identifier" }),
          messages: Type.Array(
            Type.Object({
              role: Type.String(),
              content: Type.String(),
            }),
            { description: "Recent messages to compact" },
          ),
          trigger: Type.Optional(Type.String()),
          current_summary: Type.Optional(Type.String()),
          path_prefix: Type.Optional(Type.String()),
          target_tokens: Type.Optional(Type.Number()),
          max_output_tokens: Type.Optional(Type.Number()),
          persist: Type.Optional(Type.Boolean()),
        }),
        async execute(_toolCallId, params, _signal, context) {
          const agentId = resolveAgentId((context as AgentLikeContext | undefined)?.agentId);
          const payload = params as {
            conversation_id: string;
            window_id: string;
            messages: Array<{ role: string; content: string }>;
            trigger?: string;
            current_summary?: string;
            path_prefix?: string;
            target_tokens?: number;
            max_output_tokens?: number;
            persist?: boolean;
          };
          const result = await runWithClient(
            "compact_context",
            async (client) =>
              await client.compactContext({
                agent_id: agentId,
                conversation_id: payload.conversation_id,
                window_id: payload.window_id,
                messages: payload.messages,
                trigger: payload.trigger,
                current_summary: payload.current_summary,
                path_prefix: payload.path_prefix,
                target_tokens: payload.target_tokens,
                max_output_tokens: payload.max_output_tokens,
                persist: payload.persist,
              }),
            agentId,
          );

          return result.ok
            ? formatJsonTextResult(result.value)
            : textResult("Tachi MCP client unavailable.");
        },
      });

      registerTachiPassthrough(
        "tachi_vault_store",
        "vault_set",
        "Store a secret in the Tachi vault.",
      );
      registerTachiPassthrough(
        "tachi_vault_retrieve",
        "vault_get",
        "Retrieve a secret from the Tachi vault.",
      );
      registerTachiPassthrough(
        "tachi_vault_list",
        "vault_list",
        "List secrets in the Tachi vault.",
      );
      // NOTE: `tachi_ghost_whisper` / `tachi_ghost_listen` used to forward to
      // the server-side `ghost_publish` / `ghost_subscribe` tools, but those
      // were removed when the standard tool surface shipped. Leaving the
      // passthroughs registered here made the OpenClaw bridge advertise
      // tools that always returned `tool not found`. Remove them — agents
      // that want Ghost-style whispers should coordinate via memory / handoff
      // instead.
      registerTachiPassthrough(
        "tachi_kanban_add",
        "post_card",
        "Create a kanban card in Tachi.",
      );
      registerTachiPassthrough(
        "tachi_kanban_update",
        "update_card",
        "Update a kanban card in Tachi.",
      );
      registerTachiPassthrough(
        "tachi_kanban_list",
        "check_inbox",
        "List kanban cards from a Tachi inbox.",
      );
      registerTachiPassthrough(
        "tachi_create_handoff",
        "handoff_leave",
        "Create a Tachi handoff memo.",
      );
      registerTachiPassthrough(
        "tachi_get_handoff",
        "handoff_check",
        "Read pending Tachi handoff memos.",
      );
      registerTachiPassthrough(
        "tachi_run_skill",
        "run_skill",
        "Run a Tachi skill.",
      );
      registerTachiPassthrough(
        "tachi_hub_discover",
        "hub_discover",
        "Discover available Tachi hub capabilities.",
      );
      registerTachiPassthrough(
        "tachi_recommend_toolchain",
        "recommend_toolchain",
        "Recommend a Tachi toolchain for the current task.",
      );
    }

    api.registerTool({
      name: "todo_write",
      label: "Todo Write",
      description: "Write or replace the current session todo list.",
      parameters: Type.Object({
        todos: Type.Array(
          Type.Object({
            content: Type.String(),
            status: Type.Optional(
              Type.Union([
                Type.Literal("pending"),
                Type.Literal("in_progress"),
                Type.Literal("completed"),
                Type.Literal("cancelled"),
              ]),
            ),
            priority: Type.Optional(
              Type.Union([Type.Literal("low"), Type.Literal("medium"), Type.Literal("high")]),
            ),
          }),
        ),
      }),
      async execute(_toolCallId, params, _signal, context) {
        const scope = resolveScope(context as AgentLikeContext | undefined);
        const todos = ((params as { todos: Partial<TodoItem>[] }).todos || [])
          .map(normalizeTodoItem)
          .filter((item): item is TodoItem => Boolean(item));
        await writeTodos(scope, todos);
        return formatJsonTextResult({ scope, count: todos.length });
      },
    });

    api.registerTool({
      name: "todo_read",
      label: "Todo Read",
      description: "Read the current session todo list.",
      parameters: Type.Object({}),
      async execute(_toolCallId, _params, _signal, context) {
        const scope = resolveScope(context as AgentLikeContext | undefined);
        const todos = await readTodos(scope);
        return textResult(formatTodoItems(todos), { scope, count: todos.length, todos });
      },
    });

    api.registerTool({
      name: "todo_spawn_summary",
      label: "Todo Spawn Summary",
      description: "Show how many subagent spawns were recorded for the current session.",
      parameters: Type.Object({}),
      async execute(_toolCallId, _params, _signal, context) {
        const scope = resolveScope(context as AgentLikeContext | undefined);
        return formatJsonTextResult({
          scope,
          spawnCount: spawnCounts.get(scope) || 0,
        });
      },
    });

    api.on("before_prompt_build", async (event: EventLike, context: AgentLikeContext) => {
      const query = event.prompt;
      const scope = resolveScope(context, event);
      const agentId = resolveAgentId(context?.agentId);
      const key = `${agentId}:${scope}`;
      agentRuns.set(key, { startedAt: Date.now(), prompt: query });
      await appendRunAudit(scope, {
        type: "before_prompt_build",
        agentId,
        sessionKey: context?.sessionKey || null,
        sessionId: context?.sessionId || null,
        prompt: typeof query === "string" ? query.slice(0, 400) : null,
      });
      await emitHostContinuityEvent(api, (params) => emitTachiEvent(params, agentId), {
        eventType: "host.prompt_build",
        actor: agentId,
        sessionId: context?.sessionId || context?.sessionKey || null,
        runId: event?.runId || null,
        role: resolveHostRole(agentId),
        status: "working",
        goal: typeof query === "string" ? query.slice(0, 240) : null,
        currentStep: "before_prompt_build",
        nextAction: "run_turn",
        payload: {
          session_key: context?.sessionKey || null,
          prompt_chars: typeof query === "string" ? query.length : null,
        },
      });
      if (!query || query.length < 5) {
        return;
      }

      const recall = await performRecall(query, agentId);
      if (recall?.prependContext.trim()) {
        return { prependContext: recall.prependContext };
      }
    });

    runtimeApi.on("llm_input", async (event: EventLike, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      await appendUsage(scope, {
        type: "llm_input",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        runId: event?.runId || null,
        model: event?.model || null,
        provider: event?.provider || null,
      });
    });

    runtimeApi.on("llm_output", async (event: EventLike, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      await appendUsage(scope, {
        type: "llm_output",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        runId: event?.runId || null,
        model: event?.model || null,
        provider: event?.provider || null,
        usage: event?.usage || null,
      });
    });

    runtimeApi.on("after_tool_call", async (event: EventLike & { toolName?: string; name?: string; success?: boolean }, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      const toolName = event?.toolName || event?.name || "unknown";
      await appendTooluse(scope, {
        type: "after_tool_call",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        toolName,
        success: event?.success ?? null,
      });
      if (toolName === "sessions_spawn" || toolName === "subagents") {
        spawnCounts.set(scope, (spawnCounts.get(scope) || 0) + 1);
      }
    });

    runtimeApi.on("before_compaction", async (event: EventLike & { window_id?: string; windowId?: string; messages?: unknown[] }, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      await appendCompaction(scope, {
        type: "before_compaction",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        windowId: event?.window_id || event?.windowId || null,
        messageCount: Array.isArray(event?.messages) ? event.messages.length : null,
      });
    });

    runtimeApi.on("after_compaction", async (event: EventLike & { window_id?: string; windowId?: string; compacted_text?: string; estimated_tokens?: number }, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      await appendCompaction(scope, {
        type: "after_compaction",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        windowId: event?.window_id || event?.windowId || null,
        compactedTextLength:
          typeof event?.compacted_text === "string" ? event.compacted_text.length : null,
        estimatedTokens: event?.estimated_tokens ?? null,
      });
    });

    runtimeApi.on("subagent_spawned", async (event: EventLike & { childSessionKey?: string; id?: string; label?: string }, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      const childKey = String(event?.childSessionKey || event?.sessionKey || event?.id || Date.now());
      subagentRuns.set(childKey, { startedAt: Date.now() });
      spawnCounts.set(scope, (spawnCounts.get(scope) || 0) + 1);
      await appendRunAudit(scope, {
        type: "subagent_spawned",
        agentId: resolveAgentId(context?.agentId),
        childSessionKey: childKey,
        label: event?.label || null,
        sessionKey: context?.sessionKey || null,
      });
      await emitHostContinuityEvent(api, (params) => emitTachiEvent(params, resolveAgentId(context?.agentId)), {
        eventType: "host.subagent_spawned",
        actor: resolveAgentId(context?.agentId),
        sessionId: context?.sessionId || context?.sessionKey || null,
        runId: event?.runId || null,
        role: resolveHostRole(resolveAgentId(context?.agentId)),
        status: "working",
        currentStep: "subagent_spawned",
        nextAction: "monitor_subagent",
        payload: {
          child_session_key: childKey,
          label: event?.label || null,
        },
      });
    });

    runtimeApi.on("subagent_ended", async (event: EventLike & { childSessionKey?: string; id?: string; outcome?: string }, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      const childKey = String(event?.childSessionKey || event?.sessionKey || event?.id || "");
      const started = childKey ? subagentRuns.get(childKey) : undefined;
      if (childKey) {
        subagentRuns.delete(childKey);
      }
      await appendRunAudit(scope, {
        type: "subagent_ended",
        agentId: resolveAgentId(context?.agentId),
        childSessionKey: childKey || null,
        durationMs: started ? Math.max(0, Date.now() - started.startedAt) : null,
        outcome: event?.outcome || null,
        sessionKey: context?.sessionKey || null,
      });
      await emitHostContinuityEvent(api, (params) => emitTachiEvent(params, resolveAgentId(context?.agentId)), {
        eventType: "host.subagent_ended",
        actor: resolveAgentId(context?.agentId),
        sessionId: context?.sessionId || context?.sessionKey || null,
        runId: event?.runId || null,
        role: resolveHostRole(resolveAgentId(context?.agentId)),
        status: event?.outcome === "success" ? "done" : "waiting",
        currentStep: "subagent_ended",
        nextAction: "review_subagent_evidence",
        summary: event?.outcome || null,
        payload: {
          child_session_key: childKey || null,
          duration_ms: started ? Math.max(0, Date.now() - started.startedAt) : null,
          outcome: event?.outcome || null,
        },
      });
    });

    async function finalizeAgentRun(
      scope: string,
      agentId: string,
      context: AgentLikeContext,
      success: boolean,
      captured: unknown,
    ) {
      const key = `${agentId}:${scope}`;
      const started = agentRuns.get(key);
      agentRuns.delete(key);
      await appendRunAudit(scope, {
        type: "agent_end",
        agentId,
        sessionKey: context?.sessionKey || null,
        success,
        durationMs: started ? Math.max(0, Date.now() - started.startedAt) : null,
        captured,
      });
      await emitHostContinuityEvent(api, (params) => emitTachiEvent(params, agentId), {
        eventType: "host.agent_end",
        actor: agentId,
        sessionId: context?.sessionId || context?.sessionKey || null,
        role: resolveHostRole(agentId),
        status: success ? "done" : "blocked",
        currentStep: "agent_end",
        nextAction: success ? "review_or_dispatch_next_lane" : "inspect_failure",
        blocker: success ? null : "agent_end reported unsuccessful run",
        payload: {
          success,
          duration_ms: started ? Math.max(0, Date.now() - started.startedAt) : null,
          captured,
        },
      });
    }

    api.on("agent_end", async (event: EventLike, context: AgentLikeContext) => {
      const agentId = resolveAgentId(context?.agentId);
      const memoryAgentId = resolveMemoryAgentId(agentId);
      const scope = resolveScope(context, event);

      if (!event?.success) {
        await finalizeAgentRun(scope, agentId, context, Boolean(event?.success), null);
        return;
      }

      const sessionId = event?.sessionId || context?.sessionId || event?.conversationId || event?.runId || null;
      const sessionKey = context?.sessionKey || event?.sessionKey || null;
      const eventMessages = Array.isArray(event?.messages) ? event.messages : [];
      const fallbackMessages = eventMessages.length > 0 ? [] : await readSessionMessages(agentId, sessionId, sessionKey);
      const captureMessages = eventMessages.length > 0 ? eventMessages : fallbackMessages;
      if (captureMessages.length === 0) {
        await finalizeAgentRun(scope, agentId, context, Boolean(event?.success), {
          status: "skipped",
          reason: eventMessages.length === 0 ? "event_messages_missing_and_session_fallback_empty" : "empty_messages",
          captured: 0,
          source: eventMessages.length === 0 ? "session_fallback" : "event_messages",
          sessionId,
          sessionKey,
        });
        return;
      }
      if (eventMessages.length === 0) {
        await appendRunAudit(scope, {
          type: "capture_fallback_used",
          agentId,
          sessionKey,
          sessionId,
          messages: captureMessages.length,
        });
      }

      const conversationId =
        context?.sessionKey || event?.conversationId || event?.sessionId || `openclaw:${agentId}`;
      const turnId = event?.turnId || event?.runId || `agent_end:${Date.now()}`;

      const selfEvolutionAgents = new Set(config.selfEvolutionAgents.map((value) => value.toLowerCase()));
      if (selfEvolutionAgents.has(agentId.toLowerCase())) {
        const insights = extractSelfEvolutionInsights(captureMessages);
        if (insights.length > 0) {
          let saved = 0;
          for (const [insightIndex, insight] of insights.entries()) {
            const result = await runWithClient(
              "save_memory",
              async (client) => {
                await client.saveMemory(
                  buildSelfEvolutionMemory(
                    agentId,
                    memoryAgentId,
                    conversationId,
                    insight,
                    insightIndex,
                    new Date().toISOString(),
                  ),
                );
                return { ok: true };
              },
              agentId,
            );
            if (result.ok) {
              saved += 1;
            }
          }
          if (saved > 0) {
            await appendAudit(scope, {
              type: "self_evolution_capture",
              agentId,
              saved,
              conversationId,
            });
            api.logger.info(`tachi: saved ${saved} self-evolution notes for ${agentId}`);
          }
        }
      }

      const recentMessages = captureMessages
        .slice(-8)
        .map((message: any) => ({
          role: typeof message?.role === "string" ? message.role : "unknown",
          content: messageToText(message),
        }))
        .filter((message) => message.content.trim().length > 0);

      const combinedChars = recentMessages.reduce(
        (total, message) => total + message.content.length,
        0,
      );
      const hasKeywordTrigger = hasCaptureTrigger(recentMessages, config.captureTriggerKeywords);
      if (recentMessages.length === 0 || (combinedChars < config.captureMinChars && !hasKeywordTrigger)) {
        await finalizeAgentRun(scope, agentId, context, Boolean(event?.success), {
          status: "skipped",
          reason: recentMessages.length === 0 ? "empty_messages" : "below_capture_threshold",
          captured: 0,
        });
        return;
      }

      const result = await runWithClient(
        "capture_session",
        async (client) =>
          await client.captureSession({
            conversation_id: conversationId,
            turn_id: turnId,
            agent_id: memoryAgentId,
            messages: recentMessages,
            path_prefix: openClawPathRoot(agentId),
            scope: "project",
          }),
        agentId,
      );

      if (!result.ok) {
        api.logger.warn("tachi: capture_session skipped in degraded mode");
      }

      await finalizeAgentRun(scope, agentId, context, Boolean(event?.success), result.ok ? result.value : null);
    });

    runtimeApi.on("session_end", async (event: EventLike, context: AgentLikeContext) => {
      const scope = resolveScope(context, event);
      await appendRunAudit(scope, {
        type: "session_end",
        agentId: resolveAgentId(context?.agentId),
        sessionKey: context?.sessionKey || null,
        sessionId: context?.sessionId || null,
      });
      const agentId = resolveAgentId(context?.agentId);
      await emitHostContinuityEvent(api, (params) => emitTachiEvent(params, agentId), {
        eventType: "host.session_end",
        actor: agentId,
        sessionId: context?.sessionId || context?.sessionKey || event?.sessionId || null,
        runId: event?.runId || null,
        role: resolveHostRole(agentId),
        status: "waiting",
        currentStep: "session_end",
        nextAction: "handoff_if_unfinished",
        payload: {
          session_key: context?.sessionKey || null,
        },
      });
    });

    api.registerService({
      id: "tachi",
      start: () => api.logger.info("tachi: service started"),
      stop: async () => {
        api.logger.info("tachi: shutting down...");
        const clientPromises = Array.from(clientCache.values());
        clientCache.clear();
        for (const clientPromise of clientPromises) {
          try {
            const client = await clientPromise;
            await client.close();
          } catch {
            // Client never initialized successfully.
          }
        }
        api.logger.info("tachi: service stopped");
      },
    });
  },
};

export default memoryHybridBridgePlugin;
