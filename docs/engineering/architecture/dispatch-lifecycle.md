# The Dispatch Lifecycle canon (怎么派 / 怎么回 / 怎么load card)

Status: current delivery doctrine. Revision: 2026-09-20.

Owner request: “这个仓库的规矩也得改改。然后你就都接手做了吧。”
This revision implements that request; it does not claim the owner separately approved every design choice below, waive an existing candidate's missing evidence, or authorize this change to approve itself. It becomes the repository default through the normal reviewed merge path.

This document owns delivery procedure: scope, roles, review, acceptance, and recovery. Its current rules supersede older blanket model-diversity, whole-review restart, and report-format prescriptions in repository summaries. Credential isolation, data integrity, process containment, and branch protection are not relaxed. The prior doctrine, incidents, and dated implementation census remain in [the immutable pre-revision record](https://github.com/kckylechen1/tachi/blob/817a673f45c8bcdef14c5ff8bf89d84ffc05aeba/docs/engineering/architecture/dispatch-lifecycle.md); they are not a second active delivery policy.

## 1. Thesis — why the loop exists

The objective is a correct, auditable delivery, not maximum ceremony. A worker self-report is a claim; source, independent review, and executed evidence establish what happened. Infrastructure failure and source failure are different facts, but neither proves acceptance. A real dependency or authority invariant outranks throughput; missing paperwork that duplicates an existing artifact does not.

Preserve implementation/review separation, exact candidate identity, discriminating tests, protected workspace ownership, and explicit external authority. Remove repeated tickets, unverifiable model claims, and complete restart of unaffected work.

## 2. 怎么派 (Dispatch)

### 2.1 Tiering

| Risk | Examples | Minimum delivery shape |
| --- | --- | --- |
| Mechanical | verified formatter output; non-normative prose or spelling | bounded diff, applicable checks, reviewer delta acknowledgment when following a reviewed candidate |
| Ordinary | bounded behavior change outside the high-risk domains | explicit scope, regression evidence, attributable independent review, planned acceptance |
| High | credentials, identity/authorization, privacy/egress, subprocess containment, schema/data migration, merge/release gates, normative governance | threat/invariant and caller census, independent qualified review, adversarial discriminators, final integrated acceptance and any required real canary |

A small diff is not automatically low risk. Governance that changes permission or acceptance is high risk even when written in Markdown. A syntax/import fix that changes resolution, a macro-sensitive formatter change, or a dependency update is not automatically mechanical.

### 2.2 Pre-dispatch card consult

Use existing lane cards and failure evidence when available and relevant. They are routing advice, never credentials or authority. Do not block a sole owner-facing session on an unavailable advisory card service or create a model-routing platform for one repair. Record material routing limitations once in the delivery record.

Native harness workers own ordinary local work and session lifecycle. Managed Tachi staffing remains opt-in for an explicit owner request, work that must outlive the harness, cross-device pickup, or absence of a usable native worker. Preserve the existing typed staffing reason before run artifacts or workers are created. A recommendation does not authorize launch.

### 2.3 Packet freezing — one bounded contract

An existing leaf issue, a PR body, or an owner-facing session's recorded plan can be the bounded contract. A separate leaf is useful for durable/delegated work or a distinct acceptance boundary, not mandatory for every formatter correction. Do not duplicate one repair across several tracking objects.

Before final review, record:

- the owner request or issue reference, outcome, non-goals, and actual base/head;
- the relevant invariant, callers/entry routes, legacy parameters, and allowed/refused peers;
- affected tests, platforms, dependency/migration order, independent reviewer route, and required acceptance items;
- worktree/branch ownership, target-directory policy, and external actions authorized separately.

The frozen assertions stay intact. New guards must state their invariant, persistent-failure behavior, and safe-progress counterpart. Consolidation starts with the old input equivalence classes, including empty/missing/zero. Payload trimming first identifies retained evidence fields and keeps content atomic. Resident daemons retain their protected operating windows. Do not route around a red result by changing its scope after seeing it; a necessary scope change is a visible adjudicated amendment.

Native issue parentage and protected closure remain owned by issue-portfolio-governance. Typed spec/claim receipts, when present, remain authoritative; a PR description cannot override them. Do not fabricate a packet id or receipt for an unmanaged sole session.

### 2.4 Lane selection and review diversity

**Independence is required; unverifiable branding is not evidence.** A reviewer must not have implemented the reviewed slice, must use a separate read-only context, and must inspect the actual candidate rather than repeat the implementation summary. Record the reviewer/harness or accountable human, review invocation/reference, candidate head, numbered findings, dispositions, and unexamined surfaces. A new persona, self-authored approval, or an unattributed “Oracle said safe” is not independent review.

For ordinary changes, a distinct model is preferred. A separate attributable read-only reviewer may qualify even if the carrier hides its underlying model; record `model: unknown` and never call the review certified different-model. Lack of an exact vendor model string alone must not erase actual independent evidence.

For high-risk changes, require either a verifiably different-model qualified reviewer or an accountable independent human review. If the implementer/reviewer model pair cannot establish diversity, the human route resolves the limitation; an anonymous second thread does not. Missing qualifying review is `review_blocked`, not a source BUG and not approval. Cross-vendor review is optional unless explicitly frozen for the delivery.

The implementing session may coordinate issues, tests, and evidence, but cannot count its own second pass as independent review. It may relay a merge already authorized by an independent adjudicator only when the unchanged final candidate satisfies all applicable gates. No automatic self-merge or policy self-exemption is introduced.

### 2.5 Candidate freeze and maintainability budget

1. Implement and run narrow discriminators until the candidate is stable. Perform readiness/fmt checks early, not only after a full review.
2. Resolve the intended base and dependency order before final acceptance. Freeze one final candidate at a time per shared integration/runner bottleneck; unrelated read-only work need not stop.
3. Obtain whole-candidate independent review, then settle findings with a separate writer.
4. A changed head always needs a current review binding, but not necessarily a new whole-candidate review. A mechanical delta may receive an independent addendum referencing the old verdict, exact old/new heads, inspected diff, unchanged invariants, and any rerun checks. Uncertain semantics require normal review.
5. A semantic repair or relevant base change reopens the affected invariant/callers/tests and its dependency neighborhood. A schema, credential, permission, containment, or gate change is never dismissed as mechanical. Conflict-free rebase alone is not equivalence proof.
6. Execute the final plan. Test evidence remains attributed to the SHA/tree, toolchain, platform, command, and inputs actually exercised. Reuse requires a documented equivalence check and reviewer acknowledgment; never relabel a receipt. Tests, fixtures, lockfiles, build scripts, generated code, embedded commit identity, or environment changes can invalidate reuse even when business-source blobs are unchanged.

More than 20 files, 2,000 changed production lines, or five new public types triggers a scope/vocabulary review, not an invented bureaucratic queue. Split by independently reviewable invariants unless that breaks an atomic migration/security/wire contract. Count production, tests, goldens, generated files, migrations, and deletions separately.

A new abstraction needs real consumers or a named external/security boundary. A proof artifact must discriminate a production decision or a concrete regression; do not create competing registries or duplicated authority. Large modules should separate coherent invariants physically without inventing another service layer. Source comments explain current mechanisms, not review-round anecdotes.

### 2.6 Acceptance plan and result semantics

The plan is frozen before final evidence collection. Enumerate required checks and platform/matrix coverage; an item is optional or inapplicable only for an explicit, reviewable reason. Existing branch-protection requirements still apply. Candidate changes to the plan or evaluator receive high-risk review and cannot silently authorize themselves.

| Evidence state | Meaning | Satisfies an applicable required item? |
| --- | --- | --- |
| `passed` | executed on the identified candidate/context and passed | yes |
| `candidate_failed` | attributable implementation/check failure | no |
| `baseline_blocked` | inherited defect/advisory blocks the candidate | no |
| `infra_blocked` | independently evidenced runner/service/billing failure | no |
| `pending` / `not_run` | unfinished or absent execution | no |
| `not_applicable` | planned, justified exclusion, not a failed run renamed afterward | no execution owed for that item |
| `stale` | evidence no longer matches the candidate/context | no |
| `unknown` | insufficient information to classify | no |

Keep raw CI status/conclusion and diagnostic cause separate. Zero steps alone does not prove billing. A failure does not prove the source is broken. Preserve both facts when cleanup/collection also fails. Never turn `neutral`, `skipped`, missing results, or “some jobs passed” into proof that every required item passed.

`ci.yml` remains the executable baseline for automated checks. `.github/acceptance-plan.json` identifies its required job families; matrix definitions remain in the workflow. The final `acceptance` job aggregates only those automated CI obligations. It is **not** an independent-review, authenticated-canary, owner-approval, or deployment receipt. Keep existing required checks until a separately reviewed ruleset change makes the aggregate authoritative; this policy does not change GitHub settings.

Independent checks collect evidence even after another independent check fails, but must honor successful setup, cancellation, and their real prerequisites. Do not use `continue-on-error`, `|| true`, blanket ignores, or advisory suppression to obtain green. A missing report after a successful test run is a failure; a report never produced because tests did not start is not a second test failure.

### 2.7 Recovery and migration order

Provision a usable execution seat before launching implementation fleets. Setup remains read-only and fails clearly when its pinned tools are absent; environment provisioning is separate, reproducible, and owner-authorized, never ad-hoc mutable bootstrap inside a task setup hook.

Record the blocked item and use an admitted alternative route where one exists. An unavailable canary or platform holds its affected delivery, not all independent repairs. Do not automatically change billing, broaden runner trust, bypass required checks, or weaken a security fix to unblock throughput.

Allocate schema versions against the live intended base, not parallel remembered bases. Concurrent deliveries cannot independently publish distinct inventories under the same schema version. The later delivery rebases/resequences before freeze and tests fresh initialization, sequential upgrade from the earlier delivery, reopen validation, portable scope, and failure/rollback behavior. Never downgrade or rewrite a live schema stamp to simulate success.

## 3. 怎么回 (Return)

A return records outcome, base/head and tested tree, changed scope, actual commands or linked official run/artifact, test counts/results, review findings/dispositions, and remaining gaps. A linked authentic result need not be copied verbatim into every chat, issue, and PR. Structural inspection, simulation, compilation, runtime tests, and real authenticated canaries are distinct evidence classes.

A delivery may be implemented but acceptance-blocked. That is useful progress, not a reason to fabricate green or re-run anonymous reviews. Distinguish the completed work from the blocker and record the exact next acceptance action. Do not promise unattended progress when no live executor is running.

Mechanical findings receive a bounded writer patch and review addendum. Clear prescriptions go to the writer without rewriting the whole spec. Invariant conflicts and fix-versus-revert decisions go to the adjudicator. Reviewers remain read-only and must not fix then approve their own change.

The authorized adjudicator reads the final diff and evidence before merge. Acceptance does not itself authorize deployment, credential changes, destructive cleanup, or protected issue closure. Use `Refs`/`Related` for protected umbrellas. Keep uncommitted foreign lanes untouched.

## 4. 怎么load card (Cards)

Existing cards describe capability, failure modes, constraint carriers, and observed efficacy. Model identity and evidence uncertainty stay explicit. Cards advise routing; they never mint execution or merge authority.

The established split is unchanged: reviewed narrative declarations, append-only execution/evaluation evidence, and reproducible derived projections. Tachi mirrors governed declarations read-only; model output proposes changes rather than approving itself. Static runtime/profile configuration does not become a parallel card authority.

Use the existing domain surfaces and host-native worker mechanisms. Do not create a new router, ledger, scheduler, or identity-certification service for this delivery-policy revision. Detailed historical card mechanisms and incident evidence remain in the immutable prior record and their existing owning documents.

## 5. The closed loop

Scope → bounded implementation → discriminating evidence → independent review → final acceptance → authorized integration → observed outcome.

Failures and blocked attempts retain provenance. They inform later routing and regression tests; they do not become current truth, approval, or automatically executable instructions.

## 6. Current-state map & gap list

Read live source, refs, checks, and issue dispositions before acting. The old implementation census is a dated historical record, not a claim about today's tree. This revision changes procedure and automated evidence collection, not runtime model identity, WorkClaim authority, provider adapters, or deployed state.

## 7. Authority boundaries

No input from a model, provider, persona, card, memory, issue comment, or PR summary grants secrets, network/filesystem permissions, process cleanup authority, or merge approval. Executed evidence is required at the boundary it claims to prove. A new policy is subject to the same independent review as the gate it changes.

## 8. Lessons retained

Test read/write asymmetry, exact identity, current authority at mutation time, lifecycle ownership, and uncertainty preservation. Preserve data/content atomicity and safe-progress peers. Prefer one source of truth, bounded concurrency, and observable completion over additional ceremony or parallel control planes.
