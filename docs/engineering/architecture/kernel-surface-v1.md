---
title: "Kernel Surface V1 Specification"
summary: "Defines the 5-layer API contract and exposure model for the Tachi kernel."
category: "engineering/architecture"
organize: true
---
# Kernel Surface V1

## Purpose

`Tachi` is the kernel. `Kernel Surface V1` defines the minimal, opinionated contract that the kernel exposes to hosts and agents.

This is a target contract, not a claim that every named operation is already a
public server API. Rows below call out whether they are current, partially
current, or proposed so downstream forks do not treat future vocabulary as an
implemented compatibility promise.

The goal is not to expose every primitive. The goal is to expose the right layers:

1. kernel
2. capability
3. runtime
4. workflow
5. admin

The core boundary is:

`Tachi does not need to own all execution tools; it needs to understand, recommend, and orchestrate them.`

## Layer 1: Kernel

These are the durable state and memory primitives that define the kernel itself.

- memory
- fact
- edge
- state
- section
- compact artifacts

Examples:

- `memory_search`
- `memory_save`
- `memory_get`
- `memory_graph`
- `section.build`
- `compact.rollup`
- `compact.session_memory`

This layer owns the canonical memory graph and its maintenance lifecycle.

### Portable Memory Kernel Manifest

The portable kernel is the part downstream memory consumers can take without
inheriting Tachi's operator/product surfaces. Hypermem, Hyperion, zeroclaw-style
chat harnesses, OpenClaw-style plugins, and local CLIs should be able to depend
on this bundle and still keep their own product policy outside Tachi.

The portable bundle includes:

| Surface | Layer | Portable contract |
|---|---|---|
| `memory-core` | `kernel` | Canonical memory rows, graph edges, store/open/read/write contracts, access history, and scorer types. |
| Memory DB/schema contracts | `kernel` | Stable schema expectations for `memories`, FTS, `memory_edges`, access history, event ledger, and migration-owned columns. |
| Recall/scorer/RRF | `kernel` | Hybrid search, symbolic/FTS/vector scoring, RRF/MMR ranking, decay scoring, and recall diagnostics inputs. |
| Vector/backfill | `kernel` | Embedding readiness, vector coverage, backfill state, and degraded-mode reporting. |
| Event ledger/projection | `kernel` | Append-only continuity events plus read-model projection hooks for adapters. |
| Library identity/routing | `runtime_adapter` | Project/global library identity, path binding, and routing receipts needed to keep writes/read scopes correct. |
| Status/readiness | `runtime_adapter` | Health/readiness output for vector gaps, locked vault, unavailable kernel, and degraded recall. |

It explicitly excludes GitHub, dispatch, ship, release, PR lifecycle, CI merge,
and other workflow/product automation. Those tools can use the kernel; the
kernel must not require them.

The machine-readable fixture for this contract is
[`kernel-surface-v1.fixture.json`](./kernel-surface-v1.fixture.json). Downstream
agents should cite that fixture when deciding whether a change belongs upstream
in Tachi or downstream in an adapter/product layer.

### Backend Boundary

The portable boundary intentionally mirrors the small shape seen in OMP's
`MemoryBackend`, but remains Tachi-owned and local-first:

| Operation family | Required | Implementation status | Tachi-owned meaning |
|---|---|---|---|
| `status` / `readiness` | yes | current | Report kernel availability, DB identity, vector/backfill coverage, and degraded modes. |
| `search` / `recall` | yes | current via `tachi_memory search` and `recall_simulate`; richer recall diagnostics are target contract | Return ranked memories plus provenance, score components where available, and stable row shape. |
| `save` / `retain` | yes | current via `tachi_memory save`; `retain` is adapter vocabulary | Store durable facts, experiences, observations, and policy-labeled adapter memory with provenance. |
| developer/briefing context | yes | current via briefing/readiness facades, target for generic adapters | Produce compact memory context for a host before a prompt/session without exposing product workflow tools. |
| readiness/diagnostics | yes | partially current; portable diagnostic API is target contract | Report recall lanes, fallback behavior, true-empty recall, vector health, and adapter-visible failures. |
| lifecycle hooks | optional where wired | proposed/target | Consume neutral host events such as `before_session`, `before_prompt`, `after_compact`, and `after_session`. |

Current continuity events are narrower than the target lifecycle vocabulary:
`memory.saved` exists today; retain/recall/reflect request and response events
remain proposed unless a later issue adds executable emission and projection
tests for them.

This boundary is deliberately smaller than Tachi's full MCP surface. A consumer
that only wants memory should not need `tachi_gh`, `tachi_task` dispatch,
`ship`, release notes, or GitHub PR lifecycle actions.

### Memory Ontology

Hindsight's four-network ontology is useful as design evidence. Tachi does not
clone Hindsight, but the portable kernel should preserve the same distinctions:

| Ontology | Tachi status | Notes |
|---|---|---|
| `world` / fact | in-kernel now | Stored through categories, summaries/text, entities, provenance, validity, and graph support/contradiction edges. |
| experience | in-kernel now | Stored as durable memories/events with path, source, access history, and session/project provenance. |
| opinion / preference | policy-hooked | The kernel can store the rows; adapter policy decides authority, persona impact, and whether to inject them. |
| observation / summary | policy-hooked now, kernel lifecycle later | Continue memory, distillation, projections, and pattern rows exist; typed consolidation/forgetting policy remains follow-up work. |

The rule is structural: the kernel owns durable storage, provenance, recall, and
graph relations; adapters own persona, product defaults, domain interpretation,
and final prompt composition.

## Layer 2: Capability

This is the missing “librarian brain” layer.

It should answer:

- which skill is best for this task
- which host tools are appropriate
- which toolchain has worked before
- which pack or extension should be activated

Candidate APIs:

- `recommend_capability`
- `recommend_skill`
- `recommend_toolchain`
- `tachi_skill(action="bundle")`

Status:

- first-pass recommendation APIs are now implemented
- `tachi_skill(action="bundle")` now assembles a host-aware bundle with packs, skills, host tools, and a ready-to-inject section; standalone `prepare_capability_bundle` remains a compatibility route
- current implementation is deterministic and Hub/Pack-aware
- future iterations can add richer outcome learning and LLM-assisted planning on top

This layer does not execute the host’s tools directly. It selects and orchestrates them using:

- memory
- tooluse history
- host constraints
- prior outcomes
- profile and policy

## Layer 3: Runtime

These are host/runtime-facing primitives used by adapters and hooks, not by default model-facing tool lists.

Examples:

- `recall_context`
- `capture_session`
- `compact_context`
- host lifecycle hooks (`before_session`, `before_prompt`, `after_tool`,
  `after_compact`, `before_stop`, `after_session`)

Runtime primitives are called by:

- `before_agent_start`
- `agent_end`
- future `before_compaction`
- other host lifecycle hooks

This layer is where OpenClaw and other adapters talk to Tachi during a live turn.
The neutral lifecycle contract is defined in
[`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md).

## Layer 4: Workflow

These are higher-order coordination and reflective flows.

Examples:

- `ghost_*`
- kanban / delegation
- handoff
- evolution review / projection flows

These should be:

- hidden by default
- surface-gated
- exposed only to the right host or operator flow

The key rule is that workflow tools should not be mixed into the default kernel surface.

## Layer 5: Admin

These are operator and system management capabilities.

Examples:

- hub
- vault
- sandbox
- pack
- vc
- dlq

These stay out of the normal agent-facing surface.

## Host Boundary

### Tachi

Owns:

- memory graph
- extraction
- embedding
- reranking
- distillation
- forgetting / archival
- capability recommendation
- skill evolution
- agent evolution
- profile projection

### Host adapters

Own:

- runtime timing
- lifecycle hooks
- final context assembly
- token-pressure decisions
- exposure policy
- host-native execution tool invocation

### Host-native tools

Host-native execution tools are not part of the Tachi kernel surface, but they are part of the capability model.

Examples:

- shell
- browser
- python
- filesystem
- excel or spreadsheet tooling

Tachi should know about them well enough to recommend and orchestrate them, even when the host owns actual execution.

## Default Exposure Model

Tachi now expresses exposure through additive bundles instead of mutually exclusive profiles.

### `observe`

- `search_memory`
- `tachi_memory(action="get")`; native `get_memory` is admin/backcompat only
- `memory_graph`
- `list_memories`
- `memory_stats`
- `get_edges`
- `recommend_capability`
- `recommend_skill`
- `recommend_toolchain`
- `tachi_skill(action="bundle")`; native `prepare_capability_bundle` is backcompat only

### `remember`

- `observe` +
- `save_memory`
- `extract_facts`
- `tachi_skill(action="run")`; native `run_skill` is backcompat only

### `coordinate`

- `remember` +
- `ghost_*`
- kanban / delegation / handoff

### `operate`

- `remember` +
- `recall_context`
- `capture_session`
- `compact_context`
- evolution queue / review / projection helpers
- `hub_call`

### `admin`

- full surface, including hub governance, pack, vc, vault, sandbox, and destructive operations

## Why This Matters

Without the capability layer, Tachi becomes:

- a big MCP server with too many raw tools
- a strong memory kernel with no recommendation brain

With the capability layer, Tachi becomes:

- memory kernel
- capability catalog
- recommendation engine
- orchestration brain

That is the intended direction for `Neural Foundry`.
