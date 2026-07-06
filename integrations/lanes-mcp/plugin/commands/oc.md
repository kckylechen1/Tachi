---
description: Dispatch a task to the opencode multi-model lane (ACP) via the lanes plugin — replaces /oc-dispatch.
argument-hint: "[glm|ds|kimi|free|<provider/model>] [--write] [--background] [--effort <effort>] <task>"
allowed-tools: Agent
---

Dispatch the task below to the **opencode** lane by invoking the `lanes:oc` subagent via the `Agent` tool (`subagent_type: "lanes:oc"`). The subagent is a zero-discretion relay: it calls `lane_dispatch_start` once and long-polls `lane_wait` until the turn completes, its transcript showing the progress digests. Its final message (final_message + result fields) is what you relay to the user.

Raw request:
$ARGUMENTS

Argument mapping (resolve these, then pass explicit parameters to the subagent):

- **Model shortname** (first token if it is one of these, or a `--model` value) → full `provider/model` id, mapped exactly as the `oc-dispatch` skill does:
  - `glm`  → `zhipuai-coding-plan/glm-5.2`
  - `ds`   → `deepseek/deepseek-v4-pro`
  - `kimi` → `kimi-for-coding/k2p6`
  - `free` → `opencode/deepseek-v4-flash-free`
  - A value already containing `/` is passed through unchanged.
  - If no model token is given, default to `glm` → `zhipuai-coding-plan/glm-5.2`.
- `--write` present → `read_only: false`. Absent (or `--read-only` present) → `read_only: true` (safe default; the lane cannot modify files).
- `--effort <effort>` → `effort`. Note: the opencode ACP lane does not support a reasoning-effort override; the lane will return a warning and ignore it (surfaced verbatim).
- Everything that is not a recognized flag or the model token is the natural-language `prompt`. Do not forward the flags themselves as prompt text.
- `--background`: this is an execution flag for you, not for the lane. If present, launch the `lanes:oc` subagent with the `Agent` tool in the background (run_in_background) so the bottom task line tracks it and you are notified on completion. If absent, run it in the foreground and block until it returns.

Invoke the subagent with an instruction of the form: "Dispatch to your lane. prompt=<task>. read_only=<bool>. model=<resolved provider/model>. effort=<effort or omit>. Follow your relay protocol." Relay the subagent's final result verbatim.
