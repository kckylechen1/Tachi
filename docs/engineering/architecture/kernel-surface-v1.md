---
title: "Kernel Surface V1 Specification"
summary: "Defines the 5-layer API contract and exposure model for the Tachi kernel."
category: "engineering/architecture"
organize: true
---
# Kernel Surface V1

## Purpose

`Tachi` is the kernel. `Kernel Surface V1` defines the minimal, opinionated contract that the kernel exposes to hosts and agents.

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
- `prepare_capability_bundle`

Status:

- first-pass recommendation APIs are now implemented
- `prepare_capability_bundle` now assembles a host-aware bundle with packs, skills, host tools, and a ready-to-inject section
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
- `prepare_capability_bundle`

### `remember`

- `observe` +
- `save_memory`
- `extract_facts`
- `run_skill`

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
