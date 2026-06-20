import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

/**
 * Single source of truth for the CLI version: read it straight from
 * package.json at runtime so `--version`, the banner, the in-app label, and the
 * MCP clientInfo can never silently drift (this used to be a hand-maintained
 * constant that was left stale at 1.4.2 while package.json moved on).
 */
function resolvePackageVersion(): string {
  try {
    // version.{ts,js} always sits one level below the package root
    // (src/ in dev, dist/ when built), so ../package.json resolves in both.
    const here = dirname(fileURLToPath(import.meta.url));
    const pkg = JSON.parse(readFileSync(join(here, '..', 'package.json'), 'utf-8')) as {
      version?: string;
    };
    return pkg.version ?? '0.0.0';
  } catch {
    return '0.0.0';
  }
}

export const TACHI_CLI_VERSION = resolvePackageVersion();
