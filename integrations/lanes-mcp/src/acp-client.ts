/**
 * ACP client wrapper — spawns one lane CLI as an ACP agent subprocess, performs
 * the initialize / session/new handshake, and exposes a long-lived
 * `ActiveSession` plus cancel / close controls.
 *
 * Built on the official `@agentclientprotocol/sdk`. Its `ActiveSession`
 * (`prompt()` + `nextUpdate()` loop yielding `session_update` / `stop`) maps
 * 1:1 onto spec §6, so hand-rolling the JSON-RPC core is unnecessary.
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import fs from "node:fs";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";
import type {
  ActiveSession,
  ClientContext,
  RequestPermissionRequest,
  RequestPermissionResponse,
  ReadTextFileRequest,
  ReadTextFileResponse,
  WriteTextFileRequest,
  WriteTextFileResponse,
} from "@agentclientprotocol/sdk";
import type { SpawnSpec } from "./types.js";

/** Minimal promise-with-resolvers helper (Node's Promise.withResolvers exists on 22+ but kept explicit). */
class Deferred<T> {
  readonly promise: Promise<T>;
  resolve!: (v: T | PromiseLike<T>) => void;
  reject!: (e: unknown) => void;
  constructor() {
    this.promise = new Promise<T>((res, rej) => {
      this.resolve = res;
      this.reject = rej;
    });
  }
}

export interface LaneConnectionOptions {
  spec: SpawnSpec;
  cwd: string;
  readOnly: boolean;
  /** Invoked when the agent routes a write through the client fs capability. */
  onFileWritten?: (absPath: string) => void;
}

/**
 * A live ACP connection to one lane subprocess.
 *
 * The connection stays open across multiple prompt turns (G1c persistent
 * session). Call `close()` to dispose the session and kill the subprocess.
 */
export class LaneConnection {
  readonly session: ActiveSession;
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly ctx: ClientContext;
  private readonly shutdown: Deferred<void>;
  private closed = false;

  private constructor(
    session: ActiveSession,
    child: ChildProcessWithoutNullStreams,
    ctx: ClientContext,
    shutdown: Deferred<void>,
    stderrRef: () => string,
  ) {
    this.session = session;
    this.child = child;
    this.ctx = ctx;
    this.shutdown = shutdown;
    this.getStderr = stderrRef;
  }

  private getStderr: () => string;

  get sessionId(): string {
    return this.session.sessionId;
  }

  /** Send an ACP `session/cancel` notification for this session. */
  async cancel(): Promise<void> {
    if (this.closed) return;
    await this.ctx.notify(acp.methods.agent.session.cancel, { sessionId: this.session.sessionId });
  }

  /** Dispose the session's update routing and terminate the subprocess. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.shutdown.resolve();
    try {
      this.session.dispose();
    } catch {
      /* already disposed */
    }
    if (!this.child.killed) {
      this.child.kill("SIGTERM");
      // Escalate if the child ignores SIGTERM.
      setTimeout(() => {
        if (!this.child.killed) this.child.kill("SIGKILL");
      }, 2_000).unref();
    }
  }

  stderr(): string {
    return this.getStderr();
  }

  /**
   * Spawn the lane subprocess and complete the ACP handshake. Resolves once the
   * session is created and ready to prompt; rejects if the subprocess exits or
   * the handshake fails before then.
   */
  static async connect(options: LaneConnectionOptions): Promise<LaneConnection> {
    const { spec, cwd, readOnly, onFileWritten } = options;

    const child = spawn(spec.command, spec.args, {
      cwd,
      env: { ...process.env, ...spec.env },
      stdio: ["pipe", "pipe", "pipe"],
    }) as ChildProcessWithoutNullStreams;

    let stderrTail = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      stderrTail = (stderrTail + chunk).slice(-4_000);
    });

    const ready = new Deferred<LaneConnection>();
    const shutdown = new Deferred<void>();
    let resolvedReady = false;

    child.on("error", (err) => {
      if (!resolvedReady) ready.reject(new Error(`failed to spawn '${spec.command}': ${err.message}`));
    });
    child.on("exit", (code, signal) => {
      shutdown.resolve();
      if (!resolvedReady) {
        ready.reject(
          new Error(
            `lane process exited before handshake (code=${code} signal=${signal}). stderr: ${stderrTail.trim().slice(-800)}`,
          ),
        );
      }
    });

    const input = Writable.toWeb(child.stdin) as WritableStream<Uint8Array>;
    const output = Readable.toWeb(child.stdout) as unknown as ReadableStream<Uint8Array>;
    const stream = acp.ndJsonStream(input, output);

    const handlePermission = (params: RequestPermissionRequest): RequestPermissionResponse => {
      const options_ = params.options ?? [];
      if (options_.length === 0) {
        return { outcome: { outcome: "cancelled" } };
      }
      const wanted = readOnly ? "reject" : "allow";
      const chosen = options_.find((o) => (o.kind ?? "").startsWith(wanted)) ?? options_[0];
      return { outcome: { outcome: "selected", optionId: chosen.optionId } };
    };

    const handleReadTextFile = (params: ReadTextFileRequest): ReadTextFileResponse => {
      try {
        const content = fs.readFileSync(params.path, "utf8");
        return { content };
      } catch (e) {
        throw new Error(`read_text_file failed for ${params.path}: ${e instanceof Error ? e.message : String(e)}`);
      }
    };

    const handleWriteTextFile = (params: WriteTextFileRequest): WriteTextFileResponse => {
      if (readOnly) {
        throw new Error(`write rejected: lane is read-only (path=${params.path})`);
      }
      fs.writeFileSync(params.path, params.content);
      onFileWritten?.(params.path);
      return {};
    };

    // connectWith holds the connection open for the lifetime of `op`; we park on
    // `shutdown` so the ActiveSession stays usable from outside this closure.
    acp
      .client({ name: "lanes-mcp" })
      .onRequest(acp.methods.client.session.requestPermission, (rc) => handlePermission(rc.params))
      .onRequest(acp.methods.client.fs.readTextFile, (rc) => handleReadTextFile(rc.params))
      .onRequest(acp.methods.client.fs.writeTextFile, (rc) => handleWriteTextFile(rc.params))
      .connectWith(stream, async (ctx) => {
        await ctx.request(acp.methods.agent.initialize, {
          protocolVersion: acp.PROTOCOL_VERSION,
          clientCapabilities: { fs: { readTextFile: true, writeTextFile: true } },
        });
        const session = await ctx.buildSession(cwd).start();
        const conn = new LaneConnection(session, child, ctx, shutdown, () => stderrTail);
        resolvedReady = true;
        ready.resolve(conn);
        await shutdown.promise;
        try {
          session.dispose();
        } catch {
          /* noop */
        }
      })
      .catch((e) => {
        if (!resolvedReady) ready.reject(e);
      });

    return ready.promise;
  }
}
