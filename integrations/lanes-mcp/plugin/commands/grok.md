---
description: Dispatch a task to the grok lane (ACP) via the lanes plugin — replaces /grok:dispatch.
argument-hint: "[--write] [--wait] [--model <model>] [--effort <effort>] [--read-only] <task>"
allowed-tools: Agent
---

Dispatch the task below to the **grok** lane by invoking the `lanes:grok` subagent via the `Agent` tool (`subagent_type: "lanes:grok"`). The subagent is a zero-discretion relay: it calls `lane_dispatch_start` once and long-polls `lane_wait` until the turn completes, its transcript showing the progress digests. Its final message (final_message + result fields) is what you relay to the user.

Raw request:
$ARGUMENTS

Argument mapping (resolve these, then pass explicit parameters to the subagent):

- `--write` present → `read_only: false` **and a `worktree` is mandatory** (the server rejects a write without one — writes must be isolated from the primary checkout). If the user did not name a branch, generate a default `worktree` like `lanes/grok-<short-timestamp>`. Absent `--write` (or `--read-only` present) → `read_only: true` (safe default; the lane cannot modify files, no worktree needed).
- `--model <model>` → `model` (passed through verbatim; grok maps it to `grok agent --model`).
- `--effort <effort>` → `effort` (grok `--reasoning-effort`).
- Everything that is not a recognized flag is the natural-language `prompt`. Do not forward the flags themselves as prompt text.
- **Background by default**: launch the lane subagent with the `Agent` tool with `run_in_background: true` — the bottom task line tracks it and you are notified on completion; keep working meanwhile. Only if the user passes `--wait` (or explicitly asks to block) run it in the foreground.

Invoke the subagent with an instruction of the form: "Dispatch to your lane. prompt=<task>. read_only=<bool>. worktree=<branch or omit>. model=<model or omit>. effort=<effort or omit>. Follow your relay protocol." Relay the subagent's final result verbatim.
