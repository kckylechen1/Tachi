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

- `tachi_shell` now exposes exactly `dispatch` and `status`: dispatch writes bounded packets and may launch only an admitted durable/remote exception; status is read-only.
- Added flow artifact layout under `.tachi/runs/<flow_id>/`.
- Generate `instruction.md`, `status.json`, `events.jsonl`, and injected meta skill files.
- Retained dispatch-to-executing-plans SOP injection and shared skill resolution.
- Hardened `flow_id` handling against path traversal.
- Cached git root lookup used by shell artifact resolution.
- Replaced misleading digest wording with `content_hash` / `fingerprint` terminology.
- Removed the packet-only brainstorm, plan, review, and ship actions; canonical planning, review, and GitHub shipping remain on their owning surfaces.
- Opened PR #78 for the first large Rust file mechanical refactor.

## Partially complete

- Async dispatch integration exists at the instruction/artifact level, but full durable background orchestration is not yet complete.
- `tachi_shell(action="status")` reports Shell flow state, but a full multi-subagent convoy dashboard through `tachi_task(action="board")` is not yet complete.

## Not yet complete

- GitHub PR/issue lifecycle and CI-gated shipping remain owned by `tachi_gh`, `tachi_task`, and `tachi_verify`, not Shell.
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
