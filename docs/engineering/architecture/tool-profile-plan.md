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
   - Default Lead and Worker discovery is exactly `tachi_memory`, `tachi_task`, `tachi_staff`, and `tachi_a2a` (`tachi_gh` (removed) removed 2026-10-22), not a bundle-shaped catalog
2. Runtime hooks stay explicit.
   - `recall_context`, `capture_session`, and later `compact_context` are runtime/adapter APIs, not part of the ordinary IDE default
3. Capability selection is **retired as a first-class layer** (#1690 C3 delete list: "skill recommendation and auto-selection").
   - These APIs are retired and deleted end-to-end: `recommend_capability`, `recommend_skill`, `recommend_toolchain`, `prepare_capability_bundle`, and `skill_evolve`; the router rejects them as unknown tools
   - `tachi_skill(action="discover"|"run")` is retained only for explicit Ops/admin compatibility; ordinary agents use their host-native skill loader; `bundle`/`loadout`/`from_pattern` actions are retired
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
  and `tachi_a2a` (`tachi_gh` (removed) was removed 2026-10-22).
- `delegate` / Worker — the same four names; action policy allows bounded
  status/read use while denying recursive staffing.
- `operate` / Ops — an explicit non-default surface retaining runtime, status,
  Foundry and Hub diagnostics.
- `admin` / `emergency` — the full retained catalog. Narrow-profile hiding is
  not physical route deletion.
- Legacy `observe`, `remember` (retired as a native tool alias), and `coordinate` selectors retain their
  action-level bundle semantics but their discovery is confined to product
  facades. `companion`, `copilot`, `coach`, and `workflow` are ordinary Lead
  aliases rather than broad bundle combinations.

Current clients select an admitted public profile through native HTTP client
configuration. Owner management uses supported CLI commands; public caller
metadata cannot select Ops/admin authority. See [HTTP direct connect](http-direct-connect.md).

Host aliases expand to profile/bundle sets:

- `lead`, `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity`, `companion`, `copilot`, `coach`, `workflow` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`
- `admin`, `full`, `emergency` → `admin` (only as a sole explicit token)

### Retired OpenClaw extension

The OpenClaw plugin, memory tools, lifecycle hooks and experimental passthroughs
are removed. The historical `openclaw` profile alias does not provide an
installed adapter or authorize public HTTP management. See the
[retirement guidance](../../../integrations/openclaw/README.md).

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

This is a typed runtime primitive. The OpenClaw adapter is retired; there is no pending OpenClaw compaction-hook implementation in this plan.

### Capability recommendation primitive (RETIRED by #1690 C3)

The first-pass capability layer — `recommend_capability`, `recommend_skill`, `recommend_toolchain` — was **deleted end-to-end** in #1690 C3 (the "second model brain" tool family; the router rejects these tools as unknown).

Historical behavior (deterministic ranking over Hub capabilities, visibility/callability-aware and host-aware scoring, Pack/projection-aware toolchain suggestions, and host-tool inference) is kept for record only. The surviving internal dispatch-profile recommendation consumes the DecisionFactLedger and abstains when no usable evidence exists.

## Why This Split

This keeps the model-facing surface small while preserving a rich kernel for:

- IDE integrations
- static reviewed skill discovery and enforcement
- section / compaction artifacts
- operator and workflow tooling

It also avoids the old mutually exclusive trap where a client needed a workflow tool and suddenly had to choose between unrelated surfaces. Bundles compose upward, and aliases remain for backward compatibility.

## Next Steps

1. Harden the capability layer
   - add richer outcome signals
   - connect more pack / projection metadata
2. Revisit per-session filtering
   - Move runtime tool filters off shared server state before extending `agent_register`

## Verification

```bash
cargo test -p tachi-server
```
