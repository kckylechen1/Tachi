---
title: "Tachi Shell Status"
summary: "MVP status, completed features, and next steps for Tachi Shell."
category: "product/tachi-shell"
organize: true
---
# Tachi Shell Status

## Snapshot

Tachi Shell has a working MVP for skill-gated flow artifact generation, but the full asynchronous orchestration product is not complete yet.

## Completed

- Added `tachi_shell` MCP facade with actions: `brainstorm`, `plan`, `dispatch`, `status`, `review`, and `ship`.
- Added flow artifact layout under `.tachi/runs/<flow_id>/`.
- Generate `instruction.md`, `status.json`, `events.jsonl`, and injected meta skill files.
- Added stage-to-meta-skill mapping for Superpowers workflow gates.
- Hardened `flow_id` handling against path traversal.
- Cached git root lookup used by shell artifact resolution.
- Replaced misleading digest wording with `content_hash` / `fingerprint` terminology.
- Added PR-first release flow language to `ship` instruction generation.
- Opened PR #78 for the first large Rust file mechanical refactor.

## Partially complete

- Async dispatch integration exists at the instruction/artifact level, but full durable background orchestration is not yet complete.
- `tachi_shell(action="status")` reports Shell flow state, but a full multi-subagent convoy dashboard through `tachi_task(action="board")` is not yet complete.
- `ship` generates release instructions, but does not yet execute the whole test/gitleaks/commit/push/PR/CI/distill sequence.

## Not yet complete

- GitHub PR/issue lifecycle integration from `tachi_shell`.
- CI-gated ship automation.
- Memory/GitHub linkage for flow artifacts and PR summaries.
- Parallel worktree subagent dispatch as a first-class shell mode.
- Automatic migration from document control plane to runtime shell artifacts.

## Active follow-up work

- PR #78: `refactor/large-rust-files` — mechanical split of `bootstrap.rs` and extraction of `notes_ops.rs`.
- Follow-up refactors planned from PR #78:
  - Stage 2b: split remaining `dispatch_ops.rs` responsibilities.
  - Stage 3: split `tests.rs` by feature area.
  - Stage 4: split `tools.rs` facade wrappers by domain.

## Recommended next steps

1. Land the minimal GitHub Actions CI workflow on `main`.
2. Review and merge PR #78.
3. Continue Stage 2b / Stage 3 / Stage 4 as separate branches or PRs.
4. Add `parallel_worktrees` / convoy design to the formal Tachi Shell plan.
5. Implement `ship` execution in small increments: preflight, PR body, PR creation, CI status, distill.
