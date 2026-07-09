import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const APP_NAME = "tachi";

// Load secrets that may not be in process.env when the gateway launches
// without direnv (e.g. launchd, cron, non-interactive shells).
// Mirrors the Rust tachi-server's env chain: ~/.secrets/master.env first,
// then TACHI_HOME/config.env for overrides.  Only backfills missing vars.
function loadEnvFile(filePath: string, override = false): void {
  try {
    const raw = fs.readFileSync(filePath, "utf8");
    for (const line of raw.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("#")) continue;
      const eqIdx = trimmed.indexOf("=");
      if (eqIdx <= 0) continue;
      const key = trimmed.slice(0, eqIdx).trim();
      if (!override && process.env[key]) continue; // don't overwrite existing unless override
      let val = trimmed.slice(eqIdx + 1).trim();
      // Strip surrounding quotes
      if ((val.startsWith('"') && val.endsWith('"')) || (val.startsWith("'") && val.endsWith("'"))) {
        val = val.slice(1, -1);
      }
      process.env[key] = val;
    }
  } catch {
    // File missing or unreadable — silently skip
  }
}

const secretsPath = path.join(os.homedir(), ".secrets", "master.env");
loadEnvFile(secretsPath);

export function resolveUserPath(value: string): string {
  return path.resolve(value.replace(/^~/, os.homedir()));
}

export const appHome = resolveUserPath(
  process.env.TACHI_HOME || process.env.SIGIL_HOME || `~/.${APP_NAME}`,
);

// Second pass: TACHI_HOME/config.env can override secrets (same as Rust server)
loadEnvFile(path.join(appHome, "config.env"), true);

export const defaultGlobalDbPath = path.resolve(appHome, "global", "memory.db");

// OpenClaw's plugin-local DB remains the project/workspace store. Tachi's
// process-wide global store lives at defaultGlobalDbPath.
export const defaultDbPath = path.resolve(appHome, "memory.db");
