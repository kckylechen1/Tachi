# Tachi Agent Host Substrate

Status: draft implementation note for issue #455

## Position

Tachi should be the shared control plane for agent hosts, not a memory library
that every host embeds differently. OpenClaw and ZeroClaw are foreground broker
hosts: they stay online, watch signals, run cron/readouts, dispatch work, and
report the parts that need user judgment. Codex, Claude, OpenCode, Hermes, and
Cursor are background workers: they execute bounded tasks and report evidence
back through Tachi.

```text
External systems
GitHub / Linear / Gmail / Browser / Cron / Files
        |
        v
Tachi Hub + ACP + MCP
auth, tools, evidence, policy, audit
        |
        v
Continuity board / work graph
lanes, ownership, status, blockers, artifacts, handoffs
        |
        v
Agent hosts
foreground brokers: OpenClaw / ZeroClaw
background workers: Codex / Claude / OpenCode / Hermes / Cursor
```

## Current Code Inventory

The repo already has most of the substrate pieces:

- `crates/memcore/src/types/continuity.rs` defines event authority,
  effect scopes, projection kinds, outcome labels, and continuity candidates.
- `crates/memcore/src/db/event_ledger.rs` persists append-only
  `tachi_events`.
- `crates/tachi-server/src/event_ops.rs` exposes `tachi_event` actions:
  `emit`, `query`, `metrics`, `project`, `promote`, `context`, `a2a`, and
  `label_eval`.
- `crates/tachi-server/src/continuity_ops/` projects events into read models
  such as patterns, timelines, project-cycle context, and A2A handoff bundles.
- `crates/tachi-params/src/facade/task.rs` already exposes dispatch,
  board, status, wait, complete, lifecycle, and PR/issue-oriented surfaces.
- `integrations/openclaw` already has MCP-only memory tools and lifecycle hooks,
  but it previously treated OpenClaw mostly as a memory-using agent rather than
  a foreground broker host.

The gap is not raw storage. The gap is a canonical work-lane read model that
turns host events, dispatches, issues, PRs, cron outputs, and handoffs into one
board that any connected agent can read.

## Canonical Work Lane

The durable public object should be a work lane:

```text
lane_id
goal
host
owner_agent
role: controller | executor | observer | reviewer | notifier
status: planned | working | blocked | waiting | done | cancelled
current_step
next_action
blockers
evidence_refs
artifact_refs
issue_refs / pr_refs / file_refs
handoff_summary
last_progress_at
updated_at
```

Raw host/session/cron events should enter `tachi_events` first. Projectors then
derive lane state, continue memory, and pattern memory. Long-term recall should
prefer projected state over raw transcripts.

## OpenClaw Adapter Shape

OpenClaw is the first foreground broker adapter. It should stay thin:

- register native OpenClaw memory capability backed by Tachi MCP;
- expose `continuity_board` for the current parallel work graph;
- translate lifecycle hooks into `host.*` continuity events;
- avoid writing full transcripts as permanent memories;
- expose runtime diagnostics for plugin version, native capability state, DB
  routing, and continuity-board availability.

Current branch changes start this split with:

- `integrations/openclaw/host-continuity.ts` for host role/event/board adapter
  semantics;
- `integrations/openclaw/native-memory.ts` for OpenClaw native memory capability;
- `continuity_board` tool and `host.*` lifecycle event emission.

## Next Implementation Steps

1. Add a first-class Tachi work-lane read model in core/server instead of
   relying only on generic timeline/project-cycle projections.
2. Add MCP facade actions for lane create/update/list/handoff, keeping them
   host-neutral.
3. Connect Tachi ACP dispatch and `tachi_task(action='board')` into lane state.
4. Extend OpenClaw cron/task ingestion so terminal summaries become structured
   lane events rather than Markdown-only observations.
5. Add ZeroClaw/Hermes adapters that emit the same host event schema.
6. Teach foreground brokers to dispatch background workers through Tachi ACP
   without each host installing GitHub/Linear/Gmail separately.

## Non-Goals

- Do not make OpenClaw own GitHub, Linear, or memory state directly.
- Do not require every background worker to install every external connector.
- Do not persist full transcripts as durable memory by default.
- Do not make host adapters invent their own lane schema.

## Verification Targets

- `npm --prefix integrations/openclaw run typecheck`
- `npm --prefix integrations/openclaw run build`
- `openclaw plugins inspect tachi --runtime --json` shows native capability when
  the built plugin is installed in OpenClaw.
- `memory_runtime_info` reports `openclaw_bridge.native_memory_capability`.
- `continuity_board` returns a Tachi A2A/continuity bundle without requiring raw
  transcript search.
