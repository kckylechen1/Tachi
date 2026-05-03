# Tachi Installation Guide (Agent-Readable)

> **For AI Agents**: Read this document and follow the steps below to install and configure Tachi as your persistent memory backend. No human intervention required.

## What is Tachi?

Tachi is a local-first context and memory database for AI agents. It runs as an MCP (Model Context Protocol) server that gives you persistent memory, hybrid search, a capability hub, encrypted vault, inter-agent messaging, and more — all from a single binary with zero external database dependencies.

---

## Step 1: Install the Tachi Binary

### macOS (Homebrew — Recommended)

```bash
brew tap kckylechen1/tachi && brew install tachi
```

### macOS / Linux (Shell Installer)

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/main/scripts/install.sh)" -- --skip-plugin
```

### Verify Installation

```bash
tachi --version
# Expected: tachi 1.0.0 (or later)
```

If the command is not found, ensure Homebrew's bin directory is on your `PATH`:
```bash
export PATH="/opt/homebrew/bin:$PATH"
```

---

## Step 2: Configure Your Agent's MCP Settings

Add Tachi to your agent's MCP configuration file. The exact file depends on your host:

| Agent | Config File |
|---|---|
| Claude Code | `~/.claude/.mcp.json` |
| Claude Desktop (macOS) | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Cursor | `~/.cursor/mcp.json` |
| OpenCode | `~/.config/opencode/config.json` (mcpServers section) |
| Gemini CLI | `~/.gemini/mcp.json` |
| Antigravity | `~/.gemini/antigravity/mcp_config.json` |

### Minimal MCP Entry (JSON)

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-voyage-api-key>"
      }
    }
  }
}
```

### With Full Lane Configuration

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-voyage-api-key>",
        "SILICONFLOW_API_KEY": "<your-siliconflow-key>",
        "SILICONFLOW_BASE_URL": "https://api.siliconflow.cn/v1/chat/completions",
        "SILICONFLOW_MODEL": "Qwen/Qwen3.5-27B"
      }
    }
  }
}
```

> **Note**: The server also reads `.env` from the current project root at startup. You can place API keys there instead of in the MCP config. See `.env.example` in the repository for all available environment variables.

---

## Step 3: API Keys

Tachi requires at minimum one API key:

| Key | Required | Provider | Purpose |
|---|---|---|---|
| `VOYAGE_API_KEY` | **Yes** | [Voyage AI](https://dash.voyageai.com/) | Vector embeddings (Voyage-4) |
| `SILICONFLOW_API_KEY` | Recommended | [SiliconFlow](https://siliconflow.cn/) | Fact extraction, summaries (Qwen3.5-27B) |

### Optional Per-Lane Overrides

For advanced setups, you can route different LLM tasks to different providers:

| Env Prefix | Purpose | Recommended Model |
|---|---|---|
| `EXTRACT_*` | Structured fact extraction | Qwen/Qwen3.5-27B |
| `DISTILL_*` | Compaction & rollups | MiniMax-M2.7 |
| `SUMMARY_*` | Fast L0 summaries | MiniMax-M2.7 |
| `REASONING_*` | Skill evolution, planning | GLM-5.1 |

Each prefix supports `_API_KEY`, `_BASE_URL`, and `_MODEL` suffixes.

- **EXTRACT / SUMMARY** — if omitted, fall back to `SILICONFLOW_*`.
- **DISTILL ↔ REASONING** — these two lanes cross-fallback to each other; they do *not* fall back to `SILICONFLOW_*`.
  To use Foundry LLM features (distillation, skill evolution, planning), you must configure at least one of `DISTILL_*` or `REASONING_*`.

---

## Step 4: Verify

After configuring, restart your agent and run:

```
Search my memory for "test"
```

If Tachi is working, you will see a response from the `search_memory` tool (even if no results are found yet). If the tool is not available, check:

1. The MCP config file path is correct for your agent
2. The `tachi` binary is on your PATH
3. Your VOYAGE_API_KEY is set (embedding is required for save operations)

---

## Database Locations

Tachi uses SQLite with zero external dependencies:

| Database | Path | Purpose |
|---|---|---|
| Global | `~/.tachi/global/memory.db` | Cross-project memories, user preferences |
| Project | `.tachi/memory.db` (at git root) | Project-specific context, auto-detected |

> **Safety**: Never place database files in cloud-synced folders (iCloud, Dropbox, OneDrive). SQLite WAL mode is incompatible with network filesystems.

---

## OpenClaw Integration (Optional)

If you use OpenClaw, the full installer configures both the binary and the plugin:

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/main/scripts/install.sh)"
```

This will:
- Install/upgrade `tachi` via Homebrew
- Download and install the OpenClaw `tachi` plugin to `~/.openclaw/extensions/tachi`
- Update `~/.openclaw/openclaw.json` with plugin allow-list and memory slot

---

## Available MCP Tools (120+)

Once connected, Tachi exposes 120+ MCP tools organized into layers. Most hosts should use a profile so they see a compact task-focused surface instead of the full admin catalog.

### Core Memory
`save_memory`, `search_memory`, `get_memory`, `list_memories`, `delete_memory`, `archive_memory`, `memory_stats`, `memory_gc`, `remember`, `find_similar_memory`
*(Aliases: `cyberbrain_write`, `cyberbrain_search`)*

### Knowledge Graph & Domains
`add_edge`, `get_edges`, `memory_graph`
`register_domain`, `get_domain`, `list_domains`, `delete_domain`

### State & Config
`set_state`, `get_state`

### Extraction & Ingestion
`extract_facts`, `ingest_event`, `ingest`, `ingest_source`

### Neural Foundry (Context Lifecycle & Evolution)
`recall_context`, `capture_session`, `compact_context`, `section_build`, `compact_rollup`, `compact_session_memory`
`synthesize_agent_evolution`, `queue_agent_evolution`, `list_agent_evolution_proposals`, `review_agent_evolution_proposal`, `project_agent_profile`

### Capability Hub
`hub_register`, `hub_discover`, `hub_get`, `hub_call`, `hub_feedback`, `hub_stats`, `hub_set_enabled`, `hub_disconnect`, `hub_review`, `hub_set_active_version`, `hub_export_skills`
*(Aliases: `section9_review`)*

### Skill Packs
`pack_register`, `pack_list`, `pack_get`, `pack_project`, `pack_remove`, `projection_list`

### Virtual Capabilities
`vc_register`, `vc_bind`, `vc_resolve`, `vc_list`

### Ghost Whispers (Pub/Sub)
`ghost_publish`, `ghost_subscribe`, `ghost_topics`, `ghost_ack`, `ghost_reflect`, `ghost_promote`
*(Aliases: `ghost_whisper`, `ghost_listen`, `ghost_channels`)*

### Agent Identity
`agent_register`, `agent_whoami`, `handoff_leave`, `handoff_check`

### Kanban (Inter-Agent)
`post_card`, `check_inbox`, `update_card`

### Vault (Encrypted Secrets)
`vault_init`, `vault_unlock`, `vault_lock`, `vault_set`, `vault_get`, `vault_list`, `vault_remove`, `vault_status`, `vault_setup_rotation`

### Sandbox & Governance
`sandbox_set_rule`, `sandbox_check`, `sandbox_get_policy`, `sandbox_set_policy`, `sandbox_list_policies`, `sandbox_exec_audit`
*(Aliases: `shell_set_policy`, `shell_get_policy`, `shell_list_policies`, `shell_exec_audit`)*

### Recommendations
`recommend_capability`, `recommend_skill`, `recommend_toolchain`, `prepare_capability_bundle`

### Facade & Delegation
`tachi_search`, `tachi_web_search`, `tachi_save`, `tachi_handoff`, `tachi_plan`, `tachi_unstick`, `tachi_browse`, `tachi_dispatch`, `approve_merge`, `tachi_complete`
*(Aliases for real tools: `tachi_task_brief`, `tachi_progress_check`)*

### Wiki System
`wiki_lint`, `tachi_wiki_write`, `tachi_wiki_search`, `wiki_search`, `wiki_browse`

### Utilities
`skill_evolve`, `run_skill`, `chain_skills`, `sync_memories`, `tachi_init_project_db`, `tachi_audit_log`, `section9_audit_log`, `dlq_list`, `dlq_retry`, `get_pipeline_status`, `tachi_doctor_scan`

### Tool Surface Selection

Tachi does not need to expose the full tool catalog to every host. Use `--profile` or `TACHI_PROFILE` to select an additive surface bundle:

- `standard` — Curated Tool Surface v2 for IDEs and CLI agents (11 tools: tachi_search, tachi_save, run_skill, tachi_dispatch, etc.)
- `delegate` — Minimal surface for worker agents spawned by `tachi_dispatch` (7 tools)
- `observe` — Read-only memory + capability recommendation
- `remember` — `observe` + `save_memory`, `extract_facts`, `run_skill`
- `coordinate` — `remember` + ghost / kanban / handoff tools
- `operate` — `remember` + runtime hooks and gateway/evolution helpers
- `admin` — Full surface (120+ tools)

Host aliases:

- `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`

Examples:

```bash
tachi --profile standard
TACHI_PROFILE=claude-code tachi
```

If no profile is specified, Tachi keeps the historical `admin` default for compatibility. New host integrations should explicitly set a profile like `standard`.

---

## Troubleshooting

| Problem | Solution |
|---|---|
| `tachi: command not found` | Run `brew tap kckylechen1/tachi && brew install tachi`, then verify PATH |
| MCP tools not appearing | Check your agent's MCP config file path and JSON syntax |
| `save_memory` fails | Ensure `VOYAGE_API_KEY` is set (required for embedding) |
| Search returns no results | Memory store is empty — save some memories first |
| Database locked errors | Only one Tachi instance should access each database file |

---

## Uninstall

```bash
brew uninstall tachi && brew untap kckylechen1/tachi
```

To also remove data:
```bash
rm -rf ~/.tachi
```
