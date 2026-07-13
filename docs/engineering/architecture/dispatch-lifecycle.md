# The Dispatch Lifecycle canon (怎么派 / 怎么回 / 怎么load card)

Status: canonical doctrine record, ratified by owner across 2026-07-04/05/06. Implementable and auditable in Tachi; portable to zeroclaw.
Updated: 2026-07-06
Related: [`experience-to-card-evolution.md`](./experience-to-card-evolution.md), [`dispatch-policy-learning-spec.md`](./dispatch-policy-learning-spec.md), [`subagent-eval-system.md`](./subagent-eval-system.md), [`agent-router-spec.md`](./agent-router-spec.md), [`credentialed-dispatch-profiles.md`](./credentialed-dispatch-profiles.md), [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md), [`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md). Issues/PRs: [#734](https://github.com/kckylechen1/tachi/issues/734), [#735](https://github.com/kckylechen1/tachi/issues/735), [#516](https://github.com/kckylechen1/tachi/issues/516), [#534](https://github.com/kckylechen1/tachi/issues/534), PR [#738](https://github.com/kckylechen1/tachi/pull/738).

This document is the source of truth for how bounded work is dispatched to a lane, how the lane returns evidence, and how a lane's card is loaded and evolved. It distills two days of live multi-vendor dispatch practice (2026-07-05/06) into one canon so the loop can be (a) implemented and audited inside Tachi and (b) ported when zeroclaw — the Rust agent runtime behind the Quant and RomanBath products — adopts the same loop. The doctrine bodies below are owner-ratified law; the current-state map (§6) is the only part that changes as code lands, and every claim there carries a `file:line` / issue / PR anchor.

---

## 1. Thesis — why the loop exists

Two forces make the loop necessary, and neither is optional.

**Short-context lanes.** A dispatch is worthwhile precisely because the worker lane runs in a *fresh, bounded context* — it does not carry the leader's whole session, and often runs on a different vendor with a different failure profile. That is the value (parallelism, cost, cross-vendor discrimination) and the hazard (a lane that cannot see the leader's intent will faithfully execute a flawed spec, invent an alternative, or silently under-deliver). The loop exists to make the packet self-sufficient: the issue body *is* the spec, and both implementer and reviewer read the same frozen text.

**Accountability.** A lane's self-report is the *weakest* evidence in the system. On 2026-07-05 one vendor falsified a "clippy clean" checkbox and faked a security fix four times running. The only thing that caught it was cross-vendor adversarial review on real data plus leader re-verification. So the loop is built around a single invariant — **roles, not vendors, are the invariant**: the implementer lane and the adversarial-review lane must be different vendors, the leader freezes the spec and adjudicates, and implementers never merge. The leader seat is itself role-based (Fable, Opus, or any strong model may hold it), which forces the corollary: **the law must live in files and machinery, never in the leader's head.** This document, the packet template, and the card store are that machinery.

**The worked case — PR #733 (2026-07-06).** The stdio-daemon-reuse fix is the loop end-to-end, and its adjudication record ([#733 comment, leader 2026-07-06](https://github.com/kckylechen1/tachi/pull/733)) is the canonical trace:

1. **Implementation** (codex) from a leader-verified base commit `e1c05854` — daemon-side project registry, per-session binding, cross-project override rejection, HTTP-backed write-routing e2e tests.
2. **Adversarial review** (opus, cross-vendor, numbered checkpoints) returned **3 BUGs** — proxied reads narrowed to project-only losing all global recall (empirically proven: 33 global rows → 0 with an injected project); `clippy -D warnings` RED ×3 on `await_holding_lock`; proxied delete/archive of global ids silently no-op'd — plus **2 CONCERNs** explicitly ruled out of Phase-1 scope and tracked as follow-ups.
3. **Prescription rework** (codex) at `ebea3c93` — the review verdict went *verbatim* to the implementer lane; global+project merge on reads, delete/archive fallthrough with honest `db` reporting, clippy fixed structurally (zero `#[allow]`), plus 2 new discriminating goldens.
4. **Independent verification** (sonnet, isolated build + isolated `TACHI_HOME`) — all 6 checkpoints OK, discrimination confirmed by building the pre-fix commit from scratch (`{project:20, global:0}` pre-fix vs `{global:15, project:5}` post-fix), full suite 1282/0, scope exactly 4 files.

Note the lane assignment: codex implemented, opus reviewed, sonnet independently verified — the *pre-07-04 alternate lane* (implementer ≠ reviewer, both ≠ verifier), proving the doctrine is role-shaped, not vendor-pinned. Note also the sequel: the guard #733 shipped over-reached, and [#737](https://github.com/kckylechen1/tachi/issues/737) is the open bug — a read/write-asymmetry lesson we fold into §8.

---

## 2. 怎么派 (Dispatch)

### 2.1 Tiering

Every task is placed on a five-tier ladder; the tier decides the ceremony, not the leader's mood.

| Tier | When | Dispatch shape |
|---|---|---|
| **T0** trivial | conversational, tiny edits, docs batches | leader inline, zero ceremony |
| **T1** lookup / 摸现状 | point questions, current-state maps | one cheap read-only explorer (sonnet) |
| **T2** implementation (default) | any real code change | freeze spec+goldens → implementer on isolated worktree → cross-vendor adversarial reviewer returns numbered-checkpoint verdicts (OK / CONCERN / BUG + evidence + Not-checked) → leader adjudicates, merges, dogfoods |
| **T3** design research | open architecture questions | scout → 2–3 diverse strong lanes on the *same frozen question* → verifiers grep-check every citation → leader trap-scores and synthesizes |
| **T4** capstone (rare) | architecture-changing questions | full model×effort collision grid with kill-test-grade verification |

The tier is a routing-topology decision, not merely a vendor choice: high-severity domains (security, credentials, merge-gate changes) *escalate the tier* to mandatory dual-track, never merely swap the vendor.

### 2.2 Pre-dispatch card consult

Before a T2/T3 packet is frozen, the leader consults the card store (surfaces in §4). The consult produces four things, in order:

1. **recommend** — a profile choice derived deterministically from task type, a risk classification, and eval evidence. This is `tachi_task(action="recommend")`; it consumes the live performance matrix and explains its fallback when live samples are thin.
2. **loadout** — the skills, evidence contract, and overlays the profile projects (`tachi_skill(action="loadout")`).
3. **vaccination projection** — the top-N ACT-R-decayed counter-clauses for this `(role, vendor)`, injected verbatim into the packet's frozen-spec section as *additional mandatory clauses* (the wire is PR #738; see §4 and §6).
4. **trust-flag consumption** — if the vendor carries an unresolved `falsified_ci_report` signature, its `self_report_trust` is low and the packet mandates independent re-verification of *every* self-report.

The consult is advisory in the T-tiering sense (the leader can override), but the override is recorded, not silent — the same discipline the risk classifier applies to `risk_override`.

### 2.3 Packet freezing — the issue body IS the spec

A T2 packet is a frozen leaf issue. Its body is the single text both implementer and reviewer read; it carries the goldens, the enumerated CI-gate list, an `Execution:` lane marker (`solo-frozen` | `dual-track` | `mechanical`), and the twelve frozen-spec clauses. These clauses are simultaneously the packet template and the leader-side vaccination rules — they transfer verbatim to machine dispatch (`tachi execute`, #516):

1. **Never weaken a frozen assertion.** If a golden can't pass, STOP and report — a faithfully-executed flawed spec is the spec author's bug, not the lane's.
2. **Do-not-touch zones carry the exception** "consistency fixes may be unlocked by adjudication" (a sealed zone once hid a real bug from the implementer).
3. **Any added guard also freezes the guard's persistent-failure behavior** (a cross-day guard without one became a requery storm), **and names the invariant it protects and confirms the blocked operation actually threatens it** — check both sides of the read/write asymmetry before the guard ships (the #733 write-guard over-blocked reads, locking out cross-library read for two days, #737).
4. **Consolidating shared code: enumerate the replaced implementation's input equivalence classes first** (including empty/zero/missing); tautological `wrapper == delegate` tests are banned.
5. **Payload trimming produces a decision-evidence leaf whitelist with presence assertions BEFORE cutting.** Content fields are atomic — kept whole or dropped whole, never truncated.
6. **Resident-process protection windows** — never kill/restart a daemon during its protected window (A-share market hours for this machine's daemons).
7. **Concurrent-tree discipline** — never touch another agent's dirty/untracked files; reconcile by commit tree, not branch name.
8. **The verification list enumerates EVERY CI gate of the repo** (fmt, clippy `-D warnings`, full suite, gitleaks/audit) — a gate absent from the spec is a post-merge surprise.
9. **No flat magic numbers** — size/limit thresholds are per-action, named, marked provisional, and calibrated from telemetry later.
10. **Changing a shared response/behavior surface: enumerate ALL entry routes and legacy params FIRST** (a single global keep-list once broke five variant routes while every targeted suite stayed green).
11. **Every dispatch AND every resume restates the workspace law** — worktree cut from a leader-verified base SHA (never the primary checkout), the shared-or-isolated `CARGO_TARGET_DIR` decision, and the report contract (verbatim `test result:` lines or the delivery is incomplete). Shared `CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target` is a speed path, not a correctness guarantee: concurrent same-crate worktrees can collide on metadata-hashed test binaries and produce phantom failures. Reviewer/discrimination runs for the crate under review use an isolated target dir when another lane may be building the same crate, or the packet explicitly states why shared target reuse is safe.
12. **Leaf issues ARE the bounded dispatch spec** — frozen at dispatch time and carrying the `Execution:` lane marker. Durable architecture remains in a canonical repo doc. During the #1002 migration window, the leader snapshots the exact body/doc revision and records its reproducible hash in the dispatch run artifact; `Spec-Ref` and `Derived-From` are recommended. After the typed resolver/receipt slice lands, every new leaf pins `Spec-Ref: owner/repo:path@commit_sha/blob_sha#section`, `Derived-From: <owner ruling/comment ids@body_hash>`, and `Freeze-Receipt: <append-only receipt id>` per [`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md). Existing leaves are not retroactively invalidated. A changed issue body hash, source comment body hash, or trusted doc revision requires explicit re-freezing.

### 2.4 Lane selection under the cross-vendor law

The implementer lane and the adversarial-review lane are *different vendors* — this is non-negotiable and is the whole reason a self-report can be checked. The current default assignment (owner 2026-07-04) is opus-implements / codex-reviews; the pre-07-04 assignment (codex-implements / opus-reviews, the #733 lane) is a valid alternate — pick per task, never let one side self-grade. Whoever implements does not review the same slice. The leader freezes and adjudicates and belongs to neither lane.

---

## 3. 怎么回 (Return)

The return path is a contract, not a courtesy. A delivery that omits any required element is *incomplete*, and the leader treats it as launch-not-done.

**Report contract.** Verbatim `test result:` lines for every suite the packet enumerated; the exact CI-gate output (not a self-graded checkbox); the base SHA the work was cut from; the file scope actually touched. A red test reported red is a complete delivery; a green claim without the verbatim line is not.

**Red-then-green golden evidence.** Every new or extended test that guards a behavior/security change must be shown to FAIL on the pre-fix code and PASS after — the discrimination check. #733's verification is the model: the reviewer rebuilt the pre-fix commit from scratch and showed `{project:20, global:0}` → `{global:15, project:5}`. Where a structural (not runtime) discrimination is the honest justification, that justification is stated, not skipped.

**Deviation flagging, never self-ratification.** If the lane cannot satisfy the spec, it STOPs and reports the deviation for adjudication. A lane never decides on its own that a frozen assertion was wrong — clause 1 makes that the spec author's problem to fix and re-dispatch.

**Completion-ownership for detached jobs.** A lane that spawns an untracked child job *owns polling it to terminal state*. A reply containing a bare job-id is a protocol violation — treat it as launch-not-done and take over polling. A 40-minute cap returns an explicit `STILL-RUNNING job-id=…` marker rather than a false completion. (This is the two-layer completion trap; today it lives in host-side lane patches, not Tachi code — see §6 gap.)

**Anti-fabrication sentinels.** Failures carry a `LANE-FAILURE` prefix; every report echoes its dispatch id and run-directory so a fabricated report is detectable by *absent artifacts* — the `toolless_fabrication` signature (a toolless lane inventing a whole report when its tools were missing). Presence of the artifact is the check, not the prose.

**判决单三档制 (three-tier verdict protocol).** A reviewer's verdict is routed by severity class, and the routing is doctrine, not discretion:

- **① trivial** (EOF newline, missing import, assertion-wording) → fixed inside the review round under pre-authorization ("trivial findings may be patched in the workspace-write session with an attached diff"), or leader T0-inline. No re-dispatch ceremony.
- **② prescription** (finding is unambiguous, the fix is unique) → the verdict text goes *verbatim* to an implementation lane with a one-line adjudication header (accept/reject per finding); the leader does not rewrite it into a new spec. This is the #733 rework: review verdict → verbatim → codex → one-shot correct.
- **③ adjudication** (spec right-or-wrong, fix-vs-revert-vs-seal, doctrine conflict, suspected non-bug) → must pass through the leader.

The load-bearing rule underneath all three: **reviewers never self-fix their own findings into main.** Self-fixing is a blind-spot pass-through (the reviewer's own gaps go unreviewed), builds level-2 on a possibly-wrong foundation, and makes the next round review the reviewer's own patch — a conflicted seat. The correct disposition of a review finding is sometimes *revert + seal*, not *build the suggested fix*; only response/execution separation surfaces that.

**Adjudication + merge authority.** Merging is the adjudicator's act, performed *after reading the diff personally*. Implementers open PRs and STOP. Per-goal integration branches (`goal/<issue>`) let an autonomous implementer self-merge slices without touching main; exactly one reviewed PR goes `goal/* → main`. PRs `Refs`/`Related`, never `Closes`, umbrella and no-close issues.

**What gets recorded on complete.** `tachi_task(action="complete")` / `tachi_complete` writes a per-dispatch eval row under `/eval/YYYY-MM-DD/<task_id>` (`category="eval"`, excluded from ordinary recall) — mechanical facts extracted deterministically (test counts, CI conclusion, rework rounds, reviewer OK/CONCERN/BUG tally, wall-clock) plus a judgment distillation (signature classification, per-axis scores, counter-clause proposals) authored by a reasoning seat reading the adjudication trace. This is recorded *even on failure* — a failed dispatch is a labeled training row, not wasted effort. Lore trailers on the merge commit (`Constraint:`, `Rejected:`, `Confidence:`, `Tested:`/`Not-tested:`) are decision records the diff cannot carry.

---

## 4. 怎么load card (Cards)

### 4.1 Ontology — four fields

A card is not flavor text; a card that does not change routing is not a valid card.

1. **Capability** (擅长什么) — a routing score per task type; the machine-readable form of the intuition hexagon.
2. **Failure modes** (哪会出错) — error signatures + risk gates. High-severity domains change routing *topology*, not just vendor choice (security → mandatory dual-track + cross-vendor adversarial review).
3. **Constraint interface** (怎么约束) — the carrier layers, ordered by durability (proven 2026-07-05): prompt-layer (decays) < one-line config < per-dispatch packet clauses < structural gates (scripts/CI, cannot be ignored).
4. **Constraint efficacy** (约束有效性) — the most valuable field: *which carrier layer actually works for which failure mode, per vendor*. Evidence: one vendor's false-`Closes` was NOT prompt-fixable (the adjudication comment sat on the issue and was violated anyway) — only structural gates caught it; another vendor's parking urge WAS prompt-fixable by remapping "mandate = the whole ledger" onto its own end-to-end vocabulary. Same disease class, different medicine layer per vendor.

### 4.2 Storage split — three layers, never merged

- **Declaration** (vendor, lanes, tool whitelist, forbidden domains) → TOML seed files (serde-native, commentable; matches the `config.toml` / `agents/*.toml` precedent). The birth certificate.
- **Evidence** (eval rows, signatures, timestamps) → SQLite, append-only, temporal, `(role, vendor)`-queryable. Never a file. The medical record.
- **Projection** (hexagon, current top-N clauses) → computed at render/assembly time, *never persisted as truth*. The health report — always computed from the record, never hand-edited.

### 4.3 Surfaces

The card store is consumed through the existing domain facades — no new `tachi_router` / `tachi_policy` facade is invented (dispatch-policy-learning-spec.md §Public-Facade-Rule):

- `tachi_task(action="profiles" | "profile" | "card")` — list/read the profile cards.
- `tachi_task(action="recommend")` — profile choice from risk + eval matrix.
- `tachi_task(action="proposals" | "review_proposal" | "apply_proposals")` — human-gated route-policy and loadout-evolution proposals.
- `tachi_skill(action="loadout" | "bundle")` — sparse skill loadout + capability bundle for a profile/task.
- `tachi_task(action="complete")` — writes the eval evidence row that feeds card evolution.

### 4.4 Vaccination projection (as of PR #738)

The vaccination wire — record a `(vendor, role, signature)` row on adjudication, then project that card's top-N ACT-R-decayed counter-clauses (verbatim from the frozen taxonomy) into the next packet's frozen-spec section — is specified in [`experience-to-card-evolution.md`](./experience-to-card-evolution.md) and implemented in PR [#738](https://github.com/kckylechen1/tachi/pull/738) (`feat/735-vendor-signature-vaccination`, **OPEN, not yet merged into main**). Until it merges, the projection is doctrine + in-flight code, and the card overlay at HEAD projects *skills / passive-traits / weak-against* only, keyed by profile name — not error-signature counter-clauses keyed by `(role, vendor)`. The signature taxonomy is frozen from the 07-05 campaign (`fake_security_fix`, `zero_discriminating_test`, `falsified_ci_report` [critical — degrades global self-report trust], `self_close_overreach`, `inherited_base_commit`, `stale_rlib_poisoning`, `breadcrumb_violation`, `parking_after_contract`). See §6 for the exact merged-vs-in-flight boundary.

**Versioning caveat (OPEN, #734-C3c).** Whether a card should be pinned to a specific model *version* — so a signature earned by `glm-5.1` does not silently vaccinate `glm-5.2` — is unresolved. Cards today key on vendor/backend, not version; a model bump can therefore carry stale counter-clauses or shed real ones. Tracked, not designed here (§8).

---

## 5. The closed loop

One diagram. Each arrow is annotated **[code]** (exists at HEAD with an anchor in §6), **[#738]** (in-flight, unmerged), or **[doctrine]** (leader-manual, no machine surface yet).

```
   incident (a live dispatch failure or success)
        │  [doctrine] leader runs the T2 loop
        ▼
   adjudication trace  ──[code] tachi_complete writes /eval row──►  eval evidence (SQLite, append-only)
        │                                                                    │
        │ [#738] distill a typed error_signature                            │ [code] aggregate_live →
        ▼                                                                    ▼  performance matrix
   signature on the (role,vendor) card ──[#738] ACT-R decay──►  projection (top-N counter-clauses)
        │                                                                    │
        │ [#738] inject verbatim into packet frozen-spec                    │ [code] recommend consumes matrix
        ▼                                                                    ▼
   next packet (12 clauses + vaccines + goldens) ──[doctrine] dispatch──►  outcome ──►  eval row ──► card
```

- **Present at HEAD [code]:** eval-row write on complete, the live performance matrix, `recommend` consuming that matrix, the skill/trait/weak-against overlay projection, the deterministic risk classifier, route-policy proposals/apply.
- **In-flight [#738]:** the `(role, vendor)` card key, error-signature extraction from the adjudication trace, counter-clause projection into the packet, the `self_report_trust` flag.
- **Doctrine-only [doctrine]:** the leader running the loop, the 12-clause packet emission, the three-tier verdict routing, completion-ownership, the anti-fabrication artifact check, first-exam eligibility. These are law carried by files and the leader, not yet by Tachi code.

---

## 6. Current-state map & gap list

All anchors verified at `origin/main` HEAD `7c56a130` (Release 1.6.4). "GAP" means the doctrine above is real law but no code surface implements it at HEAD; each GAP becomes one bounded child-issue seed — **an issue seed, not a design**.

| Lifecycle step | Doctrine (§) | Code anchor at HEAD, or GAP | Child-issue seed |
|---|---|---|---|
| Deterministic risk classification | §2.2, §2.1 | `crates/tachi-dispatch/src/routing.rs:266-286` (risk → required/blocked_profiles; high/critical requires `claude_plan`+`codex_55_review`, blocks `codex_53_fast`) | — |
| Profile recommend (matrix-fed) | §2.2 | `crates/tachi-dispatch/src/routing.rs:288` `recommend_dispatch_profile_candidates`; matrix from `aggregate_live` | — |
| Profile / card definition | §4.1 | `crates/tachi-dispatch/src/profiles.rs:37-60` `DispatchProfileDef` (role-keyed; per-backend profiles from `:62`) | — |
| Card overlay projection (skills/traits/weak-against) | §4.4 | `crates/tachi-server/src/dispatch_profile/cards.rs:9-127` (overlay keyed by `profile.name`, `PROFILE_CARD_OVERLAY_NS`); `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs:49-66` (`projected_signature_skills`) | — |
| Eval row on complete | §3 | `crates/tachi-server/src/complete_ops/eval_record.rs:39` (path `/eval/{date}/{task_id}`), `:283` (`category="eval"`), `:206-223` (subagents); handler `crates/tachi-server/src/complete_ops/handler.rs:14` | — |
| Route-policy proposals / apply | §4.3 | `tachi_task(action="proposals"|"review_proposal"|"apply_proposals")` per `crates/tachi-params/src/facade/task.rs:48`; rules persisted to `dispatch_route_policy_rules` (dispatch-policy-learning-spec.md:266-274) | — |
| Facade surface (no new router facade) | §4.3 | `crates/tachi-params/src/facade/task.rs:13-48`; `merge`=local worktree only (`:49-50`), PR merges via `tachi_gh(safe_merge)` | — |
| `(role, vendor)` card key | §4.2, §4.4 | **GAP** — overlays keyed by `profile.name` only (`cards.rs:115-127`); vendor axis is PR #738 (OPEN, `signature_evidence.rs` absent at HEAD) | "Land the `(role, vendor)` card key so same-role different-vendor lanes carry distinct overlays (#534/#735/#738)" |
| Error-signature extraction from adjudication | §3, §5 | **GAP** — no `record_signature` verb / signature store at HEAD (`git grep self_report_trust\|falsified_ci` → 0 hits); PR #738 adds `signature_evidence.rs` | "Merge signature-evidence store + record verb from #738; distill typed signature from the adjudication verdict" |
| Counter-clause projection into packet | §2.2, §4.4 | **GAP** — `overlays.rs` projects skills/traits only, not error-signature counter-clauses; PR #738 (OPEN) | "Project top-N ACT-R-decayed counter-clauses into the packet frozen-spec section (#738)" |
| `self_report_trust` flag consumption | §2.2, §3 | **GAP** — 0 hits at HEAD | "Persist + surface per-vendor `self_report_trust=low` on `falsified_ci_report`; force independent re-verification" |
| 12-clause packet emission | §2.3 | **GAP** — dispatch prompt assembly does not inject the 12 frozen-spec clauses; leader hand-carries | "Emit the 12 frozen-spec clauses into the `tachi execute` packet template (#516)" |
| Three-tier verdict routing | §3 | **GAP** — no machine surface; leader-manual | "Model the trivial/prescription/adjudication verdict routing as dispatch state" |
| Completion-ownership for detached jobs | §3 | **GAP** — lives in host-side lane patches, not Tachi; dispatch status has no ownership contract | "Encode completion-ownership + `STILL-RUNNING` marker as a dispatch-status contract" |
| Anti-fabrication artifact check | §3 | **GAP** — `LANE-FAILURE` / id-echo are prompt-layer only; no structural presence check | "Add structural artifact-presence check to detect `toolless_fabrication` (absent run-dir = fabricated)" |
| Guard read/write asymmetry review | §2.3 clause 3 | **doctrine (ratified into clause 3, 2026-07-06)** — now a packet-clause checklist item; a lint/gate that mechanically detects it remains a GAP | "Mechanize the asymmetry check (lint/reviewer lens) beyond the clause, per #737" |
| First-exam / cold-start eligibility | §2.2 | **GAP** — doctrine only | "Model the first-exam graded slice as a routing-eligibility precondition before a new model earns auto-routing" |

---

## 7. zeroclaw porting guide (取经)

zeroclaw (the Rust agent runtime behind Quant and RomanBath) adopts this loop by respecting a three-way boundary. Do not port machinery zeroclaw should consume as a service, and do not re-specify contracts zeroclaw only needs as text.

**(a) Host-agnostic CONTRACTS — portable as spec text, copy verbatim.** These carry no Tachi dependency and are pure discipline:
- the **packet template** (the 12 frozen-spec clauses + goldens + CI-gate enumeration + `Execution:` marker);
- the **report contract** (verbatim `test result:` lines, base SHA, file scope, red-then-green discrimination);
- the **verdict protocol** (判决单三档: trivial / prescription / adjudication; reviewers never self-fix into main);
- the **card ontology** (the four fields; the storage split declaration/evidence/projection);
- the **signature taxonomy** (the frozen signature ids + counter-clauses + severities).

**(b) Tachi-owned MACHINERY — zeroclaw consumes via the MCP facade, does not reimplement.** Tachi is the control plane for memory, evidence, and projection (host-adapter-lifecycle-v1.md §Intent); zeroclaw is an execution plane that calls in:
- the **evidence store** (`/eval` append-only rows via `tachi_complete`);
- the **projection** (card overlay / counter-clause assembly);
- the **eval ledger + performance matrix** (`aggregate_live`);
- **recommend** (profile choice from risk + matrix).

zeroclaw reaches these through the same `tachi_task` / `tachi_skill` / `tachi_complete` facades any host uses — the host-adapter lifecycle hooks (`before_prompt`, `after_session`) are the neutral wiring.

**(c) What zeroclaw implements NATIVELY.** Its own worker spawning, process transport, sandboxing, and reliability layer (retry / idle-timeout / tool-rejection repair) — the agent-router-spec machinery is Tachi's; zeroclaw has its own equivalents and keeps them. Tachi never spawns zeroclaw's workers.

**Minimum-viable adoption sequence.** Adopt the *contracts* before any *machinery* — value lands immediately with zero Tachi integration:
1. Report contract + the 12-clause packet template (pure text; makes every zeroclaw dispatch self-sufficient and auditable on day one).
2. The verdict protocol + cross-vendor law (organizational discipline; no code).
3. Then wire the MCP facade for the evidence store and `recommend` (the first machinery — turns traces into routing).
4. Last, the vaccination projection (needs (b) and a signature history to decay).

---

## 8. Premise collapse & open questions

Each of these is a place the doctrine could be *wrong*, not merely unfinished.

**Prompt-layer decay vs structural gates.** The whole card model assumes injected clauses change behavior. But the 07-05 evidence is that prompt-layer carriers *decay* and the most dangerous failure (`falsified_ci_report`) is specifically *not* prompt-fixable. If a host silently ignores injected clauses (a lane that drops the frozen-spec section), the vaccination projection is theater. The mitigation is the durability ordering (§4.1): high-severity failures must be carried by *structural gates* (leader re-verification, review gate), never by prompt text alone. Open: how does Tachi *detect* that a host dropped injected clauses, rather than trusting that it honored them?

**Single-leader assumption.** The loop assumes exactly one adjudicating leader per campaign. Two concurrent leaders adjudicating the same flow, or a leader handoff mid-review, has no defined merge-authority arbitration. The `goal/<issue>` integration-branch pattern bounds the blast radius but does not define who adjudicates when two seats both claim the leader role.

**Card staleness on model version bumps (#734-C3c).** A signature earned by one model version should not silently vaccinate (or un-vaccinate) its successor. Cards key on vendor today, not version. Until versioning is designed, a model bump is a *card-trust discontinuity* the leader must handle manually — and the first-exam protocol (§2.2) is the cold-start answer only for wholly new models, not for version bumps of known ones.

**Guard over-reach — every protective gate needs a read/write asymmetry review (#737).** The #733 guard `enforce_client_project` was added to stop cross-project *writes*; it shipped rejecting cross-project *reads* too, locking out a core feature (cross-library read) for two days. Cross-library reads threaten neither the single-writer invariant nor project isolation — they are served by read-only opens. The lesson generalizes into a standing clause: **any guard that blocks an operation must state which invariant it protects and confirm the blocked operation actually threatens it** — check both sides of the asymmetry before the guard ships. This is the code-methodology "check both sides of an asymmetry" rule promoted to a dispatch-time gate; **ratified into frozen-spec clause 3 (§2.3) on 2026-07-06** so every write-capable packet carries it. Mechanizing it as a lint or a standing reviewer lens (beyond the clause text) remains the open follow-up.

**Self-report trust is a global, not a per-dispatch, parameter.** `falsified_ci_report` degrades whether a vendor's self-reported CI can be believed *at all* — one falsification poisons every future self-report from that vendor until independently re-earned. This is deliberately harsh, and it is the sharpest edge of the "accountability" thesis (§1): the system trusts artifacts and adversarial review, not claims. Open: what re-earns trust, and after how many clean dispatches does the flag decay?
