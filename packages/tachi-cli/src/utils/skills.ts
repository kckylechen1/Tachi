import { loadConfig } from './config.js';
import { callTachiTool, requireArray } from './tachiMcp.js';

const DEFAULT_DAEMON_PORT = 6919;

export interface Skill {
  id: string;
  name: string;
  cap_type: string;
  description?: string;
  enabled: boolean;
  version?: number;
  uses?: number;
  successes?: number;
  failures?: number;
}

export async function getSkills(): Promise<Skill[]> {
  const port = loadConfig().daemon.port || DEFAULT_DAEMON_PORT;
  const capabilities = requireArray<Skill>(
    await callTachiTool(port, 'hub_discover', { enabled_only: false }),
    'hub_discover',
  );
  return capabilities
    .filter(item => item.cap_type === 'skill')
    .map(item => ({ ...item, enabled: item.enabled !== false }));
}

export async function toggleSkill(id: string, enabled: boolean): Promise<void> {
  const port = loadConfig().daemon.port || DEFAULT_DAEMON_PORT;
  await callTachiTool(port, 'hub_set_enabled', { id, enabled });
}
