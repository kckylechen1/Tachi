---
description: Dispatch a task to the opencode multi-model lane (ACP) via the lanes plugin — replaces /oc-dispatch.
argument-hint: "[glm|ds|kimi|free|<provider/model>] [--write] [--wait] [--effort <effort>] <task>"
allowed-tools: Agent
---

Dispatch the task below to the **opencode** lane by invoking the `lanes:oc` subagent via the `Agent` tool (`subagent_type: "lanes:oc"`). The subagent is a zero-discretion relay: it calls `lane_dispatch_start` once and long-polls `lane_wait` until the turn completes, its transcript showing the progress digests. Its final message (final_message + result fields) is what you relay to the user.

Raw request:
$ARGUMENTS

Argument mapping (resolve these, then pass explicit parameters to the subagent):

- **Model token** (first token if it is a shortname, or a `--model` value): pass it through as `model` **unchanged** — the server resolves shortnames from the single source in `src/constants.ts` (`OC_MODEL_ALIASES`). For reference, the current map is `glm → zhipuai-coding-plan/glm-5.2`, `ds → deepseek/deepseek-v4-pro`, `kimi → kimi-for-coding/k2p6`, `free → opencode/deepseek-v4-flash-free`; a value containing `/` is a full id. If no model token is given, default to `glm`.
- `--write` present → `read_only: false` **and a `worktree` is mandatory** (the server rejects a write without one). If the user did not name a branch, generate a default `worktree` like `lanes/oc-<short-timestamp>`. Absent `--write` (or `--read-only` present) → `read_only: true` (safe default; no worktree needed).
- `--effort <effort>` → `effort`. Note: the opencode ACP lane does not support a reasoning-effort override; the lane returns a warning and ignores it (surfaced verbatim).
- Everything that is not a recognized flag or the model token is the natural-language `prompt`. Do not forward the flags themselves as prompt text.
- **Background by default**: launch the lane subagent with the `Agent` tool with `run_in_background: true` — the bottom task line tracks it and you are notified on completion; keep working meanwhile. Only if the user passes `--wait` (or explicitly asks to block) run it in the foreground.

Invoke the subagent with an instruction of the form: "Dispatch to your lane. prompt=<task>. read_only=<bool>. worktree=<branch or omit>. model=<token, e.g. glm or zhipuai-coding-plan/glm-5.2>. effort=<effort or omit>. Follow your relay protocol." Relay the subagent's final result verbatim.
