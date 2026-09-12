import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { TACHI_CLI_VERSION } from '../version.js';

const REQUEST_TIMEOUT_MS = 10_000;
const CLEANUP_TIMEOUT_MS = 1_000;

export interface TachiMcpOptions {
  endpoint?: URL;
  timeoutMs?: number;
}

function withDeadline<T>(promise: Promise<T>, timeoutMs: number, operation: string): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${operation} timed out after ${timeoutMs}ms`)), timeoutMs);
  });
  return Promise.race([promise, deadline]).finally(() => clearTimeout(timer));
}

function toolErrorText(content: unknown): string {
  if (!Array.isArray(content)) return '';
  return content
    .filter((item): item is { type: 'text'; text: string } =>
      typeof item === 'object' && item !== null && item.type === 'text' && typeof item.text === 'string')
    .map(item => item.text)
    .join('\n');
}

async function cleanup(
  transport: StreamableHTTPClientTransport,
  client: Client,
): Promise<void> {
  let terminationError: unknown;
  try {
    if (transport.sessionId) {
      await withDeadline(transport.terminateSession(), CLEANUP_TIMEOUT_MS, 'MCP session termination');
    }
  } catch (error) {
    terminationError = error;
  } finally {
    await withDeadline(client.close(), CLEANUP_TIMEOUT_MS, 'MCP client close');
  }
  if (terminationError !== undefined) throw terminationError;
}

export async function callTachiTool(
  port: number,
  name: string,
  args: Record<string, unknown>,
  options: TachiMcpOptions = {},
): Promise<unknown> {
  const timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
  const endpoint = options.endpoint ?? new URL(`http://127.0.0.1:${port}/mcp`);
  const transport = new StreamableHTTPClientTransport(endpoint);
  const client = new Client({ name: 'tachi-cli', version: TACHI_CLI_VERSION });
  let primaryError: unknown;

  try {
    await withDeadline(client.connect(transport), timeoutMs, 'MCP initialization');
    const result = await client.callTool(
      { name, arguments: args },
      undefined,
      { timeout: timeoutMs },
    );
    const text = toolErrorText(result.content);
    if (result.isError) {
      throw new Error(`${name} failed${text ? `: ${text}` : ''}`);
    }
    if (!text) {
      throw new Error(`${name} returned no text content`);
    }
    try {
      return JSON.parse(text);
    } catch (error) {
      throw new Error(`${name} returned malformed JSON: ${error instanceof Error ? error.message : String(error)}`);
    }
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    try {
      await cleanup(transport, client);
    } catch {
      if (primaryError === undefined) {
        console.warn('MCP session cleanup failed; completed tool result retained.');
      }
    }
  }
}

export function requireArray<T>(value: unknown, toolName: string): T[] {
  if (!Array.isArray(value)) {
    throw new Error(`${toolName} returned malformed response: expected an array`);
  }
  return value as T[];
}
