import { spawn } from 'child_process';
import { getBinaryPath } from './config.js';

/**
 * Canonical provider API keys the onboarding wizard offers to store. These are
 * the names the Rust binary / provider config reads from the Vault (e.g.
 * `vault:VOYAGE_API_KEY`). Names are passed verbatim as the `<NAME>` argument to
 * `tachi vault set` — only the secret *value* is sensitive and is never placed
 * on argv.
 */
export interface ProviderKeySpec {
  /** Vault secret name, also the env var the binary resolves it under. */
  name: string;
  /** Human-readable label shown in the wizard. */
  label: string;
  /** Optional one-line hint about where to obtain the key. */
  hint?: string;
}

export const PROVIDER_KEYS: ProviderKeySpec[] = [
  {
    name: 'VOYAGE_API_KEY',
    label: 'Voyage AI (embeddings)',
    hint: 'https://dash.voyageai.com',
  },
  {
    name: 'SILICONFLOW_API_KEY',
    label: 'SiliconFlow (LLM / embeddings)',
    hint: 'https://siliconflow.cn',
  },
];

export interface VaultSetResult {
  success: boolean;
  /** Sanitized error message (never contains the secret value). */
  error?: string;
}

/**
 * Persist a single secret by spawning the existing `tachi vault set <NAME>`
 * binary subcommand. The secret VALUE and the vault master PASSWORD are both
 * delivered over STDIN, never via argv (which would leak into `ps`/process
 * listings), matching the binary's `--value-stdin` / `--stdin-password`
 * contract.
 *
 * stdin write order is significant and mirrors the binary's read order:
 *   1. `read_vault_password` consumes the first line (the master password)
 *   2. `value_stdin` consumes the next line (the secret value)
 *
 * We rely ONLY on the pre-existing `tachi vault set` command and do not add any
 * new Rust subcommand. On macOS callers may instead pass `useKeychain: true` to
 * read the master password from the system Keychain, in which case only the
 * value is piped on stdin.
 *
 * The vault must already be initialized (`tachi vault init`); if it is not, the
 * binary returns a clear error which we surface verbatim (it contains no
 * secret).
 */
export function vaultSet(
  name: string,
  value: string,
  opts: { password?: string; useKeychain?: boolean; timeoutMs?: number } = {}
): Promise<VaultSetResult> {
  const bin = getBinaryPath();
  const args = ['vault', 'set', name, '--value-stdin'];

  if (opts.useKeychain) {
    args.push('--keychain');
  } else if (opts.password !== undefined) {
    args.push('--stdin-password');
  }

  return new Promise<VaultSetResult>((resolve) => {
    let settled = false;
    const finish = (result: VaultSetResult) => {
      if (settled) return;
      settled = true;
      resolve(result);
    };

    let child;
    try {
      child = spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] });
    } catch (err) {
      finish({ success: false, error: (err as Error).message });
      return;
    }

    let stderr = '';
    child.stderr?.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    // Drain stdout so the child never blocks on a full pipe; its contents are a
    // plain "Secret '<NAME>' saved" confirmation and carry no secret value.
    child.stdout?.on('data', () => {});

    child.on('error', (err) => {
      finish({ success: false, error: err.message });
    });

    const timeoutMs = opts.timeoutMs ?? 15_000;
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      finish({ success: false, error: 'Timed out waiting for `tachi vault set`.' });
    }, timeoutMs);

    child.on('close', (code) => {
      clearTimeout(timer);
      if (code === 0) {
        finish({ success: true });
      } else {
        const detail = stderr.trim() || `exit code ${code}`;
        finish({ success: false, error: detail });
      }
    });

    // Write password first (if used), then the value, each on its own line,
    // then close stdin so the child's `read_line` calls return.
    if (!opts.useKeychain && opts.password !== undefined) {
      child.stdin?.write(opts.password + '\n');
    }
    child.stdin?.write(value + '\n');
    child.stdin?.end();
  });
}
