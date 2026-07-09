import { spawn, execFile } from 'child_process';
import { promisify } from 'util';
import { existsSync } from 'fs';
import { getBinaryPath, loadConfig } from './config.js';

const execFileAsync = promisify(execFile);

interface DaemonStatus {
  online: boolean;
  pid?: number;
  port?: number;
  memoryCount?: number;
  error?: string;
}

/**
 * Run a `tachi` subcommand and parse the JSON it prints to stdout.
 *
 * The binary writes a `tachi: logging to ...` line to stderr and clean JSON to
 * stdout; we still slice from the first `{` to be defensive against any future
 * banner noise. Returns null on spawn failure, timeout, or unparseable output.
 */
async function tachiJson(args: string[]): Promise<unknown | null> {
  const bin = getBinaryPath();
  try {
    const { stdout } = await execFileAsync(bin, args, { timeout: 15_000 });
    const start = stdout.indexOf('{');
    return start >= 0 ? JSON.parse(stdout.slice(start)) : null;
  } catch {
    return null;
  }
}

/**
 * Daemon lifecycle is delegated entirely to the real `tachi` binary, which owns
 * the HTTP daemon, its pid/lock file, and cross-process discovery. The CLI no
 * longer reimplements any of that (it previously spawned the binary in stdio
 * mode with env vars it ignores, then HTTP-polled a server it never started).
 */
export async function getDaemonStatus(enrich = true): Promise<DaemonStatus> {
  // Authoritative, cheap liveness check via the binary's own pid/lock file.
  const daemon = (await tachiJson(['daemon', 'status', '--json'])) as
    | { state?: string; pid?: number }
    | null;
  if (!daemon || daemon.state !== 'running') {
    return { online: false };
  }

  const config = loadConfig();
  const status: DaemonStatus = {
    online: true,
    pid: typeof daemon.pid === 'number' ? daemon.pid : undefined,
    port: config.daemon.port,
  };

  // Best-effort enrichment: total memory entries across all manifest DBs. Skip
  // the heavier `tachi status` query for liveness-only checks (start/stop polls).
  if (enrich) {
    const full = (await tachiJson(['status', '--json'])) as
      | { dbs?: { memory_total?: number }[] }
      | null;
    if (full && Array.isArray(full.dbs)) {
      status.memoryCount = full.dbs.reduce((sum, db) => sum + (db.memory_total ?? 0), 0);
    }
  }

  return status;
}

export async function startDaemon(): Promise<{ success: boolean; error?: string }> {
  const bin = getBinaryPath();
  if ((bin.includes('/') || bin.includes('\\')) && !existsSync(bin)) {
    return {
      success: false,
      error: `tachi binary not found at ${bin}. Install it (e.g. \`cargo install --path crates/tachi-server\`) or set TACHI_BINARY.`,
    };
  }

  if ((await getDaemonStatus(false)).online) {
    return { success: false, error: 'Daemon is already running' };
  }

  const config = loadConfig();
  // Start the binary's own HTTP daemon, detached + unref so it outlives this
  // short-lived CLI process. The daemon writes its own pid/lock file.
  const child = spawn(bin, ['--daemon', '--port', String(config.daemon.port)], {
    detached: true,
    stdio: 'ignore',
  });
  child.unref();

  // Poll the binary's own status to confirm it actually came up.
  for (let i = 0; i < 10; i++) {
    await new Promise((resolve) => setTimeout(resolve, 400));
    if ((await getDaemonStatus(false)).online) {
      return { success: true };
    }
  }
  return {
    success: false,
    error: 'Daemon did not report running; check ~/.tachi/logs/tachi.log',
  };
}

export async function stopDaemon(): Promise<{ success: boolean; error?: string }> {
  if (!(await getDaemonStatus(false)).online) {
    return { success: false, error: 'Daemon is not running' };
  }
  const bin = getBinaryPath();
  try {
    // `tachi daemon kill` sends SIGTERM to the running daemon and cleans up its
    // lock file.
    await execFileAsync(bin, ['daemon', 'kill'], { timeout: 15_000 });
    return { success: true };
  } catch (err) {
    return { success: false, error: (err as Error).message };
  }
}
