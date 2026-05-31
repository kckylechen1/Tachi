import { spawn, ChildProcess } from 'child_process';
import { existsSync } from 'fs';
import { join } from 'path';
import getPort from 'get-port';
import { getBinaryPath, getDataDir, loadConfig } from './config.js';
import { TACHI_CLI_VERSION } from '../version.js';

let daemonProcess: ChildProcess | null = null;

interface DaemonStatus {
  online: boolean;
  port?: number;
  memoryCount?: number;
  error?: string;
}

export async function startDaemon(projectDbPath?: string): Promise<{ success: boolean; error?: string }> {
  const binaryPath = getBinaryPath();
  
  if (!existsSync(binaryPath)) {
    return { success: false, error: `Daemon binary not found at ${binaryPath}. Run: cargo build --release -p memory-server` };
  }

  // Check if already running
  const status = await getDaemonStatus();
  if (status.online) {
    return { success: false, error: 'Daemon is already running' };
  }

  const port = await getPort({ port: 6919 });
  const config = loadConfig();
  const dataDir = getDataDir();

  return new Promise((resolve) => {
    daemonProcess = spawn(binaryPath, [], {
      env: {
        ...process.env,
        TACHI_PORT: String(port),
        TACHI_DATA_DIR: dataDir,
        ...(projectDbPath && { TACHI_PROJECT_DB: projectDbPath }),
      },
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: false,
    });

    let stdout = '';
    let stderr = '';

    daemonProcess.stdout?.on('data', (data) => {
      stdout += data.toString();
    });

    daemonProcess.stderr?.on('data', (data) => {
      stderr += data.toString();
    });

    daemonProcess.on('error', (err) => {
      resolve({ success: false, error: err.message });
    });

    // Wait for daemon to be ready
    setTimeout(async () => {
      const status = await checkDaemonHealth(port);
      if (status) {
        resolve({ success: true });
      } else {
        daemonProcess?.kill();
        resolve({ success: false, error: 'Daemon failed to start. Check logs.' });
      }
    }, 2000);
  });
}

export async function stopDaemon(): Promise<{ success: boolean; error?: string }> {
  const status = await getDaemonStatus();
  if (!status.online) {
    return { success: false, error: 'Daemon is not running' };
  }

  if (daemonProcess) {
    daemonProcess.kill('SIGTERM');
    daemonProcess = null;
    return { success: true };
  }

  // Try to find and kill process by port
  try {
    const result = await fetch(`http://127.0.0.1:${status.port}/mcp/message`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', method: 'shutdown', id: 1 }),
    });
    return { success: result.ok };
  } catch {
    return { success: false, error: 'Failed to stop daemon' };
  }
}

export async function getDaemonStatus(): Promise<DaemonStatus> {
  const config = loadConfig();
  const port = config.daemon.port;

  try {
    const response = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Accept': 'application/json',
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2024-11-05',
          capabilities: {},
          clientInfo: { name: 'tachi-cli', version: TACHI_CLI_VERSION },
        },
      }),
    });

    if (response.ok) {
      // Try to get memory count
      try {
        const statsResponse = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            jsonrpc: '2.0',
            id: 2,
            method: 'tools/call',
            params: { name: 'memory_stats', arguments: {} },
          }),
        });
        
        if (statsResponse.ok) {
          const data = await statsResponse.json();
          const result = (data as { result?: { content?: { text?: string }[] } })?.result?.content?.[0]?.text;
          if (result) {
            const parsed = JSON.parse(result);
            return { online: true, port, memoryCount: parsed.global_count || 0 };
          }
        }
      } catch {
        // Ignore stats error
      }
      
      return { online: true, port };
    }
  } catch {
    // Daemon is offline
  }

  return { online: false };
}

async function checkDaemonHealth(port: number): Promise<boolean> {
  try {
    const response = await fetch(`http://127.0.0.1:${port}/mcp/message`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2024-11-05',
          capabilities: {},
          clientInfo: { name: 'tachi-cli', version: TACHI_CLI_VERSION },
        },
      }),
    });
    return response.ok;
  } catch {
    return false;
  }
}