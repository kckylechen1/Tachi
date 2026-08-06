# Project Cycle Memory Spine

**Status:** implementation slice
**Date:** 2026-06-26
**Related docs:**
- [`tachi-continuity-memory-architecture.md`](./tachi-continuity-memory-architecture.md)
- [`pattern-timeline-bonding-memory.md`](./pattern-timeline-bonding-memory.md)
- [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md)
- [`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md)

## Intent

Project Cycle is the read model that connects feature work to memory:

```
GitHub issue -> linked docs/specs -> dispatch/implementation -> PR -> verification -> release note -> close_loop -> memory/wiki distillation
```

It is not a new workflow engine. The existing surfaces remain the write paths:

- `tachi_task(action="intake")` binds a GitHub issue and seeds `.tachi/runs/<flow_id>/`.
- `tachi_gh(action="link_pr")` attaches a PR to the flow.
- `tachi_verify` records required checks.
- `tachi_gh(action="pr_status")` previews GitHub PR gates.
- `tachi_gh(action="release_note")` writes release context.
- `tachi_task(action="close_loop")` records the final issue/docs/wiki sink.
- `tachi_gh(action="safe_merge")` remains the GitHub PR merge surface.
- Local worktree merge left `tachi_task` in #1683; GitHub PR merges use `tachi_gh(action="safe_merge")`.

`tachi_task(action="cycle_status")` is the read-only projection across those artifacts
and the agent navigation layer on top of it: it surfaces lifecycle evidence
(`stage`, `spec_drift`, `warnings`) and a concrete `next_action` string.

Host adapters should consume this read model rather than inventing host-specific
project state. In particular, `before_prompt` should use `cycle_status` to attach
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
- **target extension (not implemented yet):** `issue_body_hash`,
  `issue_snapshot_hash`, `linked_specs[].commit_sha`,
  `linked_specs[].blob_sha`, and `linked_specs[].section`
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

The `cycle_status` response is also read-only. It does not dispatch, comment, merge,
or close anything. Agents should follow the string field `next_action` (not a
retired `next_step` / `cycle_plan` shape). That field is the single
"what should I do next?" surface without making `cycle_status` another write path.

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

Issue refinement uses the same authority order but requires immutable snapshots.
The current read model stores raw issue/doc refs; adding body hashes and blob/section
anchors is a target extension. Once that extension lands, an `issue_ref` without
an issue body hash is `ref_only`, and a doc path without a blob SHA is advisory.
Neither may be promoted to a high-confidence current-work answer by memory or wiki
similarity. See
[`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md).

## Non-Goals

- Do not create a second GitHub sync database.
- Do not merge PRs through `tachi_task(action="cycle_status")` or
  `tachi_task(action="cycle_status")`.
- Do not treat memory/wiki summaries as higher authority than linked docs/specs or
  verification.
- Do not require live GitHub reads when a local flow already contains the needed
  issue/PR snapshots.
