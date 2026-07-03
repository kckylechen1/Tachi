import type { OpenClawPluginApi } from "openclaw/plugin-sdk";

export type HostRole = "controller" | "executor" | "observer" | "reviewer" | "notifier";

export type HostEventInput = {
  eventType: string;
  actor: string;
  sessionId?: string | null;
  runId?: string | null;
  role: HostRole;
  status?: string | null;
  goal?: string | null;
  currentStep?: string | null;
  nextAction?: string | null;
  summary?: string | null;
  blocker?: string | null;
  evidenceRefs?: string[];
  artifactRefs?: string[];
  payload?: Record<string, unknown>;
  provenance?: Record<string, unknown>;
};

export type TachiEventEmitter = (params: Record<string, unknown>) => Promise<unknown>;

export const HOST_ADAPTER_ID = "openclaw";
export const HOST_SOURCE_REPO = "openclaw";
export const HOST_DOMAIN = "agent_host";

export const hostRoles: Record<string, HostRole> = {
  main: "controller",
  ops: "controller",
  researcher: "observer",
};

export function resolveHostRole(agentId: string): HostRole {
  return hostRoles[agentId.toLowerCase()] ?? "executor";
}

export function buildHostContinuityPrompt(): string[] {
  return [
    "Tachi continuity board is the source of truth for parallel agent work.",
    "Use continuity_board before taking over unclear work, dispatching another agent, or reporting current multi-agent status.",
    "Foreground broker hosts should create, monitor, and hand off work lanes; background workers should execute bounded tasks and report evidence back to Tachi.",
  ];
}

export function buildHostEventParams(input: HostEventInput): Record<string, unknown> {
  const sessionId = input.sessionId || input.runId || "";
  const payload = {
    schema: "tachi.agent_host_event.v1",
    host: HOST_ADAPTER_ID,
    role: input.role,
    status: input.status || null,
    goal: input.goal || null,
    current_step: input.currentStep || null,
    next_action: input.nextAction || null,
    summary: input.summary || null,
    blocker: input.blocker || null,
    evidence_refs: input.evidenceRefs || [],
    artifact_refs: input.artifactRefs || [],
    ...input.payload,
  };
  return {
    action: "emit",
    source_repo: HOST_SOURCE_REPO,
    adapter: HOST_ADAPTER_ID,
    domain: HOST_DOMAIN,
    session_id: sessionId,
    actor: input.actor,
    event_type: input.eventType,
    authority: input.blocker ? "blocker" : "advisory",
    effects: ["project_cycle", "prompt"],
    projection_hints: ["timeline", "project_cycle", "pattern"],
    payload,
    provenance: {
      host: HOST_ADAPTER_ID,
      run_id: input.runId || null,
      session_id: input.sessionId || null,
      ...input.provenance,
    },
  };
}

export async function emitHostContinuityEvent(
  api: OpenClawPluginApi,
  emit: TachiEventEmitter,
  input: HostEventInput,
): Promise<unknown | null> {
  try {
    return await emit(buildHostEventParams(input));
  } catch (error) {
    api.logger.warn(`tachi: host continuity event skipped: ${String(error)}`);
    return null;
  }
}

export async function loadContinuityBoard(
  emit: TachiEventEmitter,
  params?: {
    project?: string;
    sessionId?: string;
    limit?: number;
    readOnly?: boolean;
  },
): Promise<unknown> {
  return await emit({
    action: params?.readOnly === false ? "context" : "a2a",
    adapter: HOST_ADAPTER_ID,
    source_repo: HOST_SOURCE_REPO,
    domain: HOST_DOMAIN,
    project: params?.project,
    session_id: params?.sessionId,
    projection_hints: ["timeline", "project_cycle", "pattern"],
    limit: params?.limit ?? 20,
  });
}
