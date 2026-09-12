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

const configDir = join(homedir(), '.tachi');
const configPath = join(configDir, 'config.yaml');

function createDefaultConfig(): Config {
  return {
    daemon: {
      port: 6919,
      autoStart: true,
    },
    ui: {
      language: 'en',
      theme: 'dark',
    },
    paths: {
      dataDir: configDir,
    },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Object.prototype.toString.call(value) === '[object Object]';
}

function parseConfig(value: unknown): Config {
  const config = createDefaultConfig();
  if (!isRecord(value)) return config;

  if (isRecord(value.daemon)) {
    const { port, autoStart } = value.daemon;
    if (Number.isInteger(port) && (port as number) >= 1 && (port as number) <= 65535) {
      config.daemon.port = port as number;
    }
    if (typeof autoStart === 'boolean') config.daemon.autoStart = autoStart;
  }

  if (isRecord(value.ui)) {
    const { language, theme } = value.ui;
    if (language === 'en' || language === 'zh') config.ui.language = language;
    if (theme === 'dark' || theme === 'light') config.ui.theme = theme;
  }

  if (isRecord(value.paths)) {
    const { dataDir, projectDb } = value.paths;
    if (typeof dataDir === 'string') config.paths.dataDir = dataDir;
    if (typeof projectDb === 'string') config.paths.projectDb = projectDb;
  }

  return config;
}

export async function initConfig(): Promise<void> {
  if (!existsSync(configDir)) {
    mkdirSync(configDir, { recursive: true });
  }
  
  if (!existsSync(configPath)) {
    saveConfig(createDefaultConfig());
  }
}

export function loadConfig(): Config {
  try {
    const content = readFileSync(configPath, 'utf-8');
    return parseConfig(yamlLoad(content));
  } catch {
    return createDefaultConfig();
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
 * `tachi-server`). The previous default — `~/.tachi/bin/tachi-daemon` — was a
 * path nothing installs, so every daemon command failed. Resolution order:
 *   1. `TACHI_BINARY` env override
 *   2. `tachi` / `tachi-server` on `PATH`
 *   3. common cargo / home install locations
 *   4. bare `tachi`, leaving final resolution to the OS at spawn time
 */
export function getBinaryPath(): string {
  const override = process.env.TACHI_BINARY;
  if (override && existsSync(override)) return override;

  const onPath = findOnPath('tachi') ?? findOnPath('tachi-server');
  if (onPath) return onPath;

  const candidates = [
    join(homedir(), '.cargo', 'bin', 'tachi-server'),
    join(homedir(), 'bin', 'tachi'),
    join(homedir(), '.tachi', 'bin', 'tachi'),
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }

  return 'tachi';
}
