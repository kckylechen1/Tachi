---
title: "Tachi Shell Control Plane"
summary: "Document-first control plane for Tachi Shell design and status."
category: "agent/tachi-shell"
organize: true
---
# Tachi Shell Control Plane

This directory is the document-first control plane for Tachi Shell while the runtime implementation is still evolving.

The goal is to keep design decisions, implementation status, validation evidence, and migration targets outside chat history so they can later be migrated into `tachi_shell` artifacts, memory summaries, and GitHub lifecycle automation.

## Current purpose

- Preserve the Tachi Shell design in a reviewable form.
- Track what has already shipped versus what remains planned.
- Record workflow decisions such as PR-first shipping and parallel worktree subagent dispatch.
- Provide a stable handoff surface for human operators and subagents.

## Source of truth

- Detailed original design: `wiki/agent/tachi/Tachi-Shell-重构计划-2026-05-04.md`
- Current implementation/status summary: `status.md`
- Durable design decisions: `decisions.md`

## Migration target

When Tachi Shell is mature enough, this directory should map to first-class shell artifacts:

- `README.md` -> shell overview / help surface
- `status.md` -> `tachi_shell(action="status")`, `tachi_task(action="board")`, and memory summaries
- `decisions.md` -> design memory / wiki entries / ship gates
- follow-up task lists -> GitHub issues or PR checklists only when lifecycle management is needed
