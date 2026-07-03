/**
 * Minimal type declarations for the OpenClaw Plugin SDK.
 * Replace with the official SDK types when available.
 */
declare module "openclaw/plugin-sdk" {
  type CoreAgentHookEvent = "before_agent_start" | "agent_end";
  type RuntimeHookEvent =
    | "llm_input"
    | "llm_output"
    | "after_tool_call"
    | "before_compaction"
    | "after_compaction"
    | "tool_result_persist"
    | "subagent_spawned"
    | "subagent_ended"
    | "session_end";

  export type MemorySearchResult = {
    path: string;
    score: number;
    snippet: string;
    title?: string;
    kind?: string;
    id?: string;
    startLine?: number;
    endLine?: number;
    source?: string;
    provenanceLabel?: string;
    sourceType?: string;
    sourcePath?: string;
    updatedAt?: string;
  };

  export type MemoryReadResult = {
    path: string;
    content: string;
    fromLine: number;
    lineCount: number;
    title?: string;
    kind?: string;
    id?: string;
    provenanceLabel?: string;
    sourceType?: string;
    sourcePath?: string;
    updatedAt?: string;
  };

  export type MemoryProviderStatus = {
    ok: boolean;
    provider: string;
    message?: string;
    details?: Record<string, unknown>;
  };

  export interface MemorySearchManager {
    search(
      query: string,
      opts?: {
        maxResults?: number;
        minScore?: number;
        sessionKey?: string;
        signal?: AbortSignal;
      },
    ): Promise<MemorySearchResult[]>;
    readFile(params: { relPath: string; from?: number; lines?: number }): Promise<MemoryReadResult>;
    status(): MemoryProviderStatus;
    sync?(params?: { reason?: string; force?: boolean; sessionFiles?: string[] }): Promise<void>;
    probeEmbeddingAvailability?(): Promise<{ ok: boolean; message?: string }>;
    probeVectorAvailability?(): Promise<boolean>;
    close?(): Promise<void>;
  }

  export type MemoryPluginRuntime = {
    getMemorySearchManager(params: {
      cfg: unknown;
      agentId: string;
      purpose?: "default" | "status" | "cli";
    }): Promise<{ manager: MemorySearchManager | null; error?: string }>;
    resolveMemoryBackendConfig(params: { cfg: unknown; agentId: string }): { backend: "builtin" | "qmd"; qmd?: unknown };
    closeMemorySearchManager?(params: { cfg: unknown; agentId: string }): Promise<void>;
    closeAllMemorySearchManagers?(): Promise<void>;
  };

  export type MemoryPluginCapability = {
    promptBuilder?: (params: { availableTools: Set<string>; citationsMode?: string }) => string[];
    flushPlanResolver?: (params: { cfg?: unknown; nowMs?: number }) => unknown | null;
    runtime?: MemoryPluginRuntime;
    publicArtifacts?: {
      listArtifacts(params: { cfg: unknown }): Promise<
        Array<{
          kind: string;
          workspaceDir: string;
          relativePath: string;
          absolutePath: string;
          agentIds: string[];
          contentType: "markdown" | "json" | "text";
        }>
      >;
    };
  };

  export interface OpenClawPluginApi {
    pluginConfig: unknown;
    logger: {
      info: (...args: any[]) => void;
      warn: (...args: any[]) => void;
      error: (...args: any[]) => void;
      debug: (...args: any[]) => void;
    };
    resolvePath(relativePath: string): string;
    registerTool(tool: {
      name: string;
      label: string;
      description: string;
      parameters: unknown;
      execute: (
        toolCallId: string,
        params: unknown,
        signal: AbortSignal,
        context: unknown,
      ) => Promise<{
        content: Array<{ type: string; text: string }>;
        details?: Record<string, unknown>;
      }>;
    }): void;
    on(
      event: CoreAgentHookEvent | RuntimeHookEvent,
      handler: (event: any, ctx: any) => Promise<any>,
    ): void;
    registerMemoryCapability?(capability: MemoryPluginCapability): void;
    registerService(service: {
      id: string;
      start: () => void;
      stop: () => void;
    }): void;
  }
}
