---
name: implement-plan
description: |
  Execute a previously-generated dispatch plan step by step. Read the plan in
  `plan.md`, follow each numbered step in order, edit only the files listed in
  the Files section, and verify success by running the commands in the
  Validation section. Stop and report failure if any validation command fails.
---

# implement-plan

Tachi Dispatch V2 Stage 2 skill. Loaded by `dispatch_ops::v2` when an
operator requests a two-stage Plan → Execute dispatch.

## Inputs

- `plan.md` (in the dispatch run directory) — produced by Stage 1 with
  four sections:
  - `## Goal`
  - `## Steps` (numbered)
  - `## Files`
  - `## Validation`
- The original task description.

## Workflow

1. Read `plan.md` end-to-end before touching any file.
2. Work through `## Steps` in order. Do not skip ahead and do not
   reorder steps unless a step explicitly fails and you need to recover.
3. Limit file edits to the paths declared in `## Files`. If you need to
   touch a file that is not listed, stop and surface the deviation in
   `result.md`.
4. After all steps are applied, run every command in `## Validation`.
   Any non-zero exit means the dispatch failed — capture the failing
   output verbatim and call `tachi_complete` with `outcome=failure`.
5. On full success, call `tachi_complete` with `outcome=success` and
   include the validation output in the eval note.

## Constraints

- Do **not** regenerate or rewrite the plan — Stage 1 already produced
  it and (optionally) a human approved it.
- Do **not** introduce out-of-scope refactors. The plan is the contract.
- Treat the validation section as ground truth: a green test suite that
  doesn't match the plan's validation is still a failure.
