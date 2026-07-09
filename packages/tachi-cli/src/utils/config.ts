import { writeFileSync, readFileSync, existsSync, mkdirSync } from 'fs';
import { join, delimiter } from 'path';
import { homedir } from 'os';
import { dump as yamlDump, load as yamlLoad } from 'js-yaml';

export interface Config {
  daemon: {
    port: number;
    autoStart: boolean;
  };
  ui: {
    language: 'en' | 'zh';
    theme: 'dark' | 'light';
  };
  paths: {
    dataDir: string;
    projectDb?: string;
  };
}

const defaultConfig: Config = {
  daemon: {
    port: 6919,
    autoStart: true,
  },
  ui: {
    language: 'en',
    theme: 'dark',
  },
  paths: {
    dataDir: join(homedir(), '.tachi'),
  },
};

const configDir = join(homedir(), '.tachi');
const configPath = join(configDir, 'config.yaml');

export async function initConfig(): Promise<void> {
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }
  
  if (!existsSync(configPath)) {
    saveConfig(defaultConfig);
  }
}

export function loadConfig(): Config {
  try {
    const content = readFileSync(configPath, 'utf-8');
    const parsed = yamlLoad(content) as Partial<Config>;
    return { ...defaultConfig, ...parsed };
  } catch {
    return defaultConfig;
  }
}

export function saveConfig(config: Config): void {
  const yamlContent = yamlDump(config);
  writeFileSync(configPath, yamlContent, 'utf-8');
}

export function getDataDir(): string {
  const config = loadConfig();
  return config.paths.dataDir;
}

function findOnPath(name: string): string | undefined {
  // On Windows, executables resolve via PATHEXT extensions (.exe/.cmd/.bat);
  // elsewhere the bare name is used.
  const extensions =
    process.platform === 'win32'
      ? ['', ...(process.env.PATHEXT ?? '.EXE;.CMD;.BAT').split(delimiter)]
      : [''];
  for (const dir of (process.env.PATH ?? '').split(delimiter)) {
    if (!dir) continue;
    for (const ext of extensions) {
      const candidate = join(dir, `${name}${ext}`);
      if (existsSync(candidate)) return candidate;
    }
  }
  return undefined;
}

/**
 * Resolve the real `tachi` binary (installed as `tachi`, usually a symlink to
 * `memory-server`). The previous default — `~/.tachi/bin/tachi-daemon` — was a
 * path nothing installs, so every daemon command failed. Resolution order:
 *   1. `TACHI_BINARY` env override
 *   2. `tachi` / `memory-server` on `PATH`
 *   3. common cargo / home install locations
 *   4. bare `tachi`, leaving final resolution to the OS at spawn time
 */
export function getBinaryPath(): string {
  const override = process.env.TACHI_BINARY;
  if (override && existsSync(override)) return override;

  const onPath = findOnPath('tachi') ?? findOnPath('memory-server');
  if (onPath) return onPath;

  const candidates = [
    join(homedir(), '.cargo', 'bin', 'memory-server'),
    join(homedir(), 'bin', 'tachi'),
    join(homedir(), '.tachi', 'bin', 'tachi'),
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }

  return 'tachi';
}