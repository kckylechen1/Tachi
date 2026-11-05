# Tachi Agent Host Substrate

Status: draft host-neutral implementation note for issue #455. The former
OpenClaw adapter is retired; see [its retirement guidance](../../../integrations/openclaw/README.md).

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

## Trusted-local ACP attachment v1 (#1733)

This leaf defines a deliberately narrow trust model. A trusted local host owns
the ACP process and lifecycle; Tachi owns only admission, the durable receipt,
and a provider-neutral data descriptor. The current host identity and its
admission/connection are runtime evidence, while `agent_identity_id` and the
fresh `WorkClaim` identify the distinct worker that receives the binding.
Receipts written by this path carry the closed
`identity_attribution_basis=trusted_local_host_declared` tier. This is a
declared local attribution, not proof of a verified writer or a remote host.

The worker profile is selected once at MCP initialize for the stdio connection
(`TACHI_PROFILE` is trusted-host launch configuration) and is reused for
`tools/list` and every `tools/call`. Attachment actions are coordinate/admin
surfaces; observe/delegate calls are denied before the handler. ACP negotiated
version 1 is the only stable version accepted by this leaf.

Tachi does not claim hostile same-user isolation, cryptographic host
authentication, remote authority, bearer-token issuance, verified-writer
attribution, process/session lifecycle ownership, or a provider-specific stdio
proxy. The emitted `mcpServers` value is an array of data-only descriptors
(`command`, `args`, and named environment values); the host performs any ACP
`session/new`, `session/load`, or `session/resume` operation itself.

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
- `integrations/openclaw` retains retirement guidance and historical release
  notes; its plugin implementation and lifecycle hooks have been removed.

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

## Retired OpenClaw Adapter

The former OpenClaw memory provider, `continuity_board` wrapper and `host.*`
lifecycle hooks are removed. They are not a current host integration or a
verification target. Shared continuity events and Rust read models remain
host-neutral; their presence does not imply an installed OpenClaw adapter.

## Next Implementation Steps

1. Add a first-class Tachi work-lane read model in core/server instead of
   relying only on generic timeline/project-cycle projections.
2. Add MCP facade actions for lane create/update/list/handoff, keeping them
   host-neutral.
3. Connect Tachi ACP dispatch and `tachi_task(action='board')` into lane state.
4. Add ZeroClaw/Hermes adapters that emit the same host event schema.
5. Teach foreground brokers to dispatch background workers through Tachi ACP
   without each host installing GitHub/Linear/Gmail separately.

## Non-Goals

- Do not make OpenClaw own GitHub, Linear, or memory state directly.
- Do not require every background worker to install every external connector.
- Do not persist full transcripts as durable memory by default.
- Do not make host adapters invent their own lane schema.

## Verification Targets

Verify the retained Rust continuity/context handlers and supported HTTP host
configuration. The removed OpenClaw plugin has no npm build or capability
probe target. Installed legacy copies require separate owner-managed removal.
