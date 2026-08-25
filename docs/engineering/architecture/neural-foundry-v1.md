---
title: "Neural Foundry V1 Architecture"
summary: "Spec for Tachi's shift to a shared runtime for memory, skills, and agent evolution."
category: "engineering/architecture"
organize: true
---
# Neural Foundry V1

## Why

Tachi is moving from a memory-centric MCP server toward a shared runtime for:

- memory capture and retrieval
- skill optimization
- agent/profile evolution
- cross-host projection

The core rule is simple:

- business logic lives in Tachi
- MCP / CLI / Agent SDK expose interfaces
- host adapters stay thin

The public architecture now also assumes a formal kernel surface. See [Kernel Surface V1](./kernel-surface-v1.md).

OpenClaw is the first migration target, not the final architecture.

## Boundaries

### Tachi Core

Owns the canonical source of truth for:

- evidence
- memories
- skills
- agent profiles
- evolution proposals
- projection state
- capability knowledge

Owns the heavy model work:

- embedding
- extraction
- reranking
- distillation
- forgetting / archival proposals
- skill evolution
- agent evolution

### Interfaces

Tachi may be called from:

- MCP
- CLI
- Agent SDK adapters
- future HTTP / daemon surfaces

These are transport layers, not the business layer.

### Kernel Surface

Neural Foundry now assumes five outward-facing layers:

1. `kernel`
2. `capability`
3. `runtime`
4. `workflow`
5. `admin`

The key boundary is:

`Tachi does not need to own all execution tools; it needs to understand, recommend, and orchestrate them.`

This is the piece that turns Tachi from a memory server into a capability-aware runtime.

### Adapters

Hosts such as OpenClaw, Claude Code, and Codex should only:

- capture lifecycle events
- submit evidence
- request recall / proposals
- project canonical profile data into host-native files or config

## Runtime Shape

### Online path

Latency-sensitive calls used during active agent turns:

- `recall_context`
- `memory_search`
- `capture_session`

### Offline path

Worker-driven jobs that should not block tool use:

- `memory_neighborhood` (split out of the legacy `memory_rerank` job; on the Maintenance lane, writes `metadata.related_entries` back onto the source memory)
- `recall_rerank_cache` (split out of the legacy `memory_rerank` job; on the Rerank lane, writes ephemeral `recall-cache/*` entries with `source=foundry_recall_rerank_cache` and `retention_policy=ephemeral` only when durable recall-cache writes are explicitly enabled via `TACHI_ENABLE_DURABLE_RECALL_CACHE=1`)
- `memory_distill`
- `forget_or_archive`
- `skill_evolution`
- `agent_evolution`
- `profile_projection`

## Current Status

### Implemented in `feat/neural-foundry`

The branch has already landed the first Foundry foundations and the first online migration slice.

Completed:

- canonical Foundry schema in `memcore`
- manual `synthesize_agent_evolution` tool
- first-pass `Kernel Surface V1` docs
- first-pass capability recommendation APIs (all retired by #1690): `recommend_capability`, `recommend_skill`, `recommend_toolchain`
- `prepare_capability_bundle` (retired by #1690; capability-bundle assembly is deleted)
- `memory_graph` (retired name) as the then-read-only agent-facing graph lookup (later retired/internalized off MCP
  off the MCP surface in #757 — this "Completed" entry documents the
  migration-branch state at the time, not current tool availability)
- `recall_context` online API in Tachi
- `capture_session` online API in Tachi
- `compact_context` online API in Tachi
- `section.build`, `compact.rollup`, and `compact.session_memory`
- Tachi-side rerank for recall results
- Tachi-side extraction + embedding for captured session memories
- first Foundry maintenance worker for `memory_neighborhood`, `recall_rerank_cache`, `memory_distill`, and `forget_sweep` (the legacy `memory_rerank` kind was split into `memory_neighborhood` + `recall_rerank_cache`; deserialization preserves backward compatibility via `#[serde(alias = "memory_rerank")]`)
- OpenClaw integration updated to prefer the new Tachi APIs and exposed `memory_search / memory_save / memory_get` / `memory_graph` (all since retired/internalized; as of this migration; `memory_graph` was later dropped from the OpenClaw surface too in #757 — see `tool-profile-plan.md`)

This means OpenClaw now prefers:

- `recall_context` for `before_agent_start`
- `capture_session` for `agent_end`
- queued maintenance jobs for post-capture memory upkeep

### Still using compatibility fallback

The migration is intentionally partial right now.

OpenClaw still keeps local fallback paths for:

- local FTS search

Tachi still keeps some inline maintenance in the online path for safety:

- capture extraction and first-write persistence before worker handoff

That remaining compatibility layer is temporary and exists only to keep the plugin usable while the Tachi-side worker pipeline stabilizes.

## Canonical Data

### Evidence

Evidence is broader than memory. It includes:

- memories
- reflections
- tooluse traces
- eval logs
- ghost messages
- session outcomes
- skill telemetry
- profile snapshots
- capability outcomes

### Agent profile

`AGENTS.md` and `IDENTITY.md` are projection targets, not the source of truth.

The canonical profile should capture:

- identity / mutable traits
- routing rules
- tool policy
- memory policy
- model policy
- evolution proposals

### Capability model

Tachi should eventually maintain a canonical capability catalog for:

- tools
- skills
- packs / superpacks
- host extensions
- preferred toolchains

This catalog is not the same thing as the host’s execution tools. It is the layer Tachi uses to recommend and orchestrate the best path for a task.

### Projection

Projection writes host-native artifacts such as:

- `AGENTS.md`
- `IDENTITY.md`
- tool / routing config
- cron specs

Projection must be section-aware. It should update managed sections instead of overwriting whole files.

## Migration Plan

### Phase 1

Stand up the Neural Foundry foundations in Tachi:

- canonical types
- synthesis prompts
- manual evolution synthesis entrypoint

Status:

- completed

### Phase 2

Move OpenClaw memory logic into Tachi:

- extractor
- embedding
- reranker
- dedup / merge

OpenClaw becomes a thin lifecycle adapter.

Status:

- in progress

Already done:

- built-in host tool profiles now separate `ide`, `runtime`, `workflow`, and `admin`
- the retired first-pass capability APIs `recommend_capability`, `recommend_skill`, and `recommend_toolchain` were deleted end-to-end by #1690 C3
- online recall moved behind `recall_context`
- session capture moved behind `capture_session`
- compaction now has a typed `compact_context` runtime API, ready for host hook wiring
- OpenClaw now prefers Tachi-side online APIs
- OpenClaw `agent_end` delegates to `capture_session` instead of doing local model calls
- failed capture windows are spooled locally and replayed on the next healthy Tachi connection
- capture writes now hand off dedup / merge to Foundry maintenance workers
- `recall_context` can auto-scope by `agent_id`, and OpenClaw now passes it through
- OpenClaw `before_agent_start` uses `recall_context` first, with local FTS fallback only for resilience
- user-initiated OpenClaw memory search now also degrades only to local FTS fallback instead of local embedding + rerank
- `capture_session` now queues Foundry maintenance jobs for `memory_neighborhood` and `forget_sweep`; `memory_distill` runs through the daily batch scheduler, and durable `recall_rerank_cache` jobs are opt-in via `TACHI_ENABLE_DURABLE_RECALL_CACHE=1` (the legacy `memory_rerank` kind was split — see the §Offline path entry above)
- capture entries that wait for background enrichment now re-enter Foundry maintenance after vectors land
- Foundry maintenance now tracks worker counters through `get_pipeline_status`
- `recall_context` now enforces agent-scoped path policy server-side
- default agent recall now pulls from both live agent memories and Foundry distill memories
- OpenClaw repo source no longer ships local extractor / reranker runtime code paths
- OpenClaw `agent_end` now forwards durable session capture internally instead of exposing a raw capture tool

Still missing:

- remove or further minimize local recall fallback in OpenClaw
- move more capture maintenance decisions and heuristics into the offline worker pipeline
- harden distill / forget policies beyond the current first-pass worker implementation
- wire `compact_context` into actual host pre-compaction hooks once available

### Phase 3

Add workerized evolution:

- reflection synthesis
- memory distillation
- forgetting / archival proposals
- skill evolution
- agent evolution proposals

Status:

- foundations in progress

Already done (historical — this manual tool surface has since been retired, see note below):

- `synthesize_agent_evolution` now persists proposal artifacts into Foundry proposal storage
- `queue_agent_evolution` can run synthesis in the background and persist job state
- **`queue_agent_evolution` idempotency**: job ids are deterministic from input fingerprint; re-queue with the same inputs returns `deduped` for active or terminal jobs instead of spawning duplicate synthesis. Stale `running` jobs (>30 minutes) can be reclaimed before retry.
- `list_agent_evolution_proposals` exposes stored proposals for operator review
- `review_agent_evolution_proposal` adds approval / rejection / applied review state

**Retired (#757):** `synthesize_agent_evolution`, `queue_agent_evolution`, `list_agent_evolution_proposals`, and `review_agent_evolution_proposal` are no longer on the MCP tool surface — the manual proposal-review workflow they backed was superseded by the memory-line promotion path (#950, #534). The underlying Foundry proposal storage still exists internally (`crates/tachi-server/src/foundry_ops/`) but is not agent-invocable.

Still missing:

- connect OpenClaw cron and other adapters to the queued evolution path
- add richer evidence ingestion for eval / tooluse / reflection bundles beyond file-path and memory-query inputs
- feed approved proposals into projection instead of stopping at review state
- deepen the public capability layer beyond deterministic first-pass ranking

### Phase 4

Add projection engine:

- write approved proposals into host-native artifacts
- sync profile changes across adapters

Status:

- foundations in progress

Already done (historical — this manual tool surface has since been retired, see note below):

- `project_agent_profile` can load persisted proposals, filter them by approval state, and project them into host documents in memory
- projection is section-aware for markdown-backed targets, so approved proposals can replace or append specific sections instead of rewriting whole files
- `write=true` can write projected content back to disk and mark the applied proposals as `applied`

**Retired (#757):** `project_agent_profile` is no longer on the MCP tool surface — the review-and-project proposal flow it consumed was superseded by the memory-line promotion path (#950, #534).

Still missing:

- connect approved proposals to adapter-specific projection flows instead of manual tool invocation
- expand projection beyond markdown document sections into richer policy/config targets
- add stronger safeguards for concurrent manual edits before enabling broader automatic write-back

## First Deliverable

The first deliverable in `feat/neural-foundry` is intentionally small:

- define canonical Foundry types
- add a manual `synthesize_agent_evolution` entrypoint
- return structured proposals without changing any host files yet

That keeps the migration reversible while the schema stabilizes.

## Next Steps

The next implementation slice should stay focused on finishing the OpenClaw-first migration before expanding the architecture.

### Next slice

- shrink or remove the remaining local recall fallback in OpenClaw
- move more capture maintenance out of the online path and into Foundry workers
- harden `memory_distill` and `forget_sweep` policies with better scoring and archival rules

### After that

- add `agent_evolution` worker jobs that consume profile docs, reflections, evals, and tooluse evidence
- add proposal storage and approval gates
- wire approved proposals into adapter-specific projection flows for `AGENTS.md`, `IDENTITY.md`, and policy files

### Exit criteria for the current migration stage

- OpenClaw no longer needs local extractor / reranker / merge logic for the normal capture path
- Tachi owns the normal online memory capture flow end-to-end
- failed capture windows are durable across temporary Tachi outages
- post-capture maintenance is queued into Foundry workers instead of living entirely inside `capture_session`
- local recall fallback remains acceptable only as a resilience path, not the primary path
