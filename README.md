<div align="center">
  <img src="assets/banner_en.png" alt="Tachi Banner" width="800" style="margin-bottom: 20px;" />
  <h1>✧ Tachi</h1>
  <p><strong>Local-First Memory and Workflow Control Plane for Autonomous AI Agents</strong></p>
  <p>
    <a href="README.md"><b>English</b></a> ·
    <a href="README.zh-CN.md">简体中文</a> ·
    <a href="README.classical.md">文言文</a>
  </p>
  <p>
    <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/License-AGPLv3-blue.svg" alt="License: AGPLv3"></a>
    <img src="https://img.shields.io/badge/Rust-Edition_2021-orange.svg" alt="Rust">
    <img src="https://img.shields.io/badge/Protocol-MCP-purple" alt="MCP">
    <img src="https://img.shields.io/badge/Backend-SQLite_+_sqlite--vec-green.svg" alt="SQLite">
    <img src="https://img.shields.io/github/v/release/kckylechen1/tachi.svg" alt="Release">
  </p>
</div>

---

## TL;DR

Tachi is a single-binary, local-first memory and coordination backend for AI agents. It runs as an [MCP](https://modelcontextprotocol.io/) server (`tachi-server`) and gives agents:

- **Persistent memory** with hybrid semantic + lexical + graph retrieval
- **Hierarchical namespaces** (`/user/preferences`, `/project/architecture`)
- **Causal graph edges** between memories, entities, and decisions
- **Domain-tagged storage** — free-text `domain` field on every memory, filterable via `save_memory`/`search_memory`
- **Encrypted local vault** for API keys and secrets
- **Agent coordination** via handoff, kanban, and pub/sub (Ghost Whispers)
- **Skill packs and capability hub** — register once, use from any agent
- **Workflow control plane** — `tachi_task`, `tachi_staff`, `tachi_verify`, `tachi_gh`
- **Continuity memory** — typed events, pattern projections, outcome labels, and project-cycle context
- **Lifecycle closure** — GitHub issues/PRs, docs/specs, wiki, memory, verification, and release notes in one loop

All state lives in embedded SQLite. **Zero external database dependencies.**

Named after the Tachikoma from *Ghost in the Shell*: agents that evolve through shared memory.

### Current Release

Current release: `v1.9.0`.

This line makes Tachi's project-cycle direction explicit:

- Continuity memory is now typed: session captures, memory writes, outcome
  labels, projections, and active patterns can be stored and queried as durable
  project context.
- Pattern memory now has an explicit feedback loop: `tachi_memory`
  `action="search" scope="patterns"` records `seen`, while `tachi_memory`
  `action="pattern_feedback"` records reviewed `hit`, `miss`, and `stale`
  signals without promoting anything into skills automatically.
- `tachi_task` can guide a full issue/PR/doc lifecycle: intake, doc index,
  cycle plan, verification status, PR handoff, release notes, reference
  building, and close-loop writes back to memory/wiki/docs.
- Recall behavior is tunable and reviewable through simulation, rerank replay,
  and scored proposals instead of one-off hard-coded ranking changes.
- Superpowers and Waza are tracked as governed skill sources with pinned
  metadata and reviewed sync planning.
- Runtime routing is stricter: repo-local project DBs are primary, global
  `--no-project-db` serves are isolated from project launch context, and
  `TACHI_DISABLE_STDIO_PROXY=1` lets source-tree MCP debug sessions avoid
  reusing an existing daemon.
- Health scoring now treats a missing daemon as a runtime mode, not a failure
  by itself; concrete problems such as stale distill, provider failures,
  vector gaps, or failed Foundry jobs still lower the score.
- The shell installer now installs a user LaunchAgent for the global Tachi
  daemon on macOS, with idle shutdown disabled and launchd `KeepAlive` omitted
  so background work keeps running without duplicate singleton-lock restarts.
- OpenClaw remains a thin MCP facade. It owns hook timing and OpenClaw-facing
  tool exposure; Tachi owns database writes, embedding, rerank, distill, graph
  maintenance, Foundry jobs, and continuity projection.
- Cargo, npm, lockfiles, docs, installer URLs, and OpenClaw plugin metadata are
  checked by `scripts/check_release_versions.py` and CI before release.

See [CHANGELOG.md](CHANGELOG.md) for the full release notes.

---

## Why Tachi

Agent memory today usually looks like this: every session starts cold, important context is stuffed into a flat vector store, and after a few weeks the prompt is bloated with irrelevant chunks while the *why* behind key decisions has vanished.

Tachi is built around four convictions:

1. **Memory should be structured, not dumped.** Hierarchical `path` namespaces, causal graph edges, and domain scoping keep long-term context organized and traversable.
2. **Retrieval should be hybrid and fast.** Semantic (sqlite-vec + Voyage), lexical (FTS5 + CJK), temporal decay (ACT-R), and graph spreading activation are fused via RRF. We optimize for local, low-latency lookups; reproducible benchmarks are on the roadmap.
3. **Agents should share infrastructure, not spawn chaos.** Tachi Hub registers MCP servers and skills once; connected agents share connection pooling, idle cleanup, circuit breakers, and sanitized environments. No zombie processes.
4. **Long-term state belongs locally.** All databases are SQLite files. No cloud DB required. Cloud sync should move encrypted bundles and event logs, not live WAL files.

## How Tachi Compares

Tachi is not a general-purpose vector database or a managed memory cloud. It is a local-first coordination backend for agents that speak MCP.

| Dimension | Tachi | Mem0 | Letta | Chroma | Raw vector DB |
|---|---|---|---|---|---|
| **Integration protocol** | MCP server (STDIO / Streamable HTTP) | Language SDKs | Python SDK + ADE | HTTP API + SDKs | None |
| **Deployment** | Single Rust binary | Library + optional service | Service + frontend | Service + optional Cloud | Depends |
| **Storage backend** | Local SQLite + sqlite-vec | Usually PG / Redis / vector DB | PG + Qdrant / Chroma | Chroma index | Various |
| **External dependencies** | Zero (embedding provider optional) | Medium | Medium | Low to medium | High |
| **Default data locality** | Local-first | Cloud-first, self-hostable | Self-hosted | Self-hosted / Cloud | Depends |
| **Memory organization** | `path` hierarchy + causal graph + domain | Entity + session | Agent state + memory blocks | Collection + metadata | None |
| **Workflow control** | `task` / `staff` / `verify` | None | Agent orchestration | None | None |
| **Target user** | Individuals / small teams running autonomous agents | App developers adding memory | Builders of stateful agents | Systems needing vector search | Infrastructure engineers |

---

## Quick Start

### 1. Install

```bash
brew tap kckylechen1/tachi && brew install tachi
```

Or use the shell installer (also installs the OpenClaw plugin when detected):

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.9.2/scripts/install.sh)"
```

On macOS the shell installer also installs/restarts a user LaunchAgent at
`~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist`. Skip that with
`--skip-daemon-service` if you only want the CLI/MCP stdio binary.

Verify:

```bash
tachi --version
tachi daemon status
```

OpenClaw users should use the full installer above to refresh both the Tachi
binary and the `tachi` OpenClaw plugin. The plugin is a thin MCP facade: it
starts or connects to the Tachi runtime over stdio, exposes OpenClaw-facing
memory tools, and does not maintain its own shadow store or SQLite index. Older
local installs may still have stale plugin metadata until reinstalled.

### 2. Configure your agent

Add Tachi to your agent's MCP config. The profile controls how many tools the agent sees:

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-key>",
        "SILICONFLOW_API_KEY": "<your-key>",
        "TACHI_PROFILE": "standard"
      }
    }
  }
}
```

- `VOYAGE_API_KEY` — required for hybrid semantic search (the server runs without it, serving lexical/graph recall only).
- `SILICONFLOW_API_KEY` — recommended for extraction, summaries, and foundry distillation.
- `TACHI_PROFILE` — see [Tool Surface Profiles](#tool-surface-profiles) below.

The server also loads `.env` from the project root automatically. Copy `.env.example` to `.env` for per-project configuration.

### 3. Use

These examples show the JSON arguments you would pass to the MCP tools. Facade tools expose the same fields as their underlying native tools; the full schemas live in `crates/tachi-params/src/facade.rs` and its `facade/` submodules.

```json
// tachi_save — structured memory
{
  "tool": "tachi_save",
  "arguments": {
    "text": "Frontend must use Vite, never Webpack. Tailwind is allowed.",
    "path": "/project/frontend",
    "importance": 0.8,
    "keywords": ["vite", "webpack", "tailwind"],
    "retention_policy": "durable"
  }
}

// tachi_memory — hybrid recall
{
  "tool": "tachi_memory",
  "arguments": {
    "action": "search",
    "query": "What is the frontend build policy?",
    "path_prefix": "/project",
    "top_k": 6,
    "scope": "memory"
  }
}

// Ask Tachi for context-aware planning, then delegate ordinary bounded work
// with your host's native subagent:
{
  "tool": "tachi_task",
  "arguments": {
    "action": "plan",
    "task": "Review the API boundary and identify compatibility risks."
  }
}
```

Host-specific config paths and advanced setup are in [`docs/INSTALL.md`](docs/INSTALL.md).

---

## Architecture

```mermaid
graph TD
    subgraph Clients["Clients"]
        CLI["tachi CLI"]
        RMCP["MCP Server (Rust binary)"]
        Node["@chaoxlabs/tachi-node"]
        Hosts["Host MCP clients"]
    end

    subgraph Cloud["Optional APIs"]
        VOYAGE["Voyage-4 Embedding"]
        SILICON["SiliconFlow / Qwen"]
    end

    subgraph Workers["Async Workers"]
        EXTRACT["Fact Extraction"]
        DISTILL["Context Distillation"]
        CAUSAL["Causal Pipeline"]
        GC["Garbage Collection"]
    end

    subgraph Core["Core (Rust memcore)"]
        API["Store API"]
        SEARCH["5-Channel Hybrid Search"]
        GRAPH["Memory Graph"]
        VAULT["Vault Metadata"]
        API --> SEARCH
        API --> GRAPH
        API --> VAULT
        SEARCH --> DB
        GRAPH --> DB
        VAULT --> DB
    end

    DB[(SQLite + sqlite-vec)]

    RMCP --> VOYAGE
    RMCP --> SILICON
    CLI --> RMCP
    Hosts --> RMCP
    Node --> Core
    Workers --> RMCP
```

---

## Project Structure

| Path | What it is |
|------|------------|
| `crates/memcore` | Rust core: SQLite storage, migrations, hybrid search, graph, domains, vault metadata, sqlite-vec. |
| `crates/tachi-bootstrap` | Shared CLI command contract and startup parsing for the Tachi server binary. |
| `crates/tachi-hub` | Shared Hub capability policy, skill execution envelope, and security scan rules used by the server/runtime. |
| `crates/tachi-server` | MCP server/runtime implementation, profile filtering, Hub handlers/routing, dispatch/workflow tools, wiki, vault encryption, daemon locking, Foundry background workers. |
| `crates/memory-node` | Node.js bindings (`@chaoxlabs/tachi-node`) for native integration. |
| `packages/tachi-cli` | TypeScript CLI and npm wrapper. |
| `tools/cleaner` | `tachi-clean` utility for safe target/worktree/temp cleanup. |
| `skill/` | Built-in skill packs: `amp`, `codex`, `superpowers`, `waza`. |
| `integrations/openclaw` | OpenClaw plugin. |
| `docs/` | Agent ecosystem specs, install guide, and engineering docs. |
| `bin/` | Locally built release binaries. |

---

## Core Capabilities

### 1. Hierarchical Memory
Memories are stored under `path` namespaces (e.g. `/user/preferences`, `/project/architecture`, `/handoff/active`) instead of a flat index. This keeps project, user, and coordination contexts isolated and composable.

### 2. 5-Channel Hybrid Search
- **Semantic** — `sqlite-vec` KNN with Voyage-4 embeddings.
- **Lexical** — CJK-optimized FTS5 via `libsimple`, with query expansion (synonyms, acronyms, phrase variants) for sparse corpora.
- **Temporal decay** — ACT-R inspired forgetting curve.
- **Graph spreading activation** — activation propagates along causal/entity edges from seed weights; noisy-OR within each hop prevents dense clusters from dominating.
- **RRF fusion** — reciprocal rank fusion blends all channels, with vector cosine weighted in to reduce rank inversions on highly semantic queries.

### 3. Causal Graph
The graph engine creates and traverses causal, temporal, and entity relationships. `save_memory` can automatically link entries sharing entities (`auto_link`). `add_edge` / `get_edges` / `memory_graph` are internal `MemoryStore` primitives — not on the MCP surface (#757); agents reach graph behavior through `tachi_save`/`tachi_memory` auto-linking and recall's graph-spreading-activation channel (§2 above) — there is no standalone graph-traversal action.

### 4. Domain-Tagged Storage
Every memory carries a free-text `domain` field (e.g. `"code-review"`, `"personal"`). `save_memory` and `search_memory` can filter by domain. There is no separate domain registry — domains are ad-hoc tags on memory rows, not a configured resource.

### 5. Encrypted Vault
Local-first secret storage: Argon2id KDF + AES-256-GCM, per-secret nonces, auto-lock after inactivity, brute-force protection, per-secret agent ACLs, and multi-key rotation. Project-local agents can resolve Vault secrets via `.tachi/vault.env` aliases. `tachi vault exec --require NAME -- <cmd>` runs a child process with Vault-delivered credentials (Vault only fills env names the caller did not already set); by default it refuses to spawn a credential-less child if the Vault is unavailable, and `--allow-unauthenticated` opts back into running with the inherited environment. See [`docs/INSTALL.md`](docs/INSTALL.md).

### 6. Tachi Hub & Skill Packs
Register MCP servers, skills, and toolchains once; any connected agent can discover and call them. `pack_register` / `pack_project` install curated skill collections and project them to Claude, Cursor, Codex, Gemini, and OpenCode formats. `tachi_skill(action="discover"|"run"|"bundle")` is the canonical skill facade; standalone `run_skill`, `prepare_capability_bundle`, and skill-focused `hub_discover` calls remain compatibility routes for older clients.

Read-only diagnostics help keep those surfaces aligned:

```bash
tachi harness status --host codex,claude,gemini,antigravity,cursor
tachi skill-surface status --host claude,codex,gemini,cursor,antigravity
```

`harness status` checks host instruction files for managed Tachi guidance, legacy blocks, duplicate workflows, and stale host-specific hardcodes. `skill-surface status` compares local skill stores and host projections, including CC Switch projection metadata when available.

### 7. Agent Coordination
- **Ghost Whispers** — persistent topic-based pub/sub between agents (`ghost_publish`, `ghost_subscribe`, `ghost_ack`, `ghost_reflect`, `ghost_promote`).
- **Kanban** — cross-agent cards with `ack` / `progress` / `result` states (`post_card`, `check_inbox`, `update_card`).
- **Handoff issue promotion** — create/link a GitHub issue from an existing handoff memo (`tachi_handoff(action='promote_issue')`). #1099: the older `handoff_leave`/`handoff_check` memo-passing routes are retired — use `tachi_memory(action='sticky_leave'|'sticky_check')` for a short agent-to-agent note, or `tachi_orchestrator(action='handoff_write'|'handoff_read')` for a structured task baton.

> Ghost and Kanban tools are native `admin`-profile surfaces (not bundled into `standard`/`coordinate`). Most agents coordinate through the `tachi_handoff`, `tachi_workflow`, `tachi_orchestrator`, and `tachi_task` facades instead.

### 8. Continuity & Project Lifecycle Memory
Continuity is the layer above raw recall. Tachi records typed project events,
projects repeated behavior into reusable pattern memories, and exposes
read-only context so future agents can continue the same project cycle without
reconstructing it from chat history.

Key surfaces:

- **Continuity events** — append/query/project neutral project events from
  session captures, memory saves, task outcomes, and repo-specific adapters.
- **Projection memories** — convert repeated or high-signal continuity events
  into durable pattern/timeline entries under project memory.
- **Outcome labels** — record whether prior agent actions helped, failed,
  drifted, or required user correction.
- **Project-cycle context** — brief future agents with active patterns,
  lifecycle state, verification evidence, and unresolved closure debt.
- **Lifecycle references** — link issues, PRs, docs/specs, wiki pages, memory
  fragments, and run artifacts so the project history stays navigable.

Relevant design docs:

- [`docs/engineering/architecture/tachi-continuity-memory-architecture.md`](docs/engineering/architecture/tachi-continuity-memory-architecture.md)
- [`docs/engineering/architecture/project-cycle-memory-spine.md`](docs/engineering/architecture/project-cycle-memory-spine.md)
- [`docs/engineering/architecture/pattern-timeline-bonding-memory.md`](docs/engineering/architecture/pattern-timeline-bonding-memory.md)
- [`docs/engineering/examples/session-to-pattern-extraction-example.md`](docs/engineering/examples/session-to-pattern-extraction-example.md)

### 9. Workflow Control Plane
Tachi is not only a memory store; it is becoming the durable control plane for agent engineering:

- **`tachi_task`** — guide work from issue intake through docs/specs,
  dispatch recommendations, PR handoff, release notes, and close-loop writes.
- **`tachi_staff`** — external staffing exception (`action='start'`/`'status'`) for durable/remote worker launch when no native subagent applies; requires a typed `staffing_reason`.
- **`tachi_verify`** — record background verification evidence (tests, type checks, safe-merge gates) under `.tachi/runs/<flow_id>/verification.json`.
- **`tachi_gh`** — read issues/PRs, post comments, digest review state, and
  run safe-merge checks with lifecycle/verification evidence.

### 10. Neural Foundry & Wiki
- **Foundry** — server-owned context lifecycle: `recall_context`, `capture_session`, `compact_context`, `section_build`, `compact_rollup`, `compact_session_memory`, plus agent evolution proposals.
- **Wiki** — durable knowledge pages maintained by agents: `tachi_wiki`, `tachi_browse`, `tachi_wiki_write`, `tachi_wiki_search`, `wiki_lint`.

### 11. Portable Memory Kernel
`portable-kernel` is the current Cargo facade over `memcore` with Tachi's admin features disabled. `portable-server` exposes that kernel over MCP stdio or loopback HTTP without linking Tachi's operator surfaces. Owner ruling #1195 retains these packages as candidate boundaries for a future ZeroClaw-native memory module, but no direct ZeroClaw Cargo integration has landed. Historically, the split was introduced for HyperTachi and HyperMemory convergence; that fork is no longer the target consumer. A **portable kernel manifest** (`docs/engineering/architecture/kernel-surface-v1.fixture.json`) freezes the durable schema, recall primitives, vector/FTS fallback behavior, and readiness diagnostics as a product-agnostic contract. A manifest global-DB write-guard runs at startup; `TACHI_BYPASS_MANIFEST=1` skips that guard for development or crash recovery.

---

## Tool Surface Profiles

Tachi exposes a filtered MCP surface based on `TACHI_PROFILE`. The full `admin` catalog is large; most agents should use a smaller profile.

| Profile | What is exposed | Best for |
|---------|-----------------|----------|
| `standard` | Daily agent-intent surface: `tachi_save`, `tachi_memory`, the non-dispatch actions of `tachi_task`, `tachi_verify`, `tachi_web_search`, `tachi_wiki`, `tachi_skill`, `tachi_gh`, `peer_query`, vault session/status tools, plus `runtime_info`, `tachi_status`, `tachi_briefing`, and `tachi_tools`. `tachi_staff` and manual native-eval intake are adapter/internal surfaces, not ordinary agent tools. | IDE agents: Claude, Cursor, Codex, Windsurf, Trae, Antigravity. Ordinary delegation uses the host's native subagent. |
| `coordinate` | `remember` + `coordinate` bundles: adds `tachi_handoff`, `tachi_workflow`, `tachi_orchestrator`, `tachi_agents`, `tachi_gh`, `tachi_staff`, and `tachi_verify`. `tachi_staff(action='start', task='review the API surface and write findings to result.md', staffing_reason='native_subagent_unavailable')` remains an explicit durable/remote exception, not the default worker launcher. | Advanced coordination and adapter workflows; not a replacement for host-native subagents. |
| `operate` | `remember` + `operate` bundles: adds Foundry lifecycle, `hub_call`, `vault_unlock`/`lock`/`status`, `wiki_lint`. | Runtime adapters, OpenClaw, ops automation. |
| `delegate` | Curated worker surface: `tachi_tools`, `runtime_info`, `tachi_memory`, `tachi_event`, `tachi_web_search`, `tachi_browse`, `tachi_unstick`, `tachi_task`, `tachi_complete`, `tachi_skill(action='discover'|'run'|'bundle')`, and read-only `peer_query`. Standalone `run_skill` remains a legacy compatibility route outside the default delegate profile. | Worker subagents spawned by an explicitly admitted admin dispatch. No recursive dispatch, no handoff, no skill candidate registration. |
| `admin` | Full catalog, including explicitly justified durable/remote Tachi dispatch. | Maintenance, development, governance, and operator-approved execution exceptions. |

Host aliases are resolved automatically: `claude`, `claude-code`, `codex`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity` → `standard`; `worker`, `subagent`, `delegate` → `delegate`; `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`.

If no profile is set, Tachi defaults to `standard` (since v1.0.1).

For MCP host debugging, set `TACHI_DISABLE_STDIO_PROXY=1` to force the stdio
process to serve locally instead of forwarding to an already-running daemon.
`TACHI_DISABLE_AUTO_DAEMON=1` only disables daemon spawn/replacement; it may
still reuse a compatible daemon unless stdio proxying is also disabled.

---

## Model Stack

Phase 2 simplified the background lanes. Extraction, summary, and daily distillation go through configured OpenAI-compatible API lanes; a distinct configured provider fallback is tried before loud `LANE_OUTAGE` recording when a chain is exhausted. `FOUNDRY_DISTILL_BACKEND=claude_cli` remains a legacy selector but still calls the API distill lane, not a Claude subprocess. Ordinary reasoning/chat is separate: it tries Claude CLI first, then the configured API fallback; provider-only callers intentionally omit CLI. For most deployments you only need:

| Purpose | Required | Default |
|---------|----------|---------|
| Embeddings | **Yes** | Voyage-4 via `VOYAGE_API_KEY` |
| Extraction / summary / distillation | Recommended | SiliconFlow `Qwen/Qwen3.5-27B` via `SILICONFLOW_API_KEY` |

Optional per-lane overrides (`EXTRACT_*`, `DISTILL_*`, `SUMMARY_*`, `REASONING_*`) remain supported for advanced setups. See `.env.example` for details.

---

## Environment Configuration

Copy `.env.example` to `.env` in your project root:

```bash
# Required for hybrid semantic search (the server boots and serves lexical/graph
# recall without it, but vector embeddings need a Voyage key).
VOYAGE_API_KEY=your_voyage_key_here

# Recommended: powers extraction, summaries, and Foundry distillation.
SILICONFLOW_API_KEY=your_siliconflow_key_here
SILICONFLOW_BASE_URL=https://api.siliconflow.cn/v1/chat/completions
SILICONFLOW_MODEL=Qwen/Qwen3.5-27B

# Optional: override the global DB path. Defaults to ~/.tachi/global/tachi-memory.db;
# project DBs are auto-detected at <git-root>/.tachi/tachi-memory.db.
MEMORY_DB_PATH=~/.tachi/global/tachi-memory.db
```

The server loads `.env` from the project root automatically.

### Other useful environment variables

| Variable | Purpose |
|----------|---------|
| `TACHI_PROFILE` | Selects the MCP tool surface (`standard`, `coordinate`, `operate`, `delegate`, `admin`, or a host alias). Defaults to `standard`. |
| `TACHI_HOME` | Overrides the Tachi home directory (default `~/.tachi`). The global DB path flows from this. |
| `GH_TOKEN` | GitHub token for `tachi_gh`, `safe_merge`, and ship operations. |
| `TACHI_DISABLE_STDIO_PROXY` | `1` forces a stdio process to serve locally instead of forwarding to a running daemon (for source-tree MCP debugging). |
| `TACHI_DISABLE_AUTO_DAEMON` | `1` disables daemon spawn/replacement (may still reuse a compatible daemon). |
| `TACHI_BYPASS_MANIFEST` | `1` skips the manifest global-DB write-guard (WalOrphan check) at startup, for development or crash-recovery. |

---

## Database Safety

Tachi uses SQLite in WAL mode. Violating these rules can corrupt the database:

| Rule | Why |
|------|-----|
| **Single instance per DB** | The server holds an exclusive file lock (`tachi-memory.db.lock`). Only one Tachi process should write to a given database file. |
| **No cloud-synced paths** | iCloud, Dropbox, OneDrive, and Google Drive are incompatible with SQLite WAL. Keep databases in `~/.tachi/` or local project paths. |
| **No concurrent raw writes** | Do not run `sqlite3` INSERT/UPDATE on the DB while the server is running. Read-only queries are safe. |
| **Graceful shutdown** | The server handles SIGINT/SIGTERM and runs `PRAGMA optimize` on exit. Avoid `kill -9`. |

Live SQLite files should stay local. Sync encrypted bundles, append-only event logs, vault ciphertext, workflow summaries, and wiki/skill artifacts instead.

---

## Local Development

```bash
# Build release binary
cargo build --release

# Run all tests (nextest is what CI runs: per-test timeouts, see .config/nextest.toml)
cargo nextest run --workspace   # cargo install cargo-nextest; plain `cargo test --all` also works
# Two tests need the gitignored docs/superpowers/ corpus (see #1378). Default runs
# skip them via #[ignore]; on a provisioned box: cargo nextest run --run-ignored all …

# Run the MCP server from source with the standard profile
cargo run -p tachi-server -- --profile standard
```

Requires Rust ≥ 1.75. `@napi-rs/cli` and `cargo-watch` are useful for Node binding work and iterative development.

---

## Benchmarks

Reproducible benchmarks are being formalized. Current design targets include:

- **Local-first latency**: optimize for sub-10 ms lookups on warm SQLite.
- **Hybrid retrieval**: combine semantic, lexical, temporal, and graph signals.
- **Tiered context**: reduce prompt bloat via `L0 → L1 → L2` compaction.
- **Zero external DB dependencies**: single Rust binary, one SQLite file per store.

---

## Acknowledgements

Tachi's design is informed by prior work in agent long-term memory:

- **[LongMem](https://github.com/Victorwz/LongMem)** (NeurIPS 2023) — decoupled memory architecture; influenced dual-DB isolation and cached long-context design.
- **[gbrain](https://github.com/garrytan/gbrain)** — brain-vs-memory layering; influenced namespace design, global/project isolation, and background GC pipelines.
- **[ENGRAM](https://arxiv.org/abs/2511.12960)** — typed memory categories + dense retrieval; validated the hybrid search direction.
- **[Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** — LLM-maintained structured wiki pages; directly inspired the Tachi Wiki system.

---

## Contributing

**Tachi is currently not accepting external contributions.**

The project is maintained by a single author. Copyright is held by one owner,
and outside pull requests are not accepted — they will be closed automatically
by `.github/workflows/close-external-prs.yml`. See [CONTRIBUTING.md](CONTRIBUTING.md)
for details.

You are welcome to use, fork, and study Tachi under the terms of the license
below, in compliance with AGPL-3.0-only.

---

## License

Tachi is licensed under the [GNU Affero General Public License v3.0 only](LICENSE).

Copyright © 2026 Kyle Chen. All rights reserved where not granted under the
AGPL-3.0-only license.

Tachi does not accept external contributions; the single-maintainer model keeps
the project's copyright and licensing direction unambiguous.
