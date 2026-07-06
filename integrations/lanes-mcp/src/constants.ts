/**
 * Shared constants and env-derived configuration for the lanes MCP server.
 *
 * Thresholds are provisional (per doctrine: no flat magic numbers baked as
 * final) and overridable by env so the leader can calibrate from telemetry.
 */
import os from "node:os";
import path from "node:path";

/** Default silence threshold before a running lane is flagged as suspected-stall. */
export const DEFAULT_STALL_THRESHOLD_MS = envInt("LANES_STALL_THRESHOLD_MS", 300_000);

/** Handshake (initialize + session/new) timeout before a lane spawn is failed. */
export const HANDSHAKE_TIMEOUT_MS = envInt("LANES_HANDSHAKE_TIMEOUT_MS", 30_000);

/**
 * Hard per-turn ceiling. suspected_stall is only a warning; this timeout is the
 * guaranteed path to a terminal state — on hit the turn is forced to `error` and
 * the subprocess is killed, so lane_dispatch can never hang forever.
 */
export const TURN_TIMEOUT_MS = envInt("LANES_TURN_TIMEOUT_MS", 2_700_000);

/** Default long-poll window for lane_wait. */
export const DEFAULT_WAIT_MS = envInt("LANES_WAIT_DEFAULT_MS", 30_000);

/**
 * Hard cap on lane_wait timeout. Kept below the typical MCP client request
 * timeout so a wait always returns before the transport gives up.
 */
export const MAX_WAIT_MS = envInt("LANES_WAIT_MAX_MS", 55_000);

/** Rough character budget for a lane_wait digest (~500 tokens). */
export const DIGEST_CHAR_BUDGET = envInt("LANES_DIGEST_CHAR_BUDGET", 2_000);

/** Character budget for a returned final_message before truncation. */
export const FINAL_MESSAGE_CHAR_BUDGET = envInt("LANES_FINAL_MESSAGE_CHAR_BUDGET", 20_000);

/** Experimental: project ACP plan progress as MCP notifications/progress. */
export const PROGRESS_EXPERIMENTAL = process.env.LANES_PROGRESS_EXPERIMENTAL === "1";

/** Root under which run artifacts (events.jsonl, chunks.log) are written. */
export const RUNS_ROOT =
  process.env.LANES_RUNS_ROOT ?? path.join(os.homedir(), ".cache", "lanes-mcp", "runs");

/** Root under which server-managed git worktrees are created. */
export const WORKTREES_ROOT =
  process.env.LANES_WORKTREES_ROOT ?? path.join(os.homedir(), ".cache", "lanes-mcp", "worktrees");

/**
 * Base git repository from which worktrees are cut. Defaults to the directory
 * Claude Code launched the server from. Cut always happens from origin/main.
 */
export const BASE_REPO = process.env.LANES_MCP_BASE_REPO ?? process.cwd();

export const SERVER_NAME = "lanes-mcp-server";
export const SERVER_VERSION = "0.1.0";

/**
 * Single source of truth for opencode model shortnames (mirrors the /oc-dispatch
 * habit). Resolved server-side by the opencode lane so commands can pass a
 * shortname or a full `provider/model` id interchangeably.
 */
export const OC_MODEL_ALIASES: Readonly<Record<string, string>> = {
  glm: "zhipuai-coding-plan/glm-5.2",
  ds: "deepseek/deepseek-v4-pro",
  kimi: "kimi-for-coding/k2p6",
  free: "opencode/deepseek-v4-flash-free",
};

/** Expand an opencode model shortname to its full id; pass through unknown/full ids. */
export function resolveOcModel(model: string | undefined): string | undefined {
  if (!model) return model;
  return OC_MODEL_ALIASES[model] ?? model;
}

function envInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (raw === undefined) return fallback;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}
