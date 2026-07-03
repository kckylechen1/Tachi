# SFT Memory and Eval Playbook

Status: partially historical — the daily SFT generation factory was retired 2026-07-03 (PR #474); the guidance below applies only to the frozen artifacts already on disk, which are no longer regenerated.
Updated: 2026-06-09

This guide defines how Tachi should use distilled SFT artifacts without pretending
the base model learned new weights. SFT is an external behavior library: it can
provide prompt exemplars, eval fixtures, and memory seeds, but it must not become
live project truth by default.

For the standalone subagent operating and evaluation contract, see
`docs/engineering/architecture/subagent-eval-system.md`.
For future LoRA/fine-tune or local classifier work, see
`docs/engineering/architecture/model-training-eval-gate.md`; model-training
artifacts are candidate evidence only until that gate is passed.

## Operating Model

Use SFT in four lanes:

| Lane | Purpose | Storage | Recall rule |
|---|---|---|---|
| Prompt exemplar | Show answer shape and verification discipline | `/sft/...` memory rows | `tachi_dispatch` may inject 1-2 compact examples |
| Memory card | One reusable rule, failure mode, command, or model behavior | `/agent/...` or `/scratch/...` after human distillation | Normal recall only after conversion |
| Wiki draft | Stable runbook or architecture rule | `/wiki/drafts/...` pending review | Human promotion required |
| Eval fixture | Score model-role routing quality | JSONL fixture or `/eval/...` production ledger | Do not mix fixture rows with live eval rows |
| Model-training artifact | Candidate dataset, checkpoint, or report | `~/.tachi/foundry-runs/model-training/<run_id>/...` | Never normal recall; promotion requires #262 gate |

Do not use raw SFT Q/A as ordinary memory. Convert it first unless the caller
explicitly asks for `scope="sft"`.

## Recall Rules

Default agent recall must exclude training rows. A row is a training seed when
any of these are true:

- `path` is `/sft` or starts with `/sft/`
- `topic` is `sft-memory`
- `source` is `sft_seed`
- `metadata.training_sample` is true

Training seeds are allowed only in explicit SFT contexts:

- `tachi_search(scope="sft", query=...)`
- `tachi_memory(action="search", scope="sft", query=...)`
- dispatch prompt exemplar injection, marked style-only

When injected into a dispatch prompt, SFT examples are answer-shape references.
They are not current facts, API contracts, file paths, or proof that the live
repo still behaves that way.

## Promotion Rules

Promote SFT content only after compression:

1. Memory card: extract one fact/rule/bug/command/case.
2. Wiki card: convert several memory cards into a stable runbook.
3. Prompt card: convert model behavior into a role-specific wrapper.
4. Eval card: convert task behavior into an input, rubric, and expected checks.

Do not promote SFT seeds automatically through graph links, REM wiki evolution,
or Foundry distillation. SFT-derived wiki entries must start as pending drafts.

Model-training datasets, checkpoints, and benchmark reports follow the same
boundary. They may reference SFT-derived examples and reviewed eval rows, but
they do not become production memory/wiki/docs and they do not change routing
policy without a reviewed proposal. The promotion gate is defined in
`model-training-eval-gate.md`.

## Eval Ledger

Use production eval rows for actual agent outcomes:

```json
{
  "agent": "glm-5.1",
  "model": "zhipuai-coding-plan/glm-5.1",
  "task_type": "execution",
  "completion_status": "completed",
  "verification_present": true,
  "cost_usd": 0.0,
  "latency_ms": 120000
}
```

When a leader uses child agents, record concise subagent evals on the completion
row instead of saving raw child transcripts:

```json
{
  "agent": "codex-leader",
  "model": "gpt-5.4",
  "task_type": "fix_request",
  "completion_status": "completed",
  "verification_present": true,
  "subagents": [
    {
      "role": "architect",
      "agent": "kimi",
      "model": "kimi-for-coding",
      "task_type": "plan_request",
      "outcome": "useful",
      "usefulness_score": 0.82,
      "verification_impact": "changed_plan",
      "verification_present": true,
      "evaluator": "leader",
      "plan_delta": "modified",
      "latency_ms": 2100,
      "retry_count": 0
    },
    {
      "role": "explore",
      "agent": "deepseek",
      "model": "deepseek-v4-flash",
      "outcome": "partial",
      "failure_mode": "missed_contract"
    }
  ]
}
```

Subagent records are for routing evidence. They should include role, provider,
model, bounded task slice, task type, outcome, latency, evaluator, usefulness,
verification impact, plan delta, retry count, and failure mode. Do not store
prompts, chain-of-thought, or uncompressed logs in memory.

Live eval records are memory-first. `tachi_complete` writes them under
`/eval/...` with `category="eval"`, and ordinary memory search should exclude
them unless the caller explicitly scopes to `/eval`. Use
`tachi_agent_eval(action="aggregate_live")` to aggregate production eval memory;
use `action="aggregate"` only for local fixture JSONL replay with
`TACHI_AGENT_EVAL_ALLOW_FIXTURE=1`.

For routing and UX workflow audits, inspect `performance_matrix` in the
`aggregate_live` response. It separates leader rows from subagent rows and
summarizes latency, token use, cost, quality, verification, retry count, human
override rate, and failures by profile/role/agent/model/task type. `telemetry`
and `perf` are aliases for the same live-memory aggregate.

Leader workflow:

1. Start with `tachi_memory(action="briefing")`.
2. Delegate bounded slices: explore maps files, architect checks design, critic
   challenges risks, verifier checks completion evidence.
3. Integrate child outputs into the final decision; the leader owns the patch.
4. Run the real verification gate.
5. Call `tachi_complete` with `subagents=[...]`.
6. Use `tachi_agent_eval(action="aggregate_live")` to update routing policy
   from live evidence. For local fixture replay, set
   `TACHI_AGENT_EVAL_ALLOW_FIXTURE=1` and call
   `tachi_agent_eval(action="aggregate", fixture_path=...)`.

Use fixture JSONL for benchmark replay. Keep fixture source explicit; never
aggregate fixture rows with live `/eval/YYYY-MM-DD/...` records unless the
report says it is a mixed benchmark.

For model-training benchmarks, compare the candidate against the current
DispatchProfile / MBIT / live-eval policy baseline on the same fixture rows.
The candidate may propose task type, risk, profile, blocked profiles, and
evidence requirements, but it must not directly mutate route policy.

Minimum role dimensions:

| Role | Primary score | Failure to watch |
|---|---|---|
| search/explore | exact files, symbols, and caveats | invented paths or broad rewrites |
| execution | small patch correctness and tests | unverified edits |
| quick | compact useful answer | over-tooling |
| critic | real risks first | rewriting instead of reviewing |
| long-context | uses all supplied evidence | loses constraints from early context |

## Current Routing Hypothesis

Treat this as an initial policy, not a final leaderboard:

| Role | Primary | Fallback |
|---|---|---|
| search/explore/librarian | DeepSeek V4 Flash | Kimi K2P6 |
| execution/worker | GLM 5.1 | DeepSeek V4 Pro |
| quick | Kimi K2P6 | GPT Spark |
| architecture/long-context challenge | Kimi K2P6 / kimi-for-coding | GPT frontier |
| critic/reviewer | DeepSeek V4 Pro | GPT frontier |
| planner/architect/final integrator | GPT frontier | none by default |

Update this table only with eval evidence: task type, model, outcome, cost,
retry count, verification, human override, and regression status.

## Safe Ingestion Checklist

Before importing SFT rows:

- Use strict/clean JSONL first; relaxed/final files are source pools only.
- Set `path` under `/sft/...`.
- Set `topic="sft-memory"` or `source="sft_seed"`.
- Set `metadata.training_sample=true`.
- Keep `auto_link` disabled or rely on server-side training seed skip.
- Do not set `tier="pattern"` for raw SFT seeds.

After import:

- Verify `tachi_search(query=..., scope="memory")` does not show SFT rows.
- Verify `tachi_search(query=..., scope="sft")` does show SFT rows.
- Verify dispatch prompts label SFT rows as style-only examples.
- Promote only compressed cards into normal memory/wiki paths.
