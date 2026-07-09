---
title: "Tool Surface Bundle Plan"
summary: "Defines tool exposure policies and surface bundles for Tachi, OpenClaw, and IDE hosts."
category: "engineering/architecture"
organize: true
---
# Tool Surface Bundle Plan

This document is the exposure-policy companion to [Kernel Surface V1](./kernel-surface-v1.md).  
`Kernel Surface V1` defines the conceptual layers; this document defines how much of that surface each host sees by default.

## Goal

Keep `Tachi` as the full kernel, but stop showing the full kernel to every host and every agent.

The target split is:

- `Tachi`
  - owns kernel, capability, runtime, workflow, and admin primitives
- `OpenClaw`
  - owns hooks, runtime timing, section assembly, and agent-facing tool exposure
- `IDE / direct MCP clients`
  - see a small host-appropriate tool subset instead of the full admin catalog (100+ tools)

## Principles

1. Agent-facing tools stay tiny.
   - Default agent surface should be a narrow `observe + remember` kernel, not the full MCP catalog
2. Runtime hooks stay explicit.
   - `recall_context`, `capture_session`, and later `compact_context` are runtime/adapter APIs, not part of the ordinary IDE default
3. Capability selection should become a first-class public layer.
   - `recommend_capability`, `recommend_skill`, and `recommend_toolchain` remain direct recommendation APIs; `tachi_skill(action="discover"|"run"|"bundle")` is the preferred skill workflow UX
   - standalone `run_skill`, `prepare_capability_bundle`, and skill-focused `hub_discover` calls are compatibility routes, not the canonical new-caller path
   - raw hub / pack / vc governance tools should not leak into ordinary agent surfaces
4. Workflow tools are not kernel primitives.
   - `ghost_*`, `post_card`, `check_inbox`, `update_card`, proposal review/project tools stay hidden unless a host or profile explicitly asks for them
5. Filtering must only reduce exposure.
   - Effective surface is the intersection of:
     - built-in surface bundle selection
     - `TACHI_EXPOSED_TOOLS`, if present
   - We should not let one layer widen another
6. `agent_register.tool_filter` is deferred.
   - The current in-process `agent_profile` state is shared too broadly for daemon-safe per-session tool filtering
   - Host-level profile selection is safe now; per-session runtime filters come later

## Implemented

### Built-in additive bundles

`Tachi` now understands additive surface bundles instead of mutually exclusive profiles:

- `observe`
  - capability recommendation
  - read-only memory and graph inspection
- `remember`
  - `observe` +
  - `save_memory`
  - `extract_facts`
  - `tachi_skill(action="run")`; standalone `run_skill` remains for compatibility
- `coordinate`
  - `remember` +
  - kanban / ghost / handoff collaboration tools
- `operate`
  - `remember` +
  - runtime hook primitives (`recall_context`, `capture_session`, `compact_*`, `section_build`)
  - routed execution / evolution helpers (`hub_call`, proposal queue/review/project tools, `agent_register`)
- `admin`
  - full surface, including hub governance, pack management, vault, sandbox, VC, and destructive operations

There are also two curated minimal profiles for common hosts:

- `standard` — default for IDE agents. Intersects the bundles with a daily facade surface (`tachi_tools`, `runtime_info`, `tachi_status`, `tachi_task`, `tachi_arena`, `tachi_verify`, `tachi_memory`, `tachi_briefing`, `tachi_save`, `tachi_web_search`, `tachi_wiki`, `tachi_skill`, `vault_status`, `tachi_gh`). `tachi_arena` remains in this surface because main agents need a tracked subagent/advisor mission ledger, and `tachi_web_search` remains because some hosts lack native search and future wiki/research-ledger flows need a canonical search intake. The canonical list is `STANDARD_MINIMAL_TOOL_PATTERNS` in `crates/tachi-server/src/profiles/patterns.rs`.
- `delegate` — for worker subagents spawned by `tachi_task(action='dispatch')`. A 7-tool surface with no dispatch and no handoff.

Selection paths:

- `tachi --profile standard`
- `tachi --profile remember`
- `tachi --profile observe+coordinate`
- `TACHI_PROFILE=openclaw tachi`
- default with no profile: `standard` since v1.0.1; set `TACHI_PROFILE=admin` explicitly for maintenance

Host aliases expand to profile/bundle sets:

- `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`
- `workflow` → `coordinate + operate`
- `admin`, `full` → `admin`

### OpenClaw extension surface

The OpenClaw plugin now keeps its default Tachi-facing model tool surface focused on:

- `memory_search`
- `memory_save`
- `memory_get`
- `memory_graph`

High-risk passthroughs and runtime-only helpers are now hidden by default. They can be re-enabled explicitly with `TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS=1`.

Examples of gated tools:

- `memory_delete`
- `compact_context`
- `tachi_vault_*`
- `tachi_ghost_*`
- `tachi_kanban_*`
- `tachi_get_handoff` / `tachi_create_handoff`
- `tachi_hub_discover`

Internally, the plugin still uses runtime-only MCP primitives through hooks:

- `before_agent_start` → `recall_context`
- `agent_end` → `capture_session`

OpenClaw now forces `TACHI_PROFILE=openclaw` when it launches the embedded MCP client.

### Compaction primitive

`Tachi` now exposes `compact_context` as a runtime-only API.

- Input
  - `agent_id`
  - `conversation_id`
  - `window_id`
  - `messages`
  - token-budget hints
- Output
  - `compacted_text`
  - `estimated_tokens`
  - topic/signal summaries for later section work

Today this is a typed MCP/runtime primitive, not an OpenClaw hook integration yet. The current OpenClaw SDK only exposes `before_agent_start` and `agent_end`, so the actual `before_compaction` wiring is deferred until the host exposes that lifecycle event.

### Capability recommendation primitive

`Tachi` now exposes a first-pass capability layer:

- `recommend_capability`
- `recommend_skill`
- `recommend_toolchain`

Current behavior:

- deterministic ranking over Hub capabilities
- visibility/callability aware
- host-aware scoring
- Pack / projection-aware toolchain suggestions
- simple host-tool inference for common task shapes

OpenClaw does not expose these directly to the model yet. They are part of the kernel surface for direct MCP hosts and future adapter orchestration.

## Why This Split

This keeps the model-facing surface small while preserving a rich kernel for:

- OpenClaw hooks
- IDE integrations
- capability recommendation and bundle preparation
- section / compaction artifacts
- operator and workflow tooling

It also avoids the old mutually exclusive trap where a client needed a workflow tool and suddenly had to choose between unrelated surfaces. Bundles compose upward, and aliases remain for backward compatibility.

## Next Steps

1. Wire OpenClaw into `compact_context`
   - as soon as the SDK exposes a pre-compaction lifecycle hook
2. Harden the capability layer
   - add richer outcome signals
   - connect more pack / projection metadata
3. Wire cron to queued evolution
   - OpenClaw cron triggers Tachi evolution jobs
   - Tachi can already load document paths, evidence paths, and memory queries directly
4. Revisit per-session filtering
   - Move runtime tool filters off shared server state before extending `agent_register`

## Verification

```bash
cargo test -p tachi-server
npm --prefix integrations/openclaw run build
```
