import path from "node:path";
import { fileURLToPath } from "node:url";
import { Type, type Static } from "@sinclair/typebox";
import { defaultDbPath, defaultGlobalDbPath, resolveUserPath } from "./constants.js";

// ============================================================================
// Memory Entry Schema (matches Phase 2 architecture)
// ============================================================================

export type SourceRef = {
  ref_type: "turn" | "file" | "message" | "event" | "other";
  ref_id: string;
  start?: number;
  end?: number;
  note?: string;
};

export type MemoryCategory = "preference" | "fact" | "decision" | "entity" | "other";

export type MemoryEntry = {
  id: string; // From entry_id
  text: string; // From lossless_restatement
  summary: string;
  keywords: string[];
  timestamp: string;
  location: string;
  persons: string[];
  entities: string[];
  topic: string;
  scope: string;
  path: string;
  category: MemoryCategory;
  importance: number;
  access_count: number;
  last_access: string | null;
  vector?: number[];
  metadata: {
    source_refs: SourceRef[];
    [key: string]: any;
  };
};

// ============================================================================
// Plugin Config Schema (OpenClaw plugin API compatible)
// ============================================================================

export type BridgeConfig = {
  globalDbPath: string;
  dbPath: string;
  shadowStorePath: string; // Keep for migration only
  auditLogPath: string;
  topK: number;
  exposeExperimentalTachiTools: boolean;
  captureMinChars: number;
  captureTriggerKeywords: string[];
  selfEvolutionAgents: string[];
  sharedMemoryAliases: Record<string, string>;
  weights: { semantic: number; fts: number; symbolic: number; decay: number };
};

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const pluginDataDir = path.resolve(moduleDir, "data");
const workspaceRoot = process.env.OPENCLAW_WORKSPACE || "";
const configuredProjectDbPath = process.env.TACHI_PROJECT_DB_PATH || process.env.MEMORY_DB_PATH;

export const defaultConfig: BridgeConfig = {
  globalDbPath: process.env.TACHI_GLOBAL_DB_PATH
    ? resolveUserPath(process.env.TACHI_GLOBAL_DB_PATH)
    : defaultGlobalDbPath,
  dbPath: configuredProjectDbPath
    ? resolveUserPath(configuredProjectDbPath)
    : defaultDbPath,
  shadowStorePath: path.resolve(
    pluginDataDir,
    "shadow-store.jsonl",
  ),
  auditLogPath: path.resolve(pluginDataDir, "audit-log.jsonl"),
  topK: 6,
  exposeExperimentalTachiTools:
    process.env.TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS === "1" ||
    process.env.TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS === "true",
  // Branch #7 (B#7) — bumped from 24 → 200 to align with the Sigil
  // capture-gate floor (TACHI_CAPTURE_GATE warns/blocks on entries shorter
  // than DEFAULT_CAPTURE_MIN_CHARS=200). Keeps OpenClaw's auto-capture from
  // emitting low-signal scratchpad noise that the server would just reject.
  captureMinChars: Number(process.env.MEMORY_BRIDGE_CAPTURE_MIN_CHARS || 200),
  captureTriggerKeywords: (
    process.env.MEMORY_BRIDGE_CAPTURE_TRIGGERS ||
    "记住,remember,偏好,喜欢,讨厌,生日,地址,电话,邮箱,习惯,计划,deadline,TODO,密码,账号,关键,always,never,重要,important"
  )
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean),
  selfEvolutionAgents: ["jayne"],
  sharedMemoryAliases: {
    ops: "main",
  },
  weights: {
    semantic: 0.4,
    fts: 0.3,
    symbolic: 0.2,
    decay: 0.1,
  },
};

export const bridgeConfigSchema = {
  parse(value: unknown): BridgeConfig {
    // Start with defaults, merge any overrides from plugin config
    const overrides = (value && typeof value === "object" ? value : {}) as Partial<BridgeConfig>;
    const rawTopK = Number(overrides.topK ?? defaultConfig.topK);
    const rawCaptureMinChars = Number(overrides.captureMinChars ?? defaultConfig.captureMinChars);
    const rawWeights: Partial<BridgeConfig["weights"]> = overrides.weights || {};

    const sharedMemoryAliases =
      overrides.sharedMemoryAliases && typeof overrides.sharedMemoryAliases === "object"
        ? {
            ...defaultConfig.sharedMemoryAliases,
            ...Object.fromEntries(
              Object.entries(overrides.sharedMemoryAliases)
                .filter(
                  (entry): entry is [string, string] =>
                    typeof entry[0] === "string" && typeof entry[1] === "string",
                )
                .map(([fromAgent, toAgent]) => [fromAgent.trim().toLowerCase(), toAgent.trim().toLowerCase()])
                .filter(([fromAgent, toAgent]) => Boolean(fromAgent) && Boolean(toAgent)),
            ),
          }
        : defaultConfig.sharedMemoryAliases;

    const clampWeight = (value: unknown, fallback: number) => {
      const parsed = Number(value);
      return Number.isFinite(parsed) ? parsed : fallback;
    };

    return {
      ...defaultConfig,
      ...overrides,
      topK: Number.isInteger(rawTopK) && rawTopK > 0 ? rawTopK : defaultConfig.topK,
      captureMinChars:
        Number.isFinite(rawCaptureMinChars) && rawCaptureMinChars > 0
          ? rawCaptureMinChars
          : defaultConfig.captureMinChars,
      selfEvolutionAgents: Array.isArray(overrides.selfEvolutionAgents)
        ? overrides.selfEvolutionAgents
            .filter((value): value is string => typeof value === "string")
            .map((value) => value.trim())
            .filter(Boolean)
        : defaultConfig.selfEvolutionAgents,
      sharedMemoryAliases,
      weights: {
        semantic: clampWeight(rawWeights.semantic, defaultConfig.weights.semantic),
        fts: clampWeight(rawWeights.fts, defaultConfig.weights.fts),
        symbolic: clampWeight(rawWeights.symbolic, defaultConfig.weights.symbolic),
        decay: clampWeight(rawWeights.decay, defaultConfig.weights.decay),
      },
    };
  },
};

export { moduleDir as pluginDir, pluginDataDir, workspaceRoot };
