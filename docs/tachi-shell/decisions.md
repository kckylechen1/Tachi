# Tachi Shell Decisions

## Document-first before runtime automation

Until Tachi Shell can fully manage its own flows, the project will keep a document-first control plane under `docs/tachi-shell/`.

This avoids relying on chat history for design state, implementation status, validation evidence, and handoffs.

## Tachi Shell is an orchestration facade

`tachi_shell` should not replace lower-level tools.

It should compose existing infrastructure:

- `tachi_task` for dispatch, board, worktrees, and merge mechanics.
- `tachi_skill` for normal skill registry and execution.
- `tachi_memory` / `tachi_save` for summaries, memos, and artifacts.
- `tachi_gh` for GitHub lifecycle actions.
- `tachi_complete` for distillation after completion.

## Meta skills are workflow gates

Superpowers and Gastown-style workflow SOPs are mandatory shell gates, not optional discoverable skills.

They should be injected by `tachi_shell` according to stage and persisted as flow artifacts.

## PR-first ship flow

`ship` should default to PR-first delivery:

1. Finish feature branch.
2. Run tests and required verification.
3. Push feature branch.
4. Open PR.
5. Pass PR gate: CI checks and review gate.
6. Merge PR.
7. Deploy or release if applicable.

Direct push to `main` or protected branches requires explicit human authorization.

## Parallel convoy workflow

For decomposable tasks, Tachi Shell should support a parallel subagent convoy:

- Create multiple worktrees from a common base branch or commit.
- Assign one independent slice per worktree.
- Dispatch one implementer subagent per slice.
- Run review gates per slice.
- Keep branches and PRs separate unless a human explicitly chooses to combine them.
- Track aggregate state through `kanban` / `status`.

This should become a first-class shell mode such as `parallel_worktrees`, not an ad hoc manual pattern.

## Reviewability over batching

Large mechanical refactors should be split into reviewable PRs.

A branch like PR #78 should remain a clean mechanical split. Follow-up stages should use separate branches and PRs unless the dependency structure makes that impossible.

## Artifact-first state

The filesystem remains the authoritative source for flow artifacts during MVP:

- `instruction.md`
- `status.json`
- `events.jsonl`
- `injected/`
- `artifacts/`

Memory entries should store summaries and lifecycle metadata, not duplicate full artifact content by default.
