# Subagent Eval System

Status: active architecture record
Updated: 2026-06-05

This record documents the Tachi subagent system built from the June 5, 2026
direct-push batch. The goal is not simply to run more agents. The goal is to
make helper agents measurably useful: a leader can delegate bounded work, verify
the result, and record enough evidence for future model-role routing decisions.

## Problem

Long coding sessions exposed a repeatable pattern:

- The leader benefits from specialist helpers for exploration, architecture
  critique, code execution, and verification.
- Different models appear to have different strengths, but subjective memory is
  too weak for routing policy.
- Raw helper transcripts are noisy and can contaminate future retrieval.
- If helper quality is not evaluated, the system becomes theater: agents were
  used, but no durable learning happened.

Tachi's role is to preserve the useful part of the experience outside the model
weights: compact memory, production eval rows, and routing evidence.

## Design

The leader remains accountable for the final patch and verification. Subagents
are bounded helpers, not autonomous owners of the whole task.

Typical roles:

| Role | Purpose | Good signal |
|---|---|---|
| `explore` | Map files, symbols, contracts, and likely edit sites | Exact references, caveats, no broad rewrite |
| `architect` | Challenge design, schema, lifecycle, and future compatibility | Finds missing dimensions and simpler durable shape |
| `critic` | Review risks and regressions before commit | Concrete failure modes, test gaps, rollback risks |
| `executor` | Implement a bounded slice | Small correct diff, local tests, no scope expansion |
| `verifier` | Check completion evidence | Reproducible commands and clear pass/fail result |

The leader records each helper in `tachi_complete.subagents` using structured
fields:

```json
{
  "role": "architect",
  "agent": "kimi",
  "model": "kimi-code/kimi-for-coding",
  "task": "Review the live eval architecture and identify missing schema fields.",
  "task_type": "plan_request",
  "outcome": "useful",
  "usefulness_score": 0.82,
  "verification_impact": "changed_plan",
  "verification_present": true,
  "evaluator": "leader",
  "plan_delta": "modified",
  "human_override": false,
  "retry_count": 0,
  "latency_ms": 2100,
  "input_tokens": 1200,
  "output_tokens": 240,
  "failure_mode": null
}
```

Do not store raw child transcripts, hidden reasoning, or uncompressed logs in
memory. Store the leader's compressed judgment and the verification impact.

## Memory Boundary

Subagent eval rows are production eval data, not ordinary working memory.

Current behavior:

- `tachi_complete` writes task outcomes under `/eval/YYYY-MM-DD/<task_id>`.
- Eval rows use `category="eval"`.
- Ordinary memory search excludes `/eval` / `category=eval`.
- Explicit `/eval` scoped search can retrieve eval rows.
- `tachi_agent_eval(action="aggregate_live")` aggregates live `/eval` memory.
- `tachi_agent_eval(action="aggregate", fixture_path=...)` is fixture replay for
  local evaluation only and requires `TACHI_AGENT_EVAL_ALLOW_FIXTURE=1`.

This keeps Tachi memory-first without creating an unmanaged JSONL double-write
track. If production eval volume later becomes large, add a dedicated indexed
ledger or generated-column indexes instead of storing duplicate live state.

## Operating Protocol

1. Start with a Tachi briefing for non-trivial work.
2. Decide whether subagents materially improve quality or speed.
3. Delegate only bounded, verifiable slices.
4. Require each helper to report evidence, not just conclusions.
5. The leader integrates, edits, verifies, and owns final output.
6. Call `tachi_complete` with `subagents=[...]`.
7. Run `tachi_agent_eval(action="aggregate_live")` periodically to update
   routing hypotheses.

`aggregate_live` now returns three evidence layers:

| Field | Purpose |
|---|---|
| `scores` | leader-level success and verification rates by agent/task type |
| `subagent_scores` | helper usefulness, plan-change, and failure counts by role/agent/model/task type |
| `performance_matrix` | leader and subagent latency, token, cost, quality, verification, retry, and override metrics |

Use `tachi_agent_eval(action="telemetry")` or `action="perf"` as readable aliases
when the goal is specifically to inspect the live performance matrix. These
aliases use the same `/eval` memory source; they do not create a second ledger.
`tachi_task(action="recommend")` also consumes this matrix: high human override
rates, repeated retries, failures, slow average latency, and high average cost
reduce profile confidence, while low-cost high-quality rows can add a small
efficiency bonus.

Do not count a helper as useful just because it produced text. Count it as
useful when it changed the plan, found a real risk, saved time, improved
verification, or produced a bounded patch the leader accepted.

## Native Skill Policy

Superpowers and Waza are native workflow gates for subagent orchestration, not
only Hub-discoverable skills.

Tachi applies lifecycle skills by stage:

| Stage | Native gates |
|---|---|
| `brainstorm` | `skill:superpowers-brainstorming`, `skill:waza-think` |
| `plan` | `skill:superpowers-writing-plans`, `skill:waza-think` |
| `dispatch` | `skill:superpowers-subagent-driven-development`, `skill:superpowers-executing-plans`, `skill:waza-tachi` |
| `execute` worker | `skill:superpowers-executing-plans` plus task-routed Waza/coding skills |
| `review` | `skill:superpowers-requesting-code-review`, `skill:superpowers-verification-before-completion`, `skill:waza-check` |
| `ship` | `skill:superpowers-verification-before-completion`, `skill:superpowers-finishing-a-development-branch`, `skill:waza-check` |

Worker role bindings:

| Worker task shape | Native skill |
|---|---|
| PR, issue, diff, release, merge review | `skill:waza-check` |
| Bug, crash, regression, repeated failure | `skill:waza-hunt` |
| UI, frontend, screenshot, component design | `skill:waza-design` |
| Multi-source research | `skill:waza-learn` |
| URL or PDF reading | `skill:waza-read` |
| Docs, release notes, prose | `skill:waza-write` |
| MCP, hooks, config, agent health | `skill:waza-health` |
| Tachi memory/task work | `skill:waza-tachi` |

The instruction packet must include the worker factory contract: leader-owned
integration, max six concurrent helpers, independent slices only, explicit
scope and validation, role-scoped MCP/tool permission injection, and mandatory
report-back. Child output remains draft evidence until the leader verifies it.

This follows the useful parts of the observed external systems:

- Amp: separate Oracle, Task, and Codebase Search lanes; prompt every worker
  with goal, deliverables, procedure, validation, constraints, and context.
- Codex native subagents: leader owns integration and final verification; model
  inheritance is the default; bounded children should not recursively
  orchestrate.
- OpenCode/OMO: typed adapters should be explicit, not hidden behind `custom`;
  default to read-only workers, deny nested delegation unless a team policy
  enables it, and treat child-idle/TODO completion as liveness rather than
  quality proof.

The typed adapter boundary is expanded in
[`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md): host systems
may provide lifecycle hooks, post-edit feedback, continuation gates, and
execution transports, but Tachi remains the source of truth for project-cycle
state, verification evidence, memory, and profile projection.

## Current Routing Hypothesis

This is a starting hypothesis, not a leaderboard:

| Role | Primary candidates | Notes |
|---|---|---|
| `explore` / `search` / `librarian` | DeepSeek V4 Flash | Cheap, broad repo scanning, repeated structure queries |
| `architect` / long-context challenge | Kimi Code / K2.6 | Good for schema and lifecycle critique |
| `executor` | GLM 5.1, DeepSeek V4 Pro | Use when edit slice is bounded and tests are clear |
| `critic` | DeepSeek V4 Pro, GPT frontier | Needs real risk detection, not rewrite impulses |
| final integration | GPT frontier leader | Owns judgment, verification, and user-facing result |

Update this table only from eval rows containing task type, model, outcome,
latency, cost, verification, human override, plan delta, and failure mode.

## Known Risks

- `usefulness_score` is subjective. Treat it as weak evidence unless paired with
  `verification_present`, `plan_delta`, or a concrete failure mode.
- `task_type` must be filled consistently or aggregation becomes misleading.
- Subagent self-report is lower trust than leader or human evaluation.
- Host CLI model aliases drift. Record the concrete model string when known.
- Eval rows should not leak into ordinary recall; keep `/eval` opt-in.

## Verification Surface

The June 5 implementation was verified with:

- `cargo test -p memory-server`
- `cargo build -p memory-server --release`
- `target/release/memory-server --help`
- Release-binary `remember/search` smoke with temporary DB overrides
- Tachi MCP save using explicit `project="Sigil"`

The relevant commits are:

- `010b77a` Make subagent work measurable in eval memory
- `29ada83` Close the live subagent eval loop
- `e0744ca` Make memory-server package tests deterministic
