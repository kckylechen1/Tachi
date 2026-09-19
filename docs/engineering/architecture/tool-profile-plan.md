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
3. Capability selection is **retired as a first-class layer** (#1690 C3 delete list: "skill recommendation and auto-selection").
   - These APIs are retired and deleted end-to-end: `recommend_capability`, `recommend_skill`, `recommend_toolchain`, `prepare_capability_bundle`, and `skill_evolve`; the router rejects them as unknown tools
   - `tachi_skill(action="discover"|"run")` is the canonical skill workflow; `bundle`/`loadout`/`from_pattern` actions are retired
   - a dispatch's skills resolve only from the explicit `skills` parameter plus the profile's static reviewed skill list; internal dispatch-profile recommendation consumes the DecisionFactLedger and abstains when no usable evidence exists
   - raw hub / pack / vc governance tools should not leak into ordinary agent surfaces
4. Workflow tools are not kernel primitives.
   - `ghost_*` stays hidden; all retired routes — `post_card`, `check_inbox`, `update_card`, and the proposal review/project routes — cannot be revived by selecting a host or profile
5. Filtering must only reduce exposure.
   - Effective surface is the intersection of:
     - built-in surface bundle selection
     - `TACHI_EXPOSED_TOOLS`, if present
   - We should not let one layer widen another
6. `agent_register.tool_filter` is deferred.
   - The current in-process `agent_profile` state is shared too broadly for daemon-safe per-session tool filtering
   - Host-level profile selection is safe now; per-session runtime filters come later

## Implemented

### Built-in role surfaces

The bundle bits remain internal action-policy classification, but model-facing
discovery now follows the Lead / Worker / Ops boundary:

- `standard` / Lead — exactly `tachi_memory`, `tachi_task`, `tachi_staff`,
  `tachi_gh`, and `tachi_a2a`.
- `delegate` / Worker — the same five names; action policy allows bounded
  status/read use while denying recursive staffing and GitHub mutation.
- `operate` / Ops — an explicit non-default surface retaining runtime, status,
  Vault-session, Foundry, and Hub diagnostics.
- `admin` / `emergency` — the full retained catalog. Narrow-profile hiding is
  not physical route deletion.
- Legacy `observe`, `remember` (retired as a native tool alias), and `coordinate` selectors retain their
  action-level bundle semantics but their discovery is confined to product
  facades. `companion`, `copilot`, `coach`, and `workflow` are ordinary Lead
  aliases rather than broad bundle combinations.

Selection paths:

- `tachi --profile standard`
- `tachi --profile remember`
- `tachi --profile observe+coordinate`
- `TACHI_PROFILE=openclaw tachi`
- default with no profile: `standard` since v1.0.1; set `TACHI_PROFILE=admin` explicitly for maintenance

Host aliases expand to profile/bundle sets:

- `lead`, `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity`, `companion`, `copilot`, `coach`, `workflow` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`
- `admin`, `full`, `emergency` → `admin` (only as a sole explicit token)

### OpenClaw extension surface

The OpenClaw plugin now keeps its default Tachi-facing model tool surface focused on:

- `memory_search`
- `memory_save`
- `memory_get`

`memory_graph` was dropped (retired) from the plugin's registered tools (and from the
Tachi MCP surface entirely — internalized in #757; the underlying graph
engine remains, just not tool-callable).

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

### Capability recommendation primitive (RETIRED by #1690 C3)

The first-pass capability layer — `recommend_capability`, `recommend_skill`, `recommend_toolchain` — was **deleted end-to-end** in #1690 C3 (the "second model brain" tool family; the router rejects these tools as unknown).

Historical behavior (deterministic ranking over Hub capabilities, visibility/callability-aware and host-aware scoring, Pack/projection-aware toolchain suggestions, and host-tool inference) is kept for record only. The surviving internal dispatch-profile recommendation consumes the DecisionFactLedger and abstains when no usable evidence exists.

## Why This Split

This keeps the model-facing surface small while preserving a rich kernel for:

- OpenClaw hooks
- IDE integrations
- static reviewed skill discovery and enforcement
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
