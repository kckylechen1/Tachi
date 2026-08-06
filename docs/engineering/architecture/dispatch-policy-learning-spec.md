# Dispatch Policy Learning

Status: active canonical spec
Updated: 2026-07-20
Related: GitHub issue #194, `docs/engineering/architecture/subagent-eval-system.md`,
`docs/engineering/architecture/sft-memory-eval-playbook.md`,
`docs/engineering/architecture/credentialed-dispatch-profiles.md`

This spec defines the product contract for Tachi's policy-learning dispatch
layer. The goal is not to build another agent harness. The goal is to make
Tachi choose the right worker profile, context, skills, and verification gate
from durable evidence.

## Product Loop

The canonical loop is:

```text
Issue or task
  -> feature flow intake
  -> feature briefing
  -> deterministic risk classification
  -> DispatchProfile and MBIT card recommendation
  -> capability and skill loadout resolution
  -> freeze required guarantees and choose an admitted execution owner
  -> host-native worker by default; qualified external assignment only when
     native candidates cannot satisfy the frozen boundary requirement
  -> leader verification
  -> live /eval completion row
  -> performance matrix aggregation
  -> route policy update or proposal
  -> release note and close-loop synthesis
```

The host harness owns ordinary worker lifecycle. Tachi owns policy advice,
frozen contracts, claims, receipts, adjudication evidence and routing
projections. Worker CLIs such as Codex, Claude Code, OpenCode, Kimi, GLM, and
DeepSeek keep their native execution loops. Tachi may own an external worker
only through an operator/admin staffing exception admitted under #749; user
preference or model choice alone is not authority.

## Public Facade Rule

Policy-learning features must stay inside the existing domain facades. The
table distinguishes implemented surfaces from planned surfaces so agents do not
invent calls that are not exposed yet.

| Capability | Public surface | Status |
|---|---|---|
| feature intake and board | `tachi_task(action="intake"|"briefing")` | implemented |
| profile/card listing | `tachi_task(action="profiles"|"profile"|"card")` | implemented |
| route recommendation | `tachi_tune(action="route_simulate")` | implemented |
| route policy replay | `tachi_tune(action="route_simulate")` | implemented |
| route policy proposals | `tachi_tune(action="route_proposals"|"route_review"|"route_apply")` (admin-only since #1426) | implemented |
| external staffing exception | `tachi_staff(action="start", staffing_reason=..., profile=...)` (the retired `tachi_task(action="dispatch")` was deleted in #1319-C2) | implemented; not ordinary delegation |
| worker board | `tachi_task(action="board")` | implemented |
| completion and eval | `tachi_task(action="complete")` / `tachi_complete` | implemented |
| performance matrix | `tachi_agent_eval(action="aggregate_live"|"perf"|"telemetry")` | implemented |
| skill bundle/loadout | `tachi_skill(action="bundle"|"loadout")` | implemented |
| PR gate preview | `tachi_gh(action="pr_status")` | implemented |
| release and closure | `tachi_gh(action="release_note")` / `tachi_task(action="close_loop")` | implemented |

Do not add new public facades such as `tachi_mbit`, `tachi_policy`, or
`tachi_router` while an existing domain facade can carry the workflow. Internal
modules may remain separate when that keeps code cohesive.

## DispatchProfile

A `DispatchProfile` is a role-level routing object. It is separate from a tool
visibility profile. It answers:

- which backend and model to use;
- what role the worker is playing;
- whether the worker may write;
- which Tachi or Hub MCP surfaces can be injected;
- which credentials may be materialized;
- which skills and passive traits shape the prompt envelope;
- which evidence the worker must return.

Built-in profiles must remain stable enough for agents to learn:

| Profile | Role | Default use |
|---|---|---|
| `claude_plan` | planner | ambiguous planning, feature breakdown |
| `glm_impl` | executor | bounded implementation with tests; resolves through the GLM coding model card |
| `opencode_builder` | executor | credentialed OpenCode implementation lane |
| `codex_55_review` | senior reviewer | high-risk review and regression detection |
| `codex_53_fast` | fast checker | low-risk quick sanity checks |
| `kimi_arch` | architect | assumption challenge and lifecycle critique |
| `deepseek_explore` | explore | cheap read-only repo mapping |

Legacy `agent` dispatch remains supported, but lifecycle agents should prefer
`profile` when a profile exists because it carries evidence and skill contracts.

## MBIT Card

MBIT means Model Behavior Identity Tag. It is the machine-usable card attached
to a dispatch profile. It is allowed to be lightweight and memorable, but it is
not cosmetic.

Each card should expose:

- display name;
- type or role tags;
- strong and weak match surfaces;
- skill loadout;
- evidence contract;
- capability bundle preference;
- evolution rules or proposal hooks when enough eval evidence exists.

MBIT data feeds route explanations, fallback chains, prompt envelope selection,
scorecard display, and future policy evolution. A card that does not affect
routing is not a valid MBIT card.

## Skill Loadout

Skill loadouts are sparse. Tachi must not attach every available skill to every
worker.

| Skill class | Meaning |
|---|---|
| common skills | reusable skills shared across profiles |
| signature skills | profile-specific prompts or workflows |
| passive traits | always-on hints that shape envelope and review |
| forbidden skills | patterns the profile must not receive |

Resolution order:

```text
DispatchProfile
  -> MBIT card
  -> skill loadout
  -> capability bundle
  -> prompt/context pack
  -> backend command
```

The resolved loadout must be visible in `recommend` output and dispatch
metadata so the leader can inspect why a worker received a given prompt shape.

## Risk Classifier

Before recommendation, Tachi classifies the task deterministically. The output
must include:

- `task_type`;
- `risk`;
- `risk_reasons`;
- required profiles;
- blocked profiles.

High-risk signals include dispatch/eval changes, vault or credential boundaries,
schema or migration paths, sandbox/MCP injection surfaces, safe-merge changes,
and matched prior eval failures. Low-risk documentation tasks may route to fast
checkers only when no sensitive file context is attached.

Risk classification should be boring and explainable. If a user passes an
explicit risk override, the override is honored but still shown in the output.

## Evidence Contract

Policy learning must not treat self-reported success as strong evidence.

Rules:

- success without tests, diff, review finding, or other evidence remains weak;
- subagent output without a leader or human evaluator remains weak;
- `human_override=true` lowers routing weight;
- high retry count lowers routing weight even when the final result is useful;
- review work should include severity or blocker structure;
- worker eval should record task type, profile, role, agent, model, latency,
  cost when available, verification impact, plan delta, and failure mode.

Raw child transcripts do not belong in memory. Persist the leader's compressed
evaluation under `/eval`.

## Live Eval and Performance Matrix

`tachi_complete` writes production eval memory under `/eval/YYYY-MM-DD/...`.
Ordinary recall excludes this evidence. `tachi_agent_eval(action="aggregate_live")`
reads the live eval rows and produces aggregate scores and the performance
matrix.

Route simulation and profile-card policy consume that matrix. Matching telemetry can:

- penalize failures;
- penalize human overrides;
- penalize repeated retries;
- penalize slow or expensive profiles;
- reward low-cost, high-quality matches when the evidence is strong enough.

When live samples are missing, route simulation should say so and fall back to
deterministic MBIT/risk fit rather than pretending the policy is learned.

## Feature Workflow UX

Every substantial policy-learning slice should be able to pass this workflow:

1. `tachi_task(action="intake", issue_ref=...)`
2. `tachi_task(action="briefing", flow_id=...)`
3. `tachi_task(action="cycle_status", flow_id=...)`
4. `tachi_task(action="profiles")` / `tachi_skill(action="loadout")` as needed
6. launch the host harness's native subagent with the frozen packet; use the
   admin/operator external staffing exception only when a proven boundary
   requirement exceeds host-native guarantees
7. ingest native accepted/started/terminal receipts into the same work ledger
8. leader verification and `tachi_verify`
9. submit evidence and independently adjudicate the outcome; legacy explicit
   Tachi runs may still use `tachi_task(action="complete", flow_id=..., dispatch_id=...)`
10. rerun `tachi_task(action="cycle_status", flow_id=...)` before PR handoff
11. `tachi_gh(action="link_pr", flow_id=..., pr_ref=...)`
12. `tachi_gh(action="pr_status", flow_id=..., pr_ref=...)`
13. `tachi_gh(action="release_note", flow_id=...)`
14. `tachi_task(action="close_loop", flow_id=...)`

The UX matrix is not just a checklist. It is a product test for whether Tachi
can guide an agent from issue to durable closure without relying on chat memory.

## Implemented Baseline

As of 2026-06-28, the baseline includes:

- feature-scoped `tachi_task(action="briefing")`;
- built-in dispatch profiles and MBIT-like profile cards;
- profile recommendation with deterministic risk classification;
- live eval performance matrix consumption by recommendation;
- read-only route simulation over recent live eval rows for `current`,
  `cost_sensitive`, and `quality_first` policy variants;
- route-policy proposal lifecycle through `tachi_tune(action="route_proposals")`,
  `review_proposal`, and `apply_proposals`, with human approval required before
  durable route-policy rules are persisted;
- sensitive file-context risk escalation;
- skill loadout fields on profiles;
- `tachi_skill(action="bundle"|"loadout")` maps worker tasks and dispatch
  profiles to sparse skill loadouts plus capability bundles;
- capability bundle auto-injection is visible in dispatch prompt artifacts and
  can be disabled per dispatch;
- `tachi_skill(action="loadout")` includes live `/eval` feedback summaries,
  sample thresholds, and guidance before any loadout evolution proposal is made;
- `tachi_tune(action="route_proposals"|"route_review"|"route_apply")` includes
  human-reviewed `loadout_evolution` proposals once profile samples,
  verification, and observed skill usage pass the evidence thresholds, then
  projects approved signature-skill, passive-trait, and evidence-contract
  changes into durable profile/card overlays;
- MBIT cards include stats plus reviewed card-risk projections for learned
  weakness markers and skill demotion targets, and merged weak-against signals
  affect route recommendation scoring;
- credentialed `opencode_builder` profile;
- feature lifecycle: `tachi_task` owns `intake` / `cycle_status` /
  `build_references` / `close_loop`; `tachi_gh` owns `link_pr` /
  `pr_status` / `release_note` / `pr_handoff`;
- feature briefing, intake instructions, issue automation plans, and GitHub
  handoff text route flow/issue/PR-backed work through read-only `cycle_status`
  before PR handoff, release notes, or close-loop;
- `dispatch(profile=...)` records flow-visible dispatch ids and compact dispatch
  card artifacts when `flow_id` is valid;
- dispatch card artifacts include the suggested completion payload, and
  `tachi_task(action="complete", flow_id=..., dispatch_id=...)` links the
  `/eval` result back into the dispatch card, flow status, and UX matrix;
- close-loop marker persistence for UX matrix completion.

## Remaining Work

The next policy-learning slices should focus on replay and richer card
projection hardening:

- harden route-policy replay and regression coverage across larger live eval
  fixtures;
- continue expanding card parsing and projection tests as new MBIT fields are
  added.

Implemented route-policy loader:

- `tachi_tune(action="route_apply")` persists approved route-policy rules
  into `dispatch_route_policy_rules`.
- `tachi_tune(action="route_simulate")` loads applied rules, requires approved
  review plus a minimum sample threshold, respects risk `blocked_profiles`, and
  explains applied/skipped rules under `route_policy_rules`.
- Applied rules add explainable weight rather than hard-overriding routing, so
  high-risk tasks can still block fast/unsafe profiles.

## Non-Goals

- Do not replace worker CLI execution loops.
- Do not store raw worker transcripts in normal memory.
- Do not create a second unmanaged live JSONL eval ledger.
- Do not force every task through a multi-agent pipeline.
- Do not let MBIT cards become flavor text disconnected from routing.
- Do not widen the public facade surface unless an existing domain facade cannot
  carry the capability.

## Closure Criteria for Issue #194

Issue #194 can close when:

- profile dispatch, MBIT cards, risk classification, skill loadouts, and route
  recommendation are available through the existing task/skill facades;
- recommendation consumes live eval performance evidence and explains fallbacks;
- dispatch writes flow-visible worker state and evidence requirements;
- at least one end-to-end feature flow proves intake, briefing, recommend,
  dispatch, board, verification, PR status, release note, and close-loop;
- route policy memory or proposals can preserve learned rules with evidence;
- route simulation or an equivalent replay surface can compare policy choices;
- tests cover the routing and evidence cases listed above.
