<div align="center">
  <img src="assets/banner_en.png" alt="Tachi Banner" width="800" style="margin-bottom: 20px;" />
  <h1>✧ Tachi</h1>
  <p><strong>A Fast, Local-First Context & Memory Database for Autonomous AI Agents</strong></p>

  <p>
    <a href="README.en.md"><b>English</b></a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.md">文言文</a>
  </p>

  <p>
    <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/License-AGPLv3-blue.svg" alt="License: AGPLv3"></a>
    <img src="https://img.shields.io/badge/Language-Rust_Edition_2021-orange.svg" alt="Language: Rust">
    <img src="https://img.shields.io/badge/Integration-MCP_Server-purple" alt="Integration: MCP">
    <img src="https://img.shields.io/badge/Integration-OpenClaw-cyan" alt="Integration: OpenClaw">
    <img src="https://img.shields.io/github/v/release/kckylechen1/tachi.svg" alt="Release Version">
  </p>
</div>

---

## 📖 Table of Contents

- [Overview](#-overview)
- [Why Tachi?](#-why-tachi)
- [Quick Start: Coding Agents (MCP)](#-quick-start-coding-agents-mcp)
- [Quick Start: Frameworks (OpenClaw)](#-quick-start-frameworks-openclaw)
- [Key Features](#-key-features)
- [Causal Worker Pipeline & Memory Relations](#-causal-worker-pipeline--memory-relations)
- [Architecture](#-architecture)
- [Model Stack](#-model-stack)
- [Manual Installation & APIs](#-manual-installation--apis)
- [Environment Configuration](#-environment-configuration)
- [Benchmarks](#-benchmarks)
- [Contributing](#-contributing)
- [License](#-license)

---

## 💡 Overview

**Tachi** is an embedded, unified context and memory management database engineered for Autonomous AI Agents. Named after Ghost in the Shell's Tachikoma — AI units that evolve through shared memory.

Standard memory models often rely on flat vector stores, leading to bloated context windows and a loss of temporal and causal relationships. Tachi addresses this by utilizing a **hierarchical, file-system-like paradigm** combined with **graph-based causal relations**, powered by a highly optimized Rust core. 

Whether integrated as a [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server or used as a native extension in frameworks like OpenClaw, Tachi delivers sub-millisecond, multi-modal semantic retrieval with **zero external database dependencies**.

---

## 🎯 Why Tachi?

1. **Solving "Context Bloat" and "Causal Amnesia"**
Traditional agents dump unstructured fragments into flat vector databases (like Chroma or Pinecone). Over time, this leads to bloated LLM context windows and a complete loss of logical sequencing. Tachi counteracts this with hierarchical namespaces, 3-tier adaptive summarization (L0/L1/L2), and Knowledge Graph Edges — transforming scattered text into a structured, evolving "Digital Hippocampus".

2. **Uncompromising Local Performance & Data Sovereignty**
An AI's long-term operational memory is its most sensitive asset. Tachi is 100% local, powered by an ultra-fast Rust core (`sqlite-vec` + native FTS5) with zero network database dependencies. Its dual-DB architecture safely isolates global user preferences from project-specific knowledge bases, all while delivering sub-millisecond retrieval vectors out of the box.

3. **Ending the MCP "Process Chaos"**
In a multi-agent future, having every agent spawn and manage its own fragile MCP child processes leads to massive resource waste, port conflicts, and zombie processes. The Tachi Hub acts as a unified Proxy and Capability Registry. Register a tool once, and every agent magically shares connections, idle-cleanup, circuit breakers, and sanitized resilient network environments.

4. **"Pristine" Memory Lifecycle**
Long-running autonomous operations require extreme hygiene to prevent hallucination drift. Tachi introduces rigorous lifecycle management: pre-save AI noise filtering (`is_noise_text`), automated background garbage collection (GC), and CASCADE hard deletions (`delete_memory`). This ensures the memory store stays pristine and contextually relevant after months of continuous operations.

5. **Skill Slots: On-Demand "Neural Extensions"**
Tachi isn't just about routing tools; it's about standardizing complex workflows. Through "Skill Slots" (`Skill-as-a-Tool`), developers can encapsulate prompt chains, SOPs, and domain-specific knowledge into simple markdown files. Tachi automatically compiles these into Native MCP tools. Agents are no longer weighed down by massive system prompts—they simply "plug into" Tachi to instantly acquire on-demand professional skills.

---

## 🤖 Quick Start: Coding Agents (MCP)

For environments like Claude Desktop, Cursor, OpenCode, Gemini CLI, or AutoGen.

### Option 1: Agent-Driven Install (paste this into your AI assistant)

> Copy the URL below into your AI assistant's chat. It will read the installation guide and configure everything automatically.
>
> ```
> https://raw.githubusercontent.com/kckylechen1/tachi/main/docs/INSTALL.md
> ```

### Option 2: One-Line Install (Terminal)

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/main/scripts/install.sh)"
```

The installer will:
- Install `tachi` via Homebrew (or download a prebuilt binary)
- Auto-detect local AI agent configs (Claude, Cursor, Gemini, OpenCode, etc.)
- Inject the Tachi MCP server entry into each detected config

### Option 3: Manual Homebrew

```bash
brew tap kckylechen1/tachi && brew install tachi
```

Then add to your agent's MCP config:
```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "TACHI_PROFILE": "coordinate"
      }
    }
  }
}
```

> **Default profile changed in v1.0.1**: If `TACHI_PROFILE` is not set, Tachi now defaults to `standard` (12 curated tools) instead of `admin` (all 148 tools). For most agents, `standard` or `coordinate` is sufficient. Set `TACHI_PROFILE=admin` only if you need raw access to vault, hub, and low-level tools.
>
> **Note**: Setting `"TACHI_PROFILE": "coordinate"` (or `"remember"`) ensures that standard Agent/IDE sidebars (like Cursor/Claude Desktop) are not cluttered with hundreds of raw Tachi skills.
> 
> **🤖 Prompting Tip for IDE Agents**: Because individual skills are hidden in gateway mode, less capable AI assistants might not know how to invoke them organically. We strongly recommend appending the following strict instruction to your IDE's Custom Instructions or `.cursorrules`:
> *"You have access to the Tachi MCP Server. You cannot see individual skills natively. When facing a complex task, you MUST first use the `recommend_skill` tool to search for an appropriate workflow capability. Once you find it, you MUST use the `run_skill` tool to execute it explicitly."*
>
> **Note**: The server loads `.env` from the project root automatically. See `.env.example` for all configuration options.
>
> **Database Safety**: Never place the database in a cloud-synced folder (iCloud, Dropbox, OneDrive). SQLite WAL mode is incompatible with network filesystems.

---

## 🦞 Quick Start: Frameworks (OpenClaw)

Tachi can be integrated as a native OpenClaw extension plugin.

**Prompt your OpenClaw IDE Assistant with the following instructions:**

```text
Please install the Tachi memory extension for OpenClaw:

1. One-command install for Tachi + the OpenClaw plugin (recommended):
   bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/main/scripts/install.sh)"

   The installer will:
   - install or upgrade `tachi` through Homebrew
   - download and install the OpenClaw `tachi` plugin
   - auto-update `~/.openclaw/openclaw.json` when present, including `plugins.allow`, `plugins.load.paths`, and `plugins.slots.memory = "tachi"`

   Note: The legacy Node.js bridge has been removed as of v1.0.0. The OpenClaw plugin now strictly communicates via the native Tachi MCP binary over stdio.

   Optional: auto-detect local agent configs and inject a Tachi MCP entry:
   python3 scripts/setup_agent_mcp.py --apply

   Optional: auto-register local skills/MCPs into Hub:
   python3 scripts/load_skills_to_hub.py
   python3 scripts/register_mcps_to_hub.py
   # By default this also syncs local agent mcp configs and consolidates direct MCP entries registered in Hub under tachi
   # Skip this behavior with: python3 scripts/register_mcps_to_hub.py --no-sync-agent-config

2. If the installer cannot locate `openclaw.json`, confirm manually:
   - `plugins.allow` includes `tachi`
   - `plugins.slots.memory` is set to `tachi`

3. Configure API keys in the project root `.env` file (see `.env.example`):
   - `VOYAGE_API_KEY` for embedding + rerank
   - `SILICONFLOW_API_KEY` for the default extraction lane
   - Optional per-lane overrides: `EXTRACT_*`, `DISTILL_*`, `SUMMARY_*`, `REASONING_*`

Operational notes:
- Current OpenClaw runtime topology is per-agent: `data/agents/<agent>/memory.db`.
- The root `data/memory.db` is legacy-only and should be treated as historical or migration data, not the active write target.
- If you want the latest local binary after a release, prefer `brew reinstall tachi` after the tap formula is updated, or point agents directly at a freshly built `target/release/memory-server`.
```

---

## ✨ Key Features

- **⚡ High-Performance Rust Core (`memory-core`)**: The foundational scoring, storage, entity extraction, and retrieval engines are written in Rust, featuring dynamic bindings for Python (`PyO3`). The legacy Node.js (`NAPI-RS`) bridge was removed in v1.0.0, and plugins like OpenClaw now communicate exclusively via the stable cross-platform MCP stdio protocol. Built-in tools plus registered proxy/skill tools are exposed dynamically.
- **🗂️ Filesystem Paradigm**: Context is managed hierarchically via a `path` parameter (e.g., `/user/preferences`, `/project/architecture`), allowing precise isolation and contextual scoping.
- **🔍 3-Channel Hybrid Search Engine**:
  - **Semantic**: Built-in vector embedding search via `sqlite-vec` (KNN).
  - **Lexical**: Native CJK-optimized full-text search utilizing `libsimple` and `FTS5`.
  - **Decay**: Temporal relevance degradation inspired by the ACT-R cognitive architecture.
- **🔒 Hard State Engine**: Introduced a deterministic Key-Value store independent of vector memory. Useful for tracking trading watchlists or rigid state.
- **🧠 3-Tier Context Extraction**: Automatically parses ingestion into three tiers: `L0` (Abstract Summary), `L1` (Overview), and `L2` (Full Text). Agents dynamically retrieve the appropriate depth based on context constraints.
- **🔄 Evolution Deduplication**: Utilizing math-based similarities for `HARD_SKIP` and `EVOLVE` updates.
- **🔌 Dual-DB Architecture**: Global memories (`~/.Tachi/global/memory.db`) shared across all projects, plus per-project memories (`.Tachi/memory.db` at git root) for project-scoped context. Automatic git root detection and legacy migration. No external databases required.
- **🎯 Tachi Hub**: A unified capability registry for Skills, Plugins, and MCP Servers. Register once, discover from any agent. Includes usage tracking, feedback metrics, and dual-DB inheritance (project overrides global). Preloaded skill count is runtime-dependent (based on installed skill packs).
- **🔀 MCP Proxy**: Register child MCP servers once in Tachi — with `tool_exposure=flatten` they appear as `server__tool`, or with `tool_exposure=gateway` they stay compact behind `hub_call`. Shared connection pool with lazy-connect, idle cleanup, circuit breaker, and per-child concurrency control. Sanitized env with 21 preserved system variables. Transport aliases (`http`, `streamable-http` → `sse`). No more zombie processes.
- **🗑️ Memory Lifecycle Management**: Full lifecycle control with `delete_memory` (permanent removal with CASCADE cleanup), `archive_memory` (soft-delete with recovery), and `memory_gc` (prune stale access history, old events, and audit log entries). Each memory entry supports a `retention_policy` field (`Ephemeral`, `Durable`, `Permanent`, `Pinned`) — `Permanent` and `Pinned` entries are exempt from garbage collection.
- **🏷️ Domain-Aware Routing**: Register domains via `register_domain` with per-domain GC thresholds (`gc_threshold_days`), default retention policies, and path prefixes. Manage with `get_domain`, `list_domains`, `delete_domain`. Tag memories on `save_memory` and filter on `search_memory` by domain for logical isolation across business areas.
- **🧹 Noise Filtering**: Automatic rejection of junk text on save (`is_noise_text`) and meaningless queries on search (`should_skip_query`). Saves embedding API costs and keeps the memory store clean. Bypassable via `force=true`.
- **🩺 Vector Backfill Maintenance**: `tachi backfill-vectors --db <path> [--dry-run]` audits any store for missing embeddings and fills them in batch, which is especially useful after migrations or when agent-local DBs fall behind.
- **⏰ Background Garbage Collection**: Periodic background GC timer (default: every 6 hours, configurable via `MEMORY_GC_INTERVAL_SECS`). Stale-memory archival window controlled by `MEMORY_GC_STALE_DAYS` (default: 90 days). All GC thresholds externalized via `GcConfig` for full configurability. Retention-aware archival respects per-domain overrides and GC-exempt policies.
- **🕸️ Knowledge Graph Tools**: Direct graph manipulation via `add_edge` and `get_edges` MCP tools. Create causal, temporal, and entity relationship edges with metadata and weights.
- **🔗 Auto-Link on Save**: `save_memory` automatically discovers existing memories sharing the same entities and creates graph edges between them (async, non-blocking). Enabled by default, disable with `auto_link=false`.
- **👤 Agent Profiles**: Each agent session can register its identity via `agent_register` — declaring an agent ID, display name, capabilities, tool allowlist (glob patterns), and per-agent rate limit overrides. Query the current profile with `agent_whoami`. Profiles are session-scoped and in-memory.
- **🧾 Provenance on Write**: New writes now carry `metadata.provenance` with the calling tool, resolved DB scope/path, registered agent profile, and optional `TACHI_PROFILE` / `TACHI_DOMAIN` tags so conflicts and stale memories are easier to audit later.
- **🤝 Cross-Agent Handoff**: When an agent session ends, it can leave a structured handoff memo via `handoff_leave` (summary, next steps, target agent, context). The next agent calls `handoff_check` at startup to pick up pending work. Memos are persisted both in-memory and to the global store (`category="handoff"`) for cross-restart durability.
- **⚡ Rate Limiter & Loop Detection**: Per-session sliding window RPM enforcement and identical-call burst detection (same tool + args within 60s). Default: RPM unlimited, burst limit 8. Configurable via `RATE_LIMIT_RPM` and `RATE_LIMIT_BURST` env vars, or per-agent via `agent_register`.
- **📤 Skill Export**: `hub_export_skills` exports Hub skills to agent-specific formats — Claude (SKILL.md + symlinks), OpenClaw (plugin manifest), Cursor (.mdc rules), and generic (raw markdown). Supports visibility filtering, skill ID selection, agent-local scope filtering, and clean mode.
- **🧬 Skill Evolution**: `skill_evolve` uses LLM analysis of the current prompt, usage feedback, and success/failure metrics to generate an improved skill version. Creates versioned capabilities (`skill:name/vN`) with optional auto-activation and dry-run mode.
- **🔮 Virtual Capabilities**: An abstraction layer above Hub capabilities. Register VCs (`vc:*`), bind to multiple concrete MCP backends with priority ordering, resolve at call time with deterministic priority + version pinning. Sandbox policies inherit from VC to concrete backend.
- **🔐 Tachi Vault (Encrypted Secret Storage)**: Local-first encrypted vault for API keys and secrets. Argon2id KDF + AES-256-GCM encryption with per-secret nonces. 9 MCP tools for full lifecycle (`vault_init`, `vault_unlock`, `vault_lock`, `vault_set`, `vault_get`, `vault_list`, `vault_remove`, `vault_status`, `vault_setup_rotation`). Auto-lock after 30min inactivity, brute-force protection (5 failed attempts → 5min lockout), per-secret agent ACLs (`allowed_agents`), and full audit logging. Multi-key rotation with round-robin, random, and LRU strategies.
- **📧 Agent Kanban (Global-Only)**: Cross-agent communication via kanban cards with global-only storage. ACPX protocol extensions (`ack`, `progress`, `result` card types) for structured request/response flows. Workspace and conversation context metadata with filtering support. Kanban card GC for auto-pruning stale resolved/expired cards.
- **👻 Ghost Whispers (Inter-Agent Pub/Sub)**: Persistent topic-based messaging between agents. `ghost_publish`, `ghost_subscribe`, `ghost_topics`, `ghost_ack`, `ghost_reflect`, and `ghost_promote` tools. Messages survive daemon restarts via SQLite persistence. Reflection-to-rule derivation via LLM.
- **🏭 Neural Foundry**: Server-owned context lifecycle — `recall_context`, `capture_session`, `compact_context`, `section_build`, `compact_rollup`, and `compact_session_memory`. Memory capture, compaction, and durable session artifacts live in Tachi instead of host adapters.
- **📦 Skill Packs**: Install, project, and manage curated skill collections. `pack_register`, `pack_list`, `pack_get`, `pack_project`, `pack_remove`. Project skills to multiple agent formats (Claude, Cursor, Codex, Gemini, OpenCode).
- **🧠 Capability Recommendations**: `recommend_capability`, `recommend_skill`, `recommend_toolchain`, and `prepare_capability_bundle` let Tachi suggest optimal tool combinations for any task.
- **🩺 Memory Governance**: `tachi doctor` audits SQLite DB health (extension-aware classification, WAL orphan detection, corruption checks) with safe auto-fix and quarantine; `tachi manifest` maintains `~/.tachi/manifest.json` as the single source of truth for owned DBs, with write guards, resolvers, and paranoid sweep; `tachi rescue` splits mixed antigravity DBs into per-project stores with provenance and source backup.
- **🛡️ Capture Gate**: Pre-write validation on `save_memory` — domain required, path must match allowed buckets, markdown-dump detection, and configurable min-chars floor (default 200, scratch-exempt). Three modes: `warn` (default), `enforce`, `off` via `TACHI_CAPTURE_GATE`.
- **🏭 Foundry Job Lifecycle Hardening**: Atomic terminal-reason metadata updates, GC retention extended to 30 days, and `job_status_histogram` for cross-DB overview.
- **🔌 OpenClaw Manifest Bridge**: OpenClaw now routes through `~/.tachi/manifest.json`, raises default capture floor to 200 chars, and tolerates noisy MCP JSON (BOM, whitespace, bracket recovery).
- **🎭 Tool Surface v2 (Facade & Delegation)**: Provides compact, task-focused unified tools (`tachi_search`, `tachi_web_search`, `tachi_save`, etc.) along with advanced delegation and evaluation capabilities (`tachi_dispatch`, `approve_merge`, `tachi_complete`) for cross-agent orchestration.
- **📚 Wiki System**: Built-in, durable knowledge base system (`tachi_wiki_write`, `tachi_wiki_search`, `wiki_browse`, `wiki_lint`) optimized for multi-agent knowledge sharing.

---

## ⚙️ Causal Worker Pipeline & Memory Relations

Tachi incorporates advanced reasoning components to maintain long-term logical consistency (Note: this pipeline is **disabled by default** to prioritize latency; enable it with `ENABLE_PIPELINE=true`):

### 1. The Causal Extraction Pipeline
When an agent submits execution logs, an asynchronous worker can route extraction, compaction, and evolution prompts through dedicated model lanes. The default extraction lane uses **Qwen3.5-27B** via SiliconFlow to analyze the interaction and extract:
*   `Causes`: The events triggering the action.
*   `Decisions`: The reasoning pathway and logic applied.
*   `Results`: The concrete outcomes.
*   `Impacts`: Long-term consequences within the workspace.

### 2. Derived Isolation
Both causal derivations and distilled rules are physically isolated within a dedicated `derived_items` table, keeping the primary memory layers pure and intact from automated AI-inferred hallucinations.

---

## 🏗️ Architecture

```mermaid
graph TD
    subgraph Clients["Integrations"]
        CLI["Tachi CLI (npm)"]
        RMCP["MCP Server (Rust 5.2MB binary)"]
        OC["OpenClaw Extension (Node.js)"]
        NATIVE["Native Rust Crates"]
    end

    subgraph Cloud["Cloud APIs"]
        VOYAGE["Voyage-4 Embedding"]
        SILICON["SiliconFlow Qwen LLM"]
    end

    subgraph Operations["Async Workers"]
        EXTRACT["Fact Extractor (Qwen)"]
        DISTILL["Context Distiller (Qwen)"]
        CAUSAL["Causal Worker"]
        CONSOLIDATE["Session Consolidator"]
    end

    subgraph Core["Tachi Core (Rust memory-core)"]
        NAPI["NAPI Binding"]
        PYO3["PyO3 Binding"]

        NAPI --- LIB[/"lib.rs (Store API)"/]
        PYO3 --- LIB

        LIB --> SEARCH["5-Channel Hybrid Search"]
        LIB --> GRAPH["Memory Graph (PageRank)"]

        SEARCH --> SQLITE[("Embedded SQLite + vec0")]
        GRAPH --> SQLITE
    end

    RMCP ==>|"Static link, no FFI"| LIB
    RMCP -->|"reqwest"| VOYAGE
    RMCP -->|"async-openai"| SILICON
    CLI -->|"MCP stdio"| RMCP
    OC -->|"MCP stdio preferred"| RMCP
    OC -.->|"NAPI fallback"| NAPI

    classDef client fill:#3b2e5a,stroke:#8a5cf5,stroke-width:2px,color:#fff;
    classDef cloud fill:#2e3d5a,stroke:#5a9cf5,stroke-width:2px,color:#fff;
    classDef worker fill:#5a4f2e,stroke:#f5c55a,stroke-width:2px,color:#fff;
    classDef rust fill:#5a2e2e,stroke:#f55c5c,stroke-width:2px,color:#fff;
    classDef db fill:#2e5a40,stroke:#5cf58a,stroke-width:2px,color:#fff;

    class CLI,RMCP,OC,NATIVE client;
    class VOYAGE,SILICON cloud;
    class EXTRACT,DISTILL,CAUSAL,CONSOLIDATE worker;
    class NAPI,PYO3,LIB,SEARCH,GRAPH rust;
    class SQLITE db;
```

---

## 🧩 Model Stack

Tachi now exposes separate text lanes so you can tune extraction, compaction, summary, and reasoning independently.

Current benchmark-backed guidance:

| Lane | Recommended Model | Why |
|------|-------------------|-----|
| **Embedding / Rerank** | [Voyage-4](https://voyageai.com/) | Best retrieval quality in local A/B tests; remains the default vector backbone. |
| **Extract** | [Qwen3.5-27B](https://cloud.siliconflow.cn/) via SiliconFlow | Most reliable structured fact extraction in the current Tachi/OpenClaw benchmarks. |
| **Distill** | MiniMax M2.7 | Best compaction fidelity and reusable context blocks in round-2 lane tests. Can be wired directly through the OpenAI-compatible MiniMax endpoint. |
| **Summary** | MiniMax M2.7 | Strongest low-token status summaries while preserving useful signal density. Can share the same OpenAI-compatible MiniMax endpoint as `DISTILL`. |
| **Reasoning / Skill Audit** | GLM-5.1 via Z.AI | Best architectural judgment, evolution prioritization, and skill audit final-pass decisions. |
| **Fast Pre-Audit / Scout (Optional)** | Gemini Flash or MiniMax M2.7 | Useful for cheap first-pass scanning before a GLM final decision. |

Implementation note:
- The current Rust client speaks OpenAI-compatible chat completions directly.
- MiniMax's chat-completions endpoint can be used directly for `DISTILL_*` and `SUMMARY_*`.
- The default out-of-the-box release path remains fully usable with Voyage + SiliconFlow, while the lane configuration lets you override individual roles as your providers mature.

---

## 💻 Manual Installation & APIs

For direct integration of `memory-core` into custom applications:

### ⚙️ MCP Tool Examples (via any MCP-compatible client)
```python
# These are MCP tool calls — invoke from any MCP client (Claude, Cursor, etc.)

# 1. Save structured memory (auto-embedding + summary)
save_memory(
    text="The user prefers React frontend with Vite, no Webpack. Tailwind is permitted.",
    path="/user/project_preferences",
    importance=0.8,
    keywords=["react", "vite", "webpack", "tailwind"]
)

# 2. Hybrid search (semantic + lexical + decay)
results = search_memory(
    query="What is the preferred bundler?",
    path_prefix="/user",
    top_k=3
)

# 3. Hard state (deterministic KV, no embeddings)
set_state(
    namespace="trading",
    key="watchlist",
    value={"600089": "TBEA", "688256": "Cambricon"}
)
```

### ⚙️ Environment Configuration (`.env`)
Copy `.env.example` to `.env` in the root directory.
```bash
# Core Embedding and Retrieval
VOYAGE_API_KEY="your_voyage_key_here"

# Shared OpenAI-compatible default lane
SILICONFLOW_API_KEY="your_siliconflow_key_here"
SILICONFLOW_BASE_URL="https://api.siliconflow.cn/v1/chat/completions"
SILICONFLOW_MODEL="Qwen/Qwen3.5-27B"

# Optional per-lane overrides
EXTRACT_API_KEY=""
EXTRACT_BASE_URL=""
EXTRACT_MODEL="Qwen/Qwen3.5-27B"

DISTILL_API_KEY="your_minimax_key_here"
DISTILL_BASE_URL="https://api.minimaxi.com/v1/chat/completions"
DISTILL_MODEL="MiniMax-M2.7"

SUMMARY_API_KEY="your_minimax_key_here"
SUMMARY_BASE_URL="https://api.minimaxi.com/v1/chat/completions"
SUMMARY_MODEL="MiniMax-M2.7"

REASONING_API_KEY="your_glm_key_here"
REASONING_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
REASONING_MODEL="glm-5.1"

# Database path (Optional — auto-resolves to ~/.Tachi/global/memory.db + .Tachi/memory.db per project)
MEMORY_DB_PATH="~/.Tachi/global/memory.db"
```

Operational note:
- The Rust release currently expects OpenAI-compatible chat-completions endpoints for lane overrides.
- The tested release path is:
  - `EXTRACT = Qwen3.5-27B`
  - `DISTILL = MiniMax M2.7`
  - `SUMMARY = MiniMax M2.7`
  - `REASONING = GLM-5.1`

---

## 🛡️ Database Safety

> **Important**: Tachi uses SQLite in WAL mode for maximum single-writer performance. Violating the rules below may corrupt the database.

| Rule | Why |
|------|-----|
| **Single instance only** | The server acquires an exclusive file lock (`memory.db.lock`) at startup. If you see "Another memory-server instance is already running", stop the duplicate process. |
| **No cloud-synced paths** | iCloud, Dropbox, OneDrive, and Google Drive are **incompatible** with SQLite WAL. Use a local-only directory (e.g., `~/.Tachi/`). |
| **No concurrent CLI writes** | Do not run `sqlite3` INSERT/UPDATE on the database while the server is running. Read-only queries are safe with `PRAGMA busy_timeout = 5000`. |
| **Auto-recovery on startup** | The server runs `PRAGMA quick_check` on startup and auto-backfills an empty FTS index from the main `memories` table. |
| **Graceful shutdown** | The server handles SIGINT/SIGTERM to flush WAL and run `PRAGMA optimize` before exit. Avoid `kill -9`. |

---

## 🏎️ Benchmarks

- **P95 Latency (Rust Core)**: `< 1.2ms` for localized hybrid lookups.
- **Extraction Parallelism**: Background thread pools manage extraction with strict isolation from the main event loop.
- **Token Efficiency**: The hierarchical `L0` → `L1` → `L2` context tiering reduces prompt context bloat by up to **85%** compared to standard RAG chunking, significantly enhancing instruction adherence.

---

## 🤝 Contributing

Contributions to Tachi are welcome. To establish a local development environment:
1. Ensure Rust (`rustc>=1.75`) is installed.
2. Install build utilities: `cargo install maturin cargo-watch`.
3. The core storage API is located at `crates/memory-core/src/lib.rs`.
4. Validate changes utilizing `cargo test --all` prior to submitting a pull request.

Commit messages must conform to the [Conventional Commits](https://www.conventionalcommits.org/) specification.

---

## 📜 License

[AGPLv3 License](LICENSE) © 2026 Tachi Authors.
