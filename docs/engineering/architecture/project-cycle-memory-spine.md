# Project Cycle Memory Spine

**Status:** implementation slice
**Date:** 2026-06-26
**Related docs:**
- [`tachi-continuity-memory-architecture.md`](./tachi-continuity-memory-architecture.md)
- [`pattern-timeline-bonding-memory.md`](./pattern-timeline-bonding-memory.md)
- [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md)

## Intent

Project Cycle is the read model that connects feature work to memory:

```
GitHub issue -> linked docs/specs -> dispatch/implementation -> PR -> verification -> release note -> close_loop -> memory/wiki distillation
```

It is not a new workflow engine. The existing surfaces remain the write paths:

- `tachi_task(action="intake")` binds a GitHub issue and seeds `.tachi/runs/<flow_id>/`.
- `tachi_task(action="link_pr")` attaches a PR to the flow.
- `tachi_verify` records required checks.
- `tachi_task(action="pr_status")` previews GitHub PR gates.
- `tachi_task(action="release_note")` writes release context.
- `tachi_task(action="close_loop")` records the final issue/docs/wiki sink.
- `tachi_gh(action="safe_merge")` remains the GitHub PR merge surface.
- `tachi_task(action="merge")` remains local dispatched worktree merge only.

`tachi_task(action="cycle_status")` is the read-only projection across those artifacts.
`tachi_task(action="cycle_plan")` is the read-only agent navigation layer on top of
that projection: it turns the same evidence into an ordered checklist, current
blockers, readiness flags, and concrete next command suggestions.

Host adapters should consume this read model rather than inventing host-specific
project state. In particular, `before_prompt` should use `cycle_plan` to attach
the current lifecycle checklist, and `before_stop` should use the same evidence
to decide whether the host can stop or needs a bounded continuation directive.

## Authority Order

The read model reports evidence in this order:

1. `github_active_state`
2. `linked_docs_specs`
3. `verification_evidence`
4. `runtime_artifacts`
5. `memory_checkpoints`
6. `wiki_distillation`

The important boundary is that memory and wiki can preserve lessons, but they do not
override active issue/PR state, linked contracts, or verification evidence.

## Data Sources

`cycle_status` reads from existing local artifacts first:

- `.tachi/runs/<flow_id>/status.json`
- `.tachi/runs/<flow_id>/events.jsonl`
- `.tachi/runs/<flow_id>/verification.json`
- `.tachi/runs/<flow_id>/release_note.md`
- `.tachi/runs/<flow_id>/close_loop.json`
- `.tachi/runs/<flow_id>/result.md`

If `flow_id` is omitted, `issue_ref` or `pr_ref` can locate the most recent matching
local flow by scanning run status files. When no local flow exists, the command can
fall back to external issue/PR refs and report `no_local_flow` drift.

## Output Contract

The `cycle_status` response is JSON-first and read-only:

- `flow_id` / `cycle_id`
- `stage`
- `state`
- `issue_ref`
- `pr_ref`
- `linked_docs`
- `linked_specs`
- `contract_refs`
- `github`
- `verification`
- `artifacts`
- `events`
- `spec_drift`
- `warnings`
- `next_action`
- `source`

`spec_drift` is not a value judgment. It lists missing or inconsistent lifecycle
evidence, including missing docs/specs, missing verification, mismatched refs,
unclosed result artifacts, stale verification heads, or release notes that were
written before the PR gate became ready.

The `cycle_plan` response is also read-only. It does not dispatch, comment, merge,
or close anything. It derives:

- `steps`: ordered lifecycle checkpoints from intake through close_loop.
- `next_step`: the first required checkpoint that is not passed.
- `current_blockers`: blocking gaps from the next step plus blocking drift items.
- `readiness`: booleans for dispatch, PR handoff, PR gate, release note, close_loop,
  and closed state.
- `status_summary`: the status evidence used to derive the plan.

This gives agents a single "what should I do next?" surface without making
`cycle_plan` another write path.

## Why This Belongs With Memory

Continuity memory needs project lifecycle boundaries so it can distinguish:

- planned requirements from implementation results;
- active PR state from stale recollection;
- verified behavior from unchecked claims;
- release/closure lessons from temporary work notes.

This makes Project Cycle the bridge between GitHub work management and long-term
memory distillation. The memory system should distill durable lessons after
`close_loop`, but `cycle_status` keeps the current operational truth anchored in
the flow artifacts.

## Non-Goals

- Do not create a second GitHub sync database.
- Do not merge PRs through `tachi_task(action="cycle_status")` or
  `tachi_task(action="cycle_plan")`.
- Do not treat memory/wiki summaries as higher authority than linked docs/specs or
  verification.
- Do not require live GitHub reads when a local flow already contains the needed
  issue/PR snapshots.
