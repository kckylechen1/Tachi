# Model Training Eval Gate

Status: active architecture record
Updated: 2026-06-09
Related: GitHub issue #262, `sft-memory-eval-playbook.md`,
`subagent-eval-system.md`, `dispatch-policy-learning-spec.md`

This record defines the gate for any future Tachi model training, fine-tune, or
local classifier work. The current production system is the dispatch profile /
MBIT / live `/eval` policy layer. A trained model may assist that layer only
after it beats the current policy on an isolated benchmark and passes an
artifact promotion review.

## Product Boundary

Model training is not part of the production routing loop until this gate is
passed.

Allowed today:

- SFT factory exports candidate JSONL under `~/.tachi/foundry-runs/sft/`.
- `/sft/...` rows may be used as style-only prompt exemplars when explicitly
  requested by the dispatch prompt assembler.
- Fixture JSONL may be used by `tachi_agent_eval(action="aggregate",
  fixture_path=...)`.
- Live routing uses `tachi_agent_eval(action="aggregate_live")`,
  `tachi_task(action="recommend")`, `route_simulate`, and reviewed policy /
  profile-card proposals.

Not allowed by default:

- importing raw SFT rows into ordinary memory or wiki recall;
- loading SFT data into the production vector DB for normal recall;
- treating model-training datasets, checkpoints, or weights as wiki/docs;
- replacing DispatchProfile / MBIT route scoring with a trained classifier;
- applying model-driven route/profile changes without reviewed eval evidence.

## Artifact Classes

Training work must keep artifacts outside production memory/wiki/eval stores.

| Artifact | Location | Promotion default |
|---|---|---|
| Candidate SFT export | `~/.tachi/foundry-runs/sft/*.jsonl` | never auto-promote |
| Benchmark fixture | `~/.tachi/foundry-runs/model-training/<run_id>/fixtures/*.jsonl` | fixture only |
| Training dataset | `~/.tachi/foundry-runs/model-training/<run_id>/dataset/*.jsonl` | candidate only |
| Model checkpoint / LoRA | external model store or `~/.tachi/foundry-runs/model-training/<run_id>/checkpoints/` | candidate only |
| Eval report | `~/.tachi/foundry-runs/model-training/<run_id>/eval_report.json` and markdown summary | evidence only |
| Promotion proposal | `/eval/...` or route-policy proposal metadata | reviewed before apply |

Artifact paths should be run-scoped and immutable after the report is written.
If a later run changes the dataset, model, or metric calculation, it gets a new
`run_id`.

## Benchmark Target

A candidate model must be compared against the current production policy, not
against a hand-picked prompt baseline.

Baseline:

- deterministic risk classification;
- built-in DispatchProfile / MBIT fit;
- live `/eval` weighted recommendation;
- reviewed route-policy and profile-card overlays;
- `route_simulate` variants when applicable.

Candidate model outputs:

```json
{
  "task_type": "review_request",
  "risk": "high",
  "recommended_profile": "codex_55_review",
  "blocked_profiles": ["codex_53_fast"],
  "required_evidence": ["tests_run", "diff_present"],
  "confidence": 0.82,
  "reasons": ["schema migration risk", "prior eval failures on fast checker"]
}
```

The model may propose route/profile choices, risk, and evidence requirements.
It must not directly mutate route policy or profile overlays.

## Fixture Sources

Use isolated fixtures only:

- cleaned SFT-derived task/routing fixtures;
- replay fixtures from reviewed `/eval` rows;
- issue/PR lifecycle fixtures with expected profile/risk/evidence labels;
- synthetic regression cases for sensitive boundaries such as vault, MCP,
  schema migration, dispatch, safe-merge, and model-training artifacts.

Do not train or benchmark against production memory/wiki rows unless they are
copied into a run-scoped fixture with provenance and redaction reviewed.

## Minimum Metrics

Every eval report must include these metrics at the same task grain:

| Metric | Requirement |
|---|---|
| route/profile accuracy | candidate must beat or tie baseline overall and must not regress high-risk cases |
| risk classification accuracy | no false-low result on sensitive boundaries |
| evidence contract accuracy | required evidence must match or strengthen baseline |
| verification-present rate | candidate route must not lower expected verification discipline |
| human override rate | lower is better; higher blocks promotion unless explained |
| retry/failure rate | lower is better; repeated retries block promotion |
| latency and token/cost | candidate must fit the intended lane budget |
| abstention / fallback rate | candidate should fall back to baseline when confidence is low |

Promotion requires both aggregate metrics and a sampled error review. A model
that improves easy routes but weakens high-risk routes fails the gate.

## Promotion Gate

Promotion is a reviewed proposal, never an automatic write.

1. Generate a run-scoped fixture and dataset.
2. Train or evaluate the candidate outside production stores.
3. Write an eval report with baseline versus candidate metrics.
4. Record a compact `/eval` row or route-policy proposal referencing the report.
5. Human review approves or rejects the proposal.
6. `tachi_task(action="apply_proposals", confirm=true)` may apply only the
   reviewed route-policy/profile-card delta.
7. Keep rollback trivial: disable the proposal or remove the overlay; do not
   delete the baseline DispatchProfile logic.

No model checkpoint is promoted by copying it into memory, wiki, guide, docs, or
the live daemon home. Runtime model configuration must remain an explicit
operator decision.

## Release Checklist

Before merging any model-training implementation:

- [ ] The PR links to this spec and #262 or a child issue.
- [ ] Dataset and fixture paths are run-scoped under `foundry-runs`.
- [ ] Raw SFT rows remain excluded from normal recall.
- [ ] Eval reports separate fixture evidence from live `/eval` evidence.
- [ ] Baseline and candidate metrics are computed on the same fixture rows.
- [ ] High-risk boundary cases have no false-low regressions.
- [ ] Promotion writes only reviewed route-policy/profile-card overlays.
- [ ] Rollback is documented and tested.

## Current Decision

As of 2026-06-09, Tachi should not implement Qwen/LoRA/fine-tune routing in the
production loop. The next acceptable implementation is an offline benchmark
runner or dataset manifest that proves the gate above can be exercised without
contaminating production recall or routing.
