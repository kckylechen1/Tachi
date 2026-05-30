# Tachi Desktop — Electron Frontend

**Date**: 2026-03-25
**Status**: Deferred — priority shifted to memory-server + dispatch hardening; revisit when core is stable
**Branch**: `main`

## Decision

Build an Electron + Vite + React desktop application as the management UI for Tachi. The primary driver is **agent communication visualization** — Kanban board card flow and Ghost Whispers pub/sub messaging need real-time visual representation that a terminal interface cannot provide.

## Architecture

```
┌─────────────────────────────────┐
│  Tachi Desktop (Electron)       │
│  ├── React + Vite renderer      │
│  └── IPC → HTTP bridge          │
└──────────┬──────────────────────┘
           │ HTTP / StreamableHTTP
           ▼
┌─────────────────────────────────┐
│  Tachi Daemon                   │
│  tachi --daemon --port 6919     │
│  ├── Memory Store (SQLite)      │
│  ├── Hub (Skill + MCP registry) │
│  ├── MCP Connection Pool        │
│  ├── Kanban Board               │
│  └── Ghost Whispers (pub/sub)   │
└─────────────────────────────────┘
```

## Core Modules

| Module | Description | Priority |
|--------|-------------|----------|
| **Kanban Board** | Real-time agent card posting/inbox/status flow | P0 |
| **Ghost Whispers** | Pub/sub message stream visualization | P0 |
| **Memory Explorer** | Search, view, edit, delete memories | P1 |
| **Memory Graph** | Interactive graph visualization (nodes + edges) | P1 |
| **MCP Dashboard** | Registered servers, connection status, tool list | P1 |
| **Skill Manager** | Browse, search, enable/disable, visibility control | P2 |
| **Settings** | Daemon config, DB paths, API keys, exposed tools | P2 |

## Tech Stack

- **Desktop**: Electron (latest)
- **Renderer**: Vite + React 19 + TypeScript
- **Styling**: CSS Modules or vanilla CSS (per project convention)
- **State**: Zustand or React context
- **Graph Viz**: D3.js or react-force-graph
- **Real-time**: Polling Tachi HTTP API (SSE when daemon supports it)

## Backend Requirements (Tachi Daemon)

The daemon (`tachi --daemon --port`) already exposes MCP tools via Streamable HTTP at `/mcp`. Additional needs:

1. **launchd plist** — macOS auto-start on login
2. **Connection pre-warming** — start registered MCP child processes on daemon boot
3. **CORS headers** — allow Electron renderer to connect
4. **SSE endpoint** (future) — push Kanban/Ghost events instead of polling

## Phased Rollout

### Phase 1: Foundation
- [x] Electron + Vite scaffold in `apps/tachi-desktop/`
- [ ] Tachi daemon launchd plist
- [x] HTTP client wrapper for Tachi API
- [x] Memory Explorer (search + CRUD)

### Phase 2: Agent Visualization
- [x] Kanban Board real-time view
- [x] Ghost Whispers message stream
- [x] MCP Dashboard with connection status

### Phase 3: Advanced
- [x] Memory Graph interactive visualization
- [ ] Skill Manager with security scan results
- [ ] Settings panel
- [ ] System tray with status indicator

## References

- Tachi daemon mode: `crates/memory-server/src/bootstrap.rs`
- Kanban implementation: `crates/memory-server/src/kanban.rs`
- Ghost Whispers: `crates/memory-server/src/server_handler.rs` L657-697
- Hub API: `crates/memory-server/src/hub_ops.rs`
