const MCP_PROTOCOL_VERSION = '2024-11-05';
const REQUEST_TIMEOUT_MS = 5000;
export const DEFAULT_TACHI_DAEMON_PORT = 6919;
export const DEFAULT_TACHI_DAEMON_HOST = `localhost:${DEFAULT_TACHI_DAEMON_PORT}/mcp`;

type JsonRpcId = number;

interface JsonRpcErrorShape {
  code: number;
  message: string;
  data?: unknown;
}

interface JsonRpcResponseShape {
  jsonrpc: '2.0';
  id: JsonRpcId;
  result?: unknown;
  error?: JsonRpcErrorShape;
}

interface JsonRpcRequestShape {
  jsonrpc: '2.0';
  id?: JsonRpcId;
  method: string;
  params?: unknown;
}

interface ToolTextContent {
  type: string;
  text?: string;
}

interface ToolCallEnvelope {
  isError?: boolean;
  content?: ToolTextContent[];
  structuredContent?: unknown;
}

interface SessionState {
  initialized: boolean;
  sessionId?: string;
}

export interface HubCapability {
  id: string;
  cap_type: string;
  name: string;
  description?: string;
  enabled?: boolean;
  definition?: string;
  visibility?: string;
  version?: number;
  db?: string;
  uses?: number;
  successes?: number;
  failures?: number;
  avg_rating?: number;
  last_used?: string | null;
  created_at?: string;
  updated_at?: string;
  [key: string]: unknown;
}

export interface AuditLogEntry {
  id?: string;
  timestamp?: string;
  server_id?: string;
  tool_name?: string;
  status?: string;
  duration_ms?: number;
  error_category?: string | null;
  [key: string]: unknown;
}

export interface MemoryEntry {
  id: string;
  summary?: string;
  text?: string;
  path?: string;
  scope?: string;
  category?: string;
  score?: number;
  timestamp?: string;
  metadata?: Record<string, unknown>;
  entities?: string[];
  [key: string]: unknown;
}

export interface GcStats {
  global?: Record<string, unknown>;
  project?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface GhostTopic {
  topic: string;
  message_count?: number;
  count?: number;
  last_message_at?: string;
  [key: string]: unknown;
}

export interface GhostTopicSnapshot {
  active_topics: number;
  topics: GhostTopic[];
}

export class TachiOfflineError extends Error {
  readonly causeError?: unknown;

  constructor(message: string, causeError?: unknown) {
    super(message);
    this.name = 'TachiOfflineError';
    this.causeError = causeError;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function parseJsonMaybe(value: string): unknown {
  const trimmed = value.trim();
  if (!trimmed) {
    return '';
  }

  try {
    return JSON.parse(trimmed);
  } catch {
    return value;
  }
}

function toStringOrNull(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function toNumberOrNull(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function uniqueUrls(values: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const value of values) {
    const normalized = value.trim().replace(/\/+$/, '');
    if (!normalized || seen.has(normalized)) {
      continue;
    }
    seen.add(normalized);
    result.push(normalized);
  }
  return result;
}

function configuredBaseUrls(): string[] {
  const configured = (import.meta.env.VITE_TACHI_BASE_URL ?? '')
    .split(',')
    .map((part: string) => part.trim())
    .filter(Boolean);

  return uniqueUrls([
    ...configured,
    '/tachi/mcp',
    `http://127.0.0.1:${DEFAULT_TACHI_DAEMON_PORT}/mcp`,
    `http://localhost:${DEFAULT_TACHI_DAEMON_PORT}/mcp`,
  ]);
}

function normalizeJsonRpcMessages(payload: unknown): JsonRpcResponseShape[] {
  if (Array.isArray(payload)) {
    return payload.flatMap((item) => normalizeJsonRpcMessages(item));
  }

  if (isRecord(payload) && payload.jsonrpc === '2.0') {
    return [payload as unknown as JsonRpcResponseShape];
  }

  return [];
}

function parseSseMessages(raw: string): JsonRpcResponseShape[] {
  const dataLines = raw
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).trim())
    .filter((line) => line && line !== '[DONE]');

  const parsed = dataLines.map((line) => parseJsonMaybe(line));
  return normalizeJsonRpcMessages(parsed);
}

function parseHttpMessages(raw: string, contentType: string | null): JsonRpcResponseShape[] {
  const trimmed = raw.trim();
  if (!trimmed) {
    return [];
  }

  if (contentType?.includes('application/json')) {
    return normalizeJsonRpcMessages(parseJsonMaybe(trimmed));
  }

  if (contentType?.includes('text/event-stream') || trimmed.includes('\ndata:')) {
    const parsedFromSse = parseSseMessages(trimmed);
    if (parsedFromSse.length > 0) {
      return parsedFromSse;
    }
  }

  const parsedJson = normalizeJsonRpcMessages(parseJsonMaybe(trimmed));
  if (parsedJson.length > 0) {
    return parsedJson;
  }

  const lineParsed = trimmed
    .split('\n')
    .map((line) => normalizeJsonRpcMessages(parseJsonMaybe(line)))
    .flat();

  return lineParsed;
}

function extractSessionId(headers: Headers): string | undefined {
  return (
    headers.get('Mcp-Session-Id') ??
    headers.get('mcp-session-id') ??
    headers.get('MCP-Session-Id') ??
    undefined
  );
}

function parseToolCallResult(envelope: unknown): unknown {
  if (!isRecord(envelope)) {
    return envelope;
  }

  const result = envelope as ToolCallEnvelope;

  if (result.structuredContent !== undefined) {
    return result.structuredContent;
  }

  const content = Array.isArray(result.content) ? result.content : [];
  const textItems = content
    .filter((item): item is ToolTextContent => isRecord(item) && typeof item.type === 'string')
    .map((item) => item.text)
    .filter((text): text is string => typeof text === 'string');

  const parsed =
    textItems.length <= 1 ? parseJsonMaybe(textItems[0] ?? '') : textItems.map((text) => parseJsonMaybe(text));

  if (result.isError) {
    const message =
      typeof parsed === 'string'
        ? parsed
        : isRecord(parsed) && typeof parsed.error === 'string'
          ? parsed.error
          : 'Tool call returned an error';
    throw new Error(message);
  }

  return parsed;
}

function ensureArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

export function isTachiOfflineError(error: unknown): error is TachiOfflineError {
  return error instanceof TachiOfflineError;
}

export function getApiErrorMessage(error: unknown): string {
  if (error instanceof TachiOfflineError) {
    return 'Tachi daemon offline';
  }
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return 'Unknown API error';
}

class TachiApi {
  private readonly baseUrls: string[];
  private readonly sessionByBase = new Map<string, SessionState>();
  private activeBaseUrl: string | null = null;
  private nextRequestIdValue = 1;

  constructor(baseUrls = configuredBaseUrls()) {
    this.baseUrls = baseUrls;
  }

  async ping(): Promise<void> {
    // `vault_status` is a cheap, read-only tool exposed by the default
    // `standard` tool profile. Previously we called `memory_stats` which
    // only exists in `admin`, so the desktop app reported the daemon as
    // offline on any out-of-the-box install.
    await this.callTool('vault_status', {});
  }

  async fetchHubCapabilities(): Promise<HubCapability[]> {
    // `hub_discover` is admin-only since the standard profile collapsed
    // onto facades. `tachi_skill action=discover` is the standard-visible
    // replacement and returns compact capability records.
    const payload = await this.callToolSoft(
      'tachi_skill',
      { action: 'discover', enabled_only: false, limit: 200 },
      [] as HubCapability[],
    );
    if (Array.isArray(payload)) {
      return payload.filter(isRecord).map((capability) => capability as HubCapability);
    }
    // Some facade versions wrap the list under `capabilities` / `skills`.
    if (isRecord(payload)) {
      for (const key of ['capabilities', 'skills', 'results']) {
        const inner = (payload as Record<string, unknown>)[key];
        if (Array.isArray(inner)) {
          return inner.filter(isRecord).map((capability) => capability as HubCapability);
        }
      }
    }
    return [];
  }

  async fetchRecentAuditLogs(limit = 30): Promise<AuditLogEntry[]> {
    // `tachi_audit_log` is admin-only and deliberately hidden from the
    // default standard profile. Keep the method so the dashboard can
    // still render the panel, but fall back to an empty list instead of
    // crashing the whole Hub screen when the tool isn't exposed.
    const payload = await this.callToolSoft('tachi_audit_log', { limit }, [] as AuditLogEntry[]);
    const entries = ensureArray<unknown>(payload).filter(isRecord);
    return entries.map((entry) => entry as AuditLogEntry);
  }

  async searchMemory(query: string, topK = 8): Promise<MemoryEntry[]> {
    // `search_memory` is not in the standard allow-list; `tachi_search`
    // is the unified facade that replaces it.
    const payload = await this.callTool('tachi_search', {
      query,
      top_k: topK,
      scope: 'all',
      include_archived: false,
    });

    // The facade can return either `[hits]` or `{ hits: [...] }` shapes
    // depending on the version. Normalize.
    const rows: unknown[] = Array.isArray(payload)
      ? payload
      : isRecord(payload) && Array.isArray((payload as Record<string, unknown>).hits)
        ? ((payload as Record<string, unknown>).hits as unknown[])
        : [];
    return rows.filter(isRecord).map((entry) => entry as MemoryEntry);
  }

  async getGcStats(): Promise<GcStats> {
    // `memory_gc` is admin-only. Soft-degrade so HubDashboard still
    // renders with the "No GC stats yet." placeholder rather than
    // failing its entire Promise.all.
    const payload = await this.callToolSoft('memory_gc', {}, {} as GcStats);
    if (!isRecord(payload)) {
      return {};
    }
    return payload as GcStats;
  }

  async fetchGhostTopics(): Promise<GhostTopicSnapshot> {
    // `ghost_topics` was removed server-side when the standard profile
    // shipped. Keep the method as a no-op so the Ghost Whispers view
    // renders an empty state instead of throwing.
    const payload = await this.callToolSoft(
      'ghost_topics',
      {},
      { active_topics: 0, topics: [] } as GhostTopicSnapshot,
    );
    if (!isRecord(payload)) {
      return { active_topics: 0, topics: [] };
    }

    const topics = ensureArray<unknown>(payload.topics).filter(isRecord).map((topic) => {
      const topicName = toStringOrNull(topic.topic) ?? 'unknown-topic';
      const messageCount = toNumberOrNull(topic.message_count);
      const fallbackCount = toNumberOrNull(topic.count);
      const lastMessageAt = toStringOrNull(topic.last_message_at);
      return {
        ...topic,
        topic: topicName,
        message_count: messageCount ?? fallbackCount ?? undefined,
        last_message_at: lastMessageAt ?? undefined,
      } as GhostTopic;
    });

    return {
      active_topics: toNumberOrNull(payload.active_topics) ?? topics.length,
      topics,
    };
  }

  async fetchKanbanCards(limit = 100): Promise<MemoryEntry[]> {
    // `list_memories` is not in the standard allow-list. `tachi_task`
    // with action=board is the standard-visible kanban accessor.
    const payload = await this.callTool('tachi_task', {
      action: 'board',
      state_filter: 'all',
      limit,
    });

    // tachi_task board returns { board, count, tasks: [...] }. Each task
    // is already a compact summary row; we pass it through as a
    // MemoryEntry-shaped record so downstream renderers don't change.
    const tasks = isRecord(payload) && Array.isArray((payload as Record<string, unknown>).tasks)
      ? ((payload as Record<string, unknown>).tasks as unknown[])
      : Array.isArray(payload)
        ? payload
        : [];
    return tasks.filter(isRecord).map((card) => {
      const meta = card as Record<string, unknown>;
      const dispatchId = toStringOrNull(meta.dispatch_id);
      const id = dispatchId ?? toStringOrNull(meta.id) ?? `kanban-${Math.random().toString(16).slice(2)}`;
      return {
        ...meta,
        id,
        summary: toStringOrNull(meta.summary) ?? '',
        path: `/kanban/tasks/${dispatchId ?? id}`,
      } as MemoryEntry;
    });
  }

  private async callTool(name: string, args: Record<string, unknown>): Promise<unknown> {
    const result = await this.sendRequest('tools/call', { name, arguments: args });
    return parseToolCallResult(result);
  }

  /**
   * Call an MCP tool that may not be exposed under the active server profile
   * (e.g. admin-only tools like `tachi_audit_log` / `memory_gc` invoked from
   * the desktop Hub dashboard while the daemon runs with the default
   * `standard` profile). On "tool not found" we return `fallback` instead
   * of bubbling an error so the UI can render a graceful empty state.
   *
   * Genuine transport failures (`TachiOfflineError`) are still re-thrown so
   * the daemon-offline banner can react.
   */
  private async callToolSoft<T>(
    name: string,
    args: Record<string, unknown>,
    fallback: T,
  ): Promise<T | unknown> {
    try {
      return await this.callTool(name, args);
    } catch (error) {
      if (isTachiOfflineError(error)) {
        throw error;
      }
      const message = error instanceof Error ? error.message.toLowerCase() : '';
      if (message.includes('tool not found') || message.includes('unknown tool')) {
        return fallback;
      }
      // Unknown tool errors also come back as `-32602 invalid_request` in
      // some transport layers; treat any error during a soft-optional call
      // as a graceful empty rather than a UI-breaking throw.
      return fallback;
    }
  }

  private async sendRequest(method: string, params: unknown): Promise<unknown> {
    return this.withBaseUrl(async (baseUrl) => {
      await this.ensureInitialized(baseUrl);

      const id = this.nextRequestId();
      const request: JsonRpcRequestShape = {
        jsonrpc: '2.0',
        id,
        method,
        params,
      };

      const responses = await this.postMessage(baseUrl, request);
      const matched = responses.find((response) => response.id === id);

      if (!matched) {
        throw new Error(`No JSON-RPC response for request ${method}`);
      }

      if (matched.error) {
        throw new Error(matched.error.message);
      }

      return matched.result;
    });
  }

  private async ensureInitialized(baseUrl: string): Promise<void> {
    const session = this.getSession(baseUrl);
    if (session.initialized) {
      return;
    }

    const initializeId = this.nextRequestId();
    const initializeRequest: JsonRpcRequestShape = {
      jsonrpc: '2.0',
      id: initializeId,
      method: 'initialize',
      params: {
        protocolVersion: MCP_PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: {
          name: 'tachi-desktop',
          version: '0.2.0',
        },
      },
    };

    const initializeResponses = await this.postMessage(baseUrl, initializeRequest);
    const initializeResponse = initializeResponses.find((response) => response.id === initializeId);

    if (!initializeResponse) {
      throw new Error('Tachi daemon did not return initialize response');
    }
    if (initializeResponse.error) {
      throw new Error(initializeResponse.error.message);
    }

    const initializedNotification: JsonRpcRequestShape = {
      jsonrpc: '2.0',
      method: 'notifications/initialized',
      params: {},
    };
    await this.postMessage(baseUrl, initializedNotification);

    session.initialized = true;
    this.activeBaseUrl = baseUrl;
  }

  private getSession(baseUrl: string): SessionState {
    const existing = this.sessionByBase.get(baseUrl);
    if (existing) {
      return existing;
    }

    const created: SessionState = { initialized: false };
    this.sessionByBase.set(baseUrl, created);
    return created;
  }

  private async postMessage(baseUrl: string, request: JsonRpcRequestShape): Promise<JsonRpcResponseShape[]> {
    const session = this.getSession(baseUrl);
    const headers = new Headers({
      'Content-Type': 'application/json',
      Accept: 'application/json, text/event-stream',
      'MCP-Protocol-Version': MCP_PROTOCOL_VERSION,
    });

    if (session.sessionId) {
      headers.set('Mcp-Session-Id', session.sessionId);
    }

    const response = await this.fetchWithTimeout(`${baseUrl}/message`, {
      method: 'POST',
      headers,
      body: JSON.stringify(request),
    });

    const nextSessionId = extractSessionId(response.headers);
    if (nextSessionId) {
      session.sessionId = nextSessionId;
    }

    const responseText = await response.text();
    if (!response.ok) {
      const message = responseText.trim() || response.statusText;
      throw new Error(`HTTP ${response.status}: ${message}`);
    }

    return parseHttpMessages(responseText, response.headers.get('content-type'));
  }

  private async fetchWithTimeout(input: RequestInfo | URL, init: RequestInit): Promise<Response> {
    const controller = new AbortController();
    const timeoutId = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

    try {
      return await fetch(input, { ...init, signal: controller.signal });
    } finally {
      window.clearTimeout(timeoutId);
    }
  }

  private nextRequestId(): JsonRpcId {
    const id = this.nextRequestIdValue;
    this.nextRequestIdValue += 1;
    return id;
  }

  private async withBaseUrl<T>(worker: (baseUrl: string) => Promise<T>): Promise<T> {
    const ordered = this.activeBaseUrl
      ? [this.activeBaseUrl, ...this.baseUrls.filter((base) => base !== this.activeBaseUrl)]
      : [...this.baseUrls];

    let lastError: unknown;
    for (const baseUrl of ordered) {
      try {
        const result = await worker(baseUrl);
        this.activeBaseUrl = baseUrl;
        return result;
      } catch (error) {
        lastError = error;
        this.sessionByBase.delete(baseUrl);
        if (this.activeBaseUrl === baseUrl) {
          this.activeBaseUrl = null;
        }
      }
    }

    throw new TachiOfflineError(`Unable to reach Tachi daemon at ${DEFAULT_TACHI_DAEMON_HOST}`, lastError);
  }
}

export const tachiApi = new TachiApi();
