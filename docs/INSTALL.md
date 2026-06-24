# Tachi Installation Guide (Agent-Readable)

> **For AI Agents**: Read this document and follow the steps below to install and configure Tachi as your persistent memory backend. No human intervention required.

## What is Tachi?

Tachi is a local-first context and memory database for AI agents. It runs as an MCP (Model Context Protocol) server that gives you persistent memory, hybrid search, a capability hub, encrypted vault, inter-agent messaging, workflow dispatch, verification ledgers, and more — all from a single binary with zero external database dependencies.

Current architecture:

- `memory-core` owns SQLite storage, migrations, hybrid search, graph/domain state, packs, vault metadata, and sqlite-vec integration.
- `memory-server` owns the MCP/CLI surface, profile filtering, Hub routing, dispatch/workflow tools, wiki operations, vault encryption, daemon locking, Foundry background work, and verification/arena/eval ledgers.
- Hosts should connect through the profile-based MCP surface instead of exposing every low-level tool by default.

---

## Step 1: Install the Tachi Binary

### macOS (Homebrew — Recommended)

```bash
brew tap kckylechen1/tachi && brew install tachi
```

### macOS / Linux (Shell Installer)

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.6/scripts/install.sh)" -- --skip-plugin
```

### Verify Installation

```bash
tachi --version
# Expected: tachi 1.5.6 (or later)
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
| Gemini CLI | `~/.gemini/mcp.json` |
| Antigravity | `~/.gemini/antigravity/mcp_config.json` |

### Minimal MCP Entry (JSON)

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-voyage-api-key>",
        "TACHI_PROFILE": "standard"
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
        "SILICONFLOW_MODEL": "Qwen/Qwen3.5-27B",
        "TACHI_PROFILE": "standard"
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
| `SILICONFLOW_API_KEY` | Recommended | [SiliconFlow](https://siliconflow.cn/) | Fact extraction, summaries, foundry distillation (Qwen3.5-27B) |

Phase 2 simplified the lane model. Background skill and foundry calls now prefer the **Claude CLI pool** and fall back to `SILICONFLOW_*` on error. For most deployments, only `VOYAGE_API_KEY` + `SILICONFLOW_API_KEY` are needed.

### Optional Per-Lane Overrides

For advanced setups, you can still route different LLM tasks to different providers:

| Env Prefix | Purpose | Notes |
|---|---|---|
| `EXTRACT_*` | Structured fact extraction | Falls back to `SILICONFLOW_*` when omitted. |
| `DISTILL_*` | Compaction & rollups | **Deprecated in Phase 2**. Kept for backward compatibility only. |
| `SUMMARY_*` | Fast L0 summaries | Falls back to `SILICONFLOW_*` when omitted. |
| `REASONING_*` | Skill evolution, planning | **Deprecated in Phase 2**. Kept for backward compatibility only. |

Each prefix supports `_API_KEY`, `_BASE_URL`, and `_MODEL` suffixes.

---

## Step 4: Verify

After configuring, restart your agent and run:

```
Search my memory for "test"
```

If Tachi is working, you will see a response from `tachi_search` or the profile's search tool (even if no results are found yet). If the tool is not available, check:

1. The MCP config file path is correct for your agent
2. The `tachi` binary is on your PATH
3. Your `TACHI_PROFILE` is appropriate for the host (`standard`, `coordinate`, `operate`, or `admin`)
4. Your `VOYAGE_API_KEY` is set (embedding is required for save operations)

---

## Database Locations

Tachi uses SQLite with zero external dependencies:

| Database | Path | Purpose |
|---|---|---|
| Global | `~/.tachi/global/memory.db` | Cross-project memories, user preferences |
| Project | `.tachi/memory.db` (at git root) | Project-specific context, auto-detected |

> **Safety**: Never place database files in cloud-synced folders (iCloud, Dropbox, OneDrive). SQLite WAL mode is incompatible with network filesystems.

Plan C project routing keeps the repo-local project DB as canonical and uses
`~/.tachi/projects/<sanitized-repo-name>/memory.db` only as a symlink alias.
If `tachi status`, `tachi_memory(action="readiness")`, or daemon startup reports
“Plan C split-brain”, the alias is a stale regular SQLite file. Repair it
explicitly:

```bash
tachi repair --rule R11 --dry-run --db <label-from-status>
tachi repair --rule R11 --apply --db <label-from-status>
```

The repair backs up the regular alias DB, copies alias-only memory ids into the
repo-local DB without overwriting existing canonical ids, rebuilds FTS, then
replaces the alias file with a symlink to `.tachi/memory.db`.

### Database Safety Rules

| Rule | Why |
|------|-----|
| **Single instance per DB** | The server acquires an exclusive file lock (`memory.db.lock`) at startup. Only one Tachi process should write to a given database file. |
| **No cloud-synced paths** | iCloud, Dropbox, OneDrive, and Google Drive are **incompatible** with SQLite WAL. Use a local-only directory (e.g., `~/.tachi/`). |
| **No concurrent CLI writes** | Do not run `sqlite3` INSERT/UPDATE on the database while the server is running. Read-only queries are safe with `PRAGMA busy_timeout = 5000`. |
| **Auto-recovery on startup** | The server runs `PRAGMA quick_check` on startup and auto-backfills an empty FTS index from the main `memories` table. |
| **Graceful shutdown** | The server handles SIGINT/SIGTERM to flush WAL and run `PRAGMA optimize` before exit. Avoid `kill -9`. |

Live SQLite databases should stay local. Sync encrypted bundles, append-only event logs, vault ciphertext, workflow summaries, wiki/skill artifacts, and performance aggregates instead.

---

## OpenClaw Integration (Optional)

If you use OpenClaw, the full installer configures both the binary and the plugin:

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.6/scripts/install.sh)"
```

This will:

- Install/upgrade `tachi` via Homebrew
- Download and install the OpenClaw `tachi` plugin to `~/.openclaw/extensions/tachi`
- Update `~/.openclaw/openclaw.json` with plugin allow-list and memory slot

### No-project MCP serve isolation

For host-spawned MCP processes that should use only the global Tachi runtime, launch:

```bash
tachi --no-project-db --profile openclaw serve
```

In this mode Tachi treats `serve` as a global runtime entrypoint:

- The process cwd is moved to `~/.tachi/runtime` before project discovery.
- Project-local `.env`, `.tachi/config.env`, and `.sigil/config.env` files are skipped.
- No project DB is opened or inferred from the launch directory.
- Global config remains allowed through `~/.tachi/config.env`, `~/.sigil/config.env`, `~/.secrets/master.env`, explicit CLI flags, and inherited environment variables.

This keeps OpenClaw, Claude Code, and other MCP clients from inheriting Desktop or repository paths simply because the host process was launched from that directory. Use an explicit `--project-db PATH` without `--no-project-db` when a host intentionally needs a project-scoped Tachi instance.

---

## Available MCP Tools

Once connected, Tachi exposes a profile-filtered MCP surface. The full `admin` catalog is intentionally large; most hosts should use `standard`, `coordinate`, or `operate` so agents see a compact task-focused surface.

### Core Memory

`save_memory`, `search_memory`, `get_memory`, `list_memories`, `delete_memory`, `archive_memory`, `memory_stats`, `memory_gc`, `remember`, `find_similar_memory`

*(Aliases: `cyberbrain_write`, `cyberbrain_search`)*

### Knowledge Graph & Domains

`add_edge`, `get_edges`, `memory_graph`
`register_domain`, `get_domain`, `list_domains`, `delete_domain`

### State & Config

`set_state`, `get_state`, `runtime_info`

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

### Agent Identity & Handoff

`agent_register`, `agent_whoami`, `handoff_leave`, `handoff_check`

### Kanban (Inter-Agent)

`post_card`, `check_inbox`, `update_card`

### Vault (Encrypted Secrets)

`vault_init`, `vault_unlock`, `vault_lock`, `vault_set`, `vault_get`, `vault_list`, `vault_remove`, `vault_status`, `vault_setup_rotation`

Project-local agent dispatch can reference shared Vault secrets without copying plaintext keys. Put aliases in `.tachi/vault.env` under the project root:

```dotenv
LONGPORT_APP_SECRET=vault:longbridge.1.secret
GOOGLE_API_KEY=vault:google.1.api_key
```

When `tachi_dispatch` runs with `cwd` inside that project, the child agent process receives the resolved env values from the unlocked Tachi Vault.

#### Project env CLI (`tachi env`)

For direnv-style workflows, materialize project-bound secrets into a local file without committing plaintext keys:

```bash
# Inspect bindings (no decryption)
tachi env plan

# Print shell exports to stdout (for eval)
tachi env export --keychain

# Preview sync target (default — no file write)
tachi env sync --keychain

# Write .tachi/env.generated (0600, atomic rename)
tachi env sync --apply --keychain

# Overwrite an existing generated file
tachi env sync --apply --force --keychain

# Run a command with project secrets injected
tachi env run -- cargo test
```

Safety defaults:

- **`sync` is preview-only** unless you pass `--apply`. This prevents accidental plaintext secret writes.
- Output lives at `.tachi/env.generated` with a `DO NOT COMMIT plaintext secrets` header.
- Files are written with mode `0600` on Unix hosts.
- `tachi doctor` warns if `.tachi/env.generated` is git-tracked.

Add `.tachi/env.generated` to `.gitignore`. Keep only `.tachi/vault.env` (vault aliases) in version control.

#### DLQ and dispatch safety

- **`dlq_list` / `dlq_retry`**: DLQ replay skips mutating tools by default. Only idempotent read/search-style failures are safe to retry automatically.
- **Bare dispatch dedupe**: repeated identical `tachi_dispatch` tasks without a `flow_id` coalesce via fingerprint locks under `~/.tachi/runs/.dispatch-dedupe/`.
- **Crash recovery**: restarting the daemon reconciles orphaned dispatch runs left by abrupt process exit.

See also: [`docs/engineering/architecture/safety-hardening-2026-06.md`](engineering/architecture/safety-hardening-2026-06.md).

### Sandbox & Governance

`sandbox_set_rule`, `sandbox_check`, `sandbox_get_policy`, `sandbox_set_policy`, `sandbox_list_policies`, `sandbox_exec_audit`

*(Aliases: `shell_set_policy`, `shell_get_policy`, `shell_list_policies`, `shell_exec_audit`)*

### Recommendations

`recommend_capability`, `recommend_skill`, `recommend_toolchain`, `prepare_capability_bundle`

### Facade & Delegation

`tachi_search`, `tachi_web_search`, `tachi_save`, `tachi_handoff`, `tachi_plan`, `tachi_unstick`, `tachi_browse`, `tachi_dispatch`, `tachi_task`, `tachi_shell`, `approve_merge`, `tachi_complete`, `tachi_arena`, `tachi_verify`, `tachi_agent_eval`

*(Aliases for real tools: `tachi_task_brief`, `tachi_progress_check`)*

### Wiki System

`wiki_lint`, `tachi_wiki_write`, `tachi_wiki_search`, `wiki_search`, `wiki_browse`

### Utilities

`skill_evolve`, `run_skill`, `chain_skills`, `sync_memories`, `tachi_init_project_db`, `tachi_audit_log`, `section9_audit_log`, `dlq_list`, `dlq_retry`, `get_pipeline_status`, `tachi_doctor_scan`

---

## Tool Surface Selection

Tachi does not need to expose the full tool catalog to every host. Use `--profile` or `TACHI_PROFILE` to select an additive surface bundle:

| Profile | Exposed surface | Best for |
|---|---|---|
| `standard` | Curated 12-tool facade surface: `tachi_search`, `tachi_save`, `tachi_memory`, `tachi_task`, `tachi_arena`, `tachi_verify`, `tachi_agent_eval`, `tachi_web_search`, `tachi_wiki`, `tachi_skill`, `tachi_gh`, `vault_status`, plus `runtime_info` and `tachi_tools`. | IDE agents (Claude, Cursor, Codex, Windsurf, Trae, Antigravity). |
| `coordinate` | `remember` + `coordinate` bundles: adds `handoff_*`, `post_card`, `check_inbox`, `update_card`, `tachi_dispatch`, `approve_merge`, `tachi_handoff`, `tachi_workflow`, `tachi_orchestrator`. | Leader/orchestrator agents. |
| `operate` | `remember` + `operate` bundles: adds Foundry lifecycle, `agent_register`, `hub_call`, `vault_unlock`/`lock`/`status`, `wiki_lint`. | Runtime adapters and OpenClaw. |
| `delegate` | Curated 7-tool surface: `tachi_tools`, `runtime_info`, `tachi_memory`, `tachi_web_search`, `tachi_browse`, `tachi_unstick`, `tachi_complete`, `run_skill`. | Worker subagents spawned by `tachi_dispatch`. |
| `admin` | Full catalog. | Maintenance and development. |

Host aliases:

- `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`

Examples:

```bash
tachi --profile standard
TACHI_PROFILE=claude-code tachi
```

If no profile is specified, Tachi defaults to `standard`. Set `TACHI_PROFILE=admin` only for maintenance sessions that need the full low-level catalog.

---

## Troubleshooting

| Problem | Solution |
|---|---|
| `tachi: command not found` | Run `brew tap kckylechen1/tachi && brew install tachi`, then verify PATH |
| MCP tools not appearing | Check your agent's MCP config file path and JSON syntax |
| `save_memory` fails | Ensure `VOYAGE_API_KEY` is set (required for embedding) |
| Search returns no results | Memory store is empty — save some memories first |
| Database locked errors | Only one Tachi instance should access each database file |
| Profile blocks a needed tool | Set `TACHI_PROFILE=admin` temporarily, or add the tool name to `TACHI_EXTRA_TOOLS` |

---

## Uninstall

```bash
brew uninstall tachi && brew untap kckylechen1/tachi
```

To also remove data:

```bash
rm -rf ~/.tachi
```
