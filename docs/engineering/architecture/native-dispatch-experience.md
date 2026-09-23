# Native dispatch experience: bounded candidate evidence

Status: implementation candidate; not deployed or accepted.

Main integration base: `ed992a36b97859fa16806706ac0e5c739de03e17`.
The nullable mirror identity/task migration occupies **v39** after the
mainline v34–v38 session/delivery/admission/CurrentTruth migrations. No
historical identity is backfilled. Current five-facade Lead/Worker discovery
remains authoritative except for the approved Lead eval addition; coordinate
and delegate do not gain the facade. The execution-surface census records
the exact observed change while its provisional budget caps remain unchanged,
pending separate owner adjudication of the resulting budget failure.

## Owner objective

> 好的，然后你把tachi修好吧。Tachi实际上就是我想要这个来实现上面我们做的这个功能。

The referenced workflow is pre-dispatch candidate evidence, task-specific prompt
advice, verified-outcome feedback, and applicability after model upgrades. The
native host chooses and runs workers. Tachi provides experience and provenance.

The owner separately approved exposing the existing evaluation facade to the
standard daily profile for a bounded native evaluation loop. This does not
authorize managed staffing, host attachment, operator replay, merge, or deploy.

## Tool and authority map

| Intent | Existing tool/action | Source or effect |
|---|---|---|
| Register a native run | `tachi_agent_eval/register` | Existing frozen mirror-eval contract |
| Record execution observations | `tachi_agent_eval/observe` | Existing observed result and identity |
| Record verified judgment | `tachi_agent_eval/adjudicate` | Existing adjudication, rubric, prompt delta |
| Inspect one record | `tachi_agent_eval/get` | Existing mirror record |
| Consult candidates before dispatch | `tachi_agent_eval/candidate_projection` | Read-only projection of eligible mirror evidence |

Standard admits only these five actions on this facade. Missing, unknown,
operator reporting/replay, and attachment actions are refused. Delegate does
not gain the evaluation facade. Existing coordinate/admin attachment policy
remains separate. Tool discovery never substitutes for the action gate, including
when a caller invokes a handler directly.

The projection accepts candidates supplied by the host, not a fixed list of
Tachi dispatch profiles. It does not write a route decision, recommendation,
policy, feedback rule, card declaration, or new mirror record. Read retry has no
side effects. Existing registration/observation/adjudication replay rules govern
the write half of the loop.

## Evidence contract

- Requested identity is a plan; observed identity is the attribution source.
  Missing observed identity cannot be filled from a requested model name.
- Model, revision, role, and harness are distinct. A profile or vendor family
  cannot silently stand in for them.
- Unknown revision is unresolved. Equality of rolling API names is not proof
  of an immutable model release. Different revisions do not inherit confirmed
  failure claims; historical advice stays visibly historical.
- Statuses are split. `adjudication_status` says whether eligible terminal,
  adjudicated, usable, non-self-evaluation rows exist (`verified`/`insufficient`).
  `compatibility_status` says how far those rows confirm the candidate
  identity: `fully_confirmed` (every one of model, harness, role, task, and
  revision explicitly confirmed on every row), `mixed`, `unresolved`
  (a declared revision with none observed — historical), `unscoped`
  (a hard-gate match with query dimensions left unscoped), or `insufficient`.
  Nothing is an unqualified "verified" compatibility; unscoped dimensions
  are labeled `unscoped`, never confirmed, and per-sample and per-candidate
  confirmed/unresolved counts accompany the whole advisories.
- The time contract is exact and two-layered. The lower bound applies only
  to the run cohort: a run's `created_at` must be an actual instant in
  `[since, generated_at)` — RFC3339 validation is authoritative for
  inclusion (the store's `julianday` preselect is instant-aware but accepts
  strings RFC3339 rejects, and applies the row cap before validation). The
  upper bound applies to every associated fact: the observation, the
  current (last-appended) adjudication, superseded advisories, and rubric
  provenance are strictly before `generated_at` and may legitimately
  predate `since`. A future current adjudication excludes the affected
  sample entirely (no fallback to an older usable event) and a future
  superseded advisory is dropped with a visible count. A fact with an
  unusable (blank or malformed) timestamp fails its bound rather than
  passing it.
- Only eligible terminal, adjudicated, usable, non-self-evaluation evidence
  contributes to the corresponding candidate summary. Failed/rejected outcomes
  remain failures, and repeating an observation does not mint extra samples.
- The persisted `next_prompt_delta` is returned as whole advisory text with
  the evaluation and adjudication provenance. It is not a mandatory instruction
  or a claim that a clause caused an outcome.
- Candidate and evidence reads have explicit bounds and omission accounting.
  A bounded or empty result does not prove absence of historical incidents.
- The common task objective, scope, acceptance standard, and repository
  authority remain with the host; advice cannot amend them implicitly.

This is distinct from the existing `route_projection` decision-policy pipeline.
Mirror observations remain off-policy quality evidence. Retired `/eval` memory
notes and proposals are not relabeled as DecisionFactLedger evidence to make an
otherwise inert policy start influencing selection.

## Acceptance

The principal discriminator is an existing-facade round trip:
`register → observe → adjudicate → candidate_projection`. A qualified prompt
delta must be returned for its candidate with provenance, while other versions,
harnesses, roles, tasks, unobserved runs and unusable/self-evaluations cannot be
misattributed. Projection must leave routing and evaluation row counts unchanged.

The standard tool-discovery regression must show the evaluation facade is
available. The action-policy regression must demonstrate both the five admitted
actions and denial of attachment, operator replay, missing and future actions.
Existing attachment and delegate denials remain tested.

Before acceptance, freeze the exact candidate, obtain an independent
different-model review, and run the repository's canonical checks. Protocol
smoke uses an isolated database and the candidate binary; a healthy installed
daemon is not evidence that this candidate is deployed.
