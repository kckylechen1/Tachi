# The Dispatch Lifecycle canon (怎么派 / 怎么回 / 怎么load card)

Status: canonical doctrine record, ratified by owner across 2026-07-04/05/06 and amended by the #1202 card-authority ruling on 2026-07-17. Implementable and auditable in Tachi; portable to zeroclaw.
Doctrine amended: 2026-08-03
Owner review-routing ruling (2026-08-02): every non-trivial delivery requires distinct implementer/reviewer models with role separation and exact-head evidence. Same-vendor, different-model review satisfies the diversity gate for every risk class; cross-vendor review is optional defense-in-depth unless the owner explicitly freezes it for that delivery.
Owner review-recording ruling (2026-08-03): every non-trivial delivery's required independent review (per the 2026-08-02 ruling above) must also be recorded in the PR body per §2.1; no risk-class carve-out. Two earlier drafts of this ruling did not survive contact with evidence and were retracted before reaching a released head: a defect-concentration theory that narrowed the duty to two risk classes (git-falsified — the "migration race" fix was inside the same-day batch, and the real defects span two separate merge batches, not one surface class), and a "zero recorded review" claim that a direct read of the eight PR bodies disproved (seven already named a reviewing model and verdict; the actual gap is that none recorded the finding or its disposition — see §1). The owning text is in `AGENTS.md`, mirrored verbatim at §2.1; the incident is at §1's worked case. Acceptance-command alignment to `ci.yml`'s `rust` job (§3) is a leader-proposed, owner-unopposed rule adopted the same day, not a separate owner ruling.
Current-state reconciliation base: `274b930a` (2026-07-30). Rows changed by the 2026-08-02 amendment were reverified against that base; untouched historical rows retain their `7c56a130` (2026-07-06) anchors and were not re-audited.
Related: [`experience-to-card-evolution.md`](./experience-to-card-evolution.md), [`dispatch-policy-learning-spec.md`](./dispatch-policy-learning-spec.md), [`subagent-eval-system.md`](./subagent-eval-system.md), [`agent-router-spec.md`](./agent-router-spec.md), [`credentialed-dispatch-profiles.md`](./credentialed-dispatch-profiles.md), [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md), [`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md). Issues/PRs: [#734](https://github.com/kckylechen1/tachi/issues/734), [#735](https://github.com/kckylechen1/tachi/issues/735), [#516](https://github.com/kckylechen1/tachi/issues/516), [#534](https://github.com/kckylechen1/tachi/issues/534), PR [#738](https://github.com/kckylechen1/tachi/pull/738).

This document is the source of truth for how bounded work is dispatched to a lane, how the lane returns evidence, and how a lane's card is loaded and evolved. It distills live dispatch practice (2026-07-05/06) into one canon so the loop can be (a) implemented and audited inside Tachi and (b) ported when zeroclaw — the Rust agent runtime behind the Quant and RomanBath products — adopts the same loop. The doctrine bodies below are owner-ratified law; the current-state map (§6) is the only part that changes as code lands, and every claim there carries a `file:line` / issue / PR anchor.

---

## 1. Thesis — why the loop exists

Two forces make the loop necessary, and neither is optional.

**Short-context lanes.** A dispatch is worthwhile precisely because the worker lane runs in a *fresh, bounded context* — it does not carry the leader's whole session and may have a different failure profile. That is the value (parallelism, cost, and independent discrimination) and the hazard (a lane that cannot see the leader's intent will faithfully execute a flawed spec, invent an alternative, or silently under-deliver). The loop exists to make the packet self-sufficient: the issue body *is* the spec, and both implementer and reviewer read the same frozen text.

**Accountability.** A lane's self-report is the *weakest* evidence in the system. A historical 2026 campaign showed that independent cross-vendor review can catch falsified checks and fake security fixes, but the load-bearing separation is model and role, not vendor. Every non-trivial delivery gets a fresh, read-only reviewer using a different model from the implementer and bound to the actual candidate head. Same-vendor, different-model review satisfies this gate for every risk class; cross-vendor review is optional defense-in-depth unless the owner explicitly freezes it for that delivery. If a different-model reviewer is unavailable, the delivery is incomplete. The leader freezes the spec and adjudicates, and implementers never merge. The leader seat is role-based, which forces the corollary: **the law must live in files and machinery, never in the leader's head.** This document, the packet template, and the card store are that machinery.

**Historical worked case — PR #733 (2026-07-06; not a current routing default).** The stdio-daemon-reuse fix is the loop end-to-end, and its adjudication record ([#733 comment, leader 2026-07-06](https://github.com/kckylechen1/tachi/pull/733)) is the canonical trace:

1. **Implementation** (codex) from a leader-verified base commit `e1c05854` — daemon-side project registry, per-session binding, cross-project override rejection, HTTP-backed write-routing e2e tests.
2. **Adversarial review** (opus, cross-vendor, numbered checkpoints) returned **3 BUGs** — proxied reads narrowed to project-only losing all global recall (empirically proven: 33 global rows → 0 with an injected project); `clippy -D warnings` RED ×3 on `await_holding_lock`; proxied delete/archive of global ids silently no-op'd — plus **2 CONCERNs** explicitly ruled out of Phase-1 scope and tracked as follow-ups.
3. **Prescription rework** (codex) at `ebea3c93` — the review verdict went *verbatim* to the implementer lane; global+project merge on reads, delete/archive fallthrough with honest `db` reporting, clippy fixed structurally (zero `#[allow]`), plus 2 new discriminating goldens.
4. **Independent verification** (sonnet, isolated build + isolated `TACHI_HOME`) — all 6 checkpoints OK, discrimination confirmed by building the pre-fix commit from scratch (`{project:20, global:0}` pre-fix vs `{global:15, project:5}` post-fix), full suite 1282/0, scope exactly 4 files.

Note the lane assignment: codex implemented, opus reviewed, sonnet independently verified — the *pre-07-04 alternate lane* (implementer ≠ reviewer, both ≠ verifier), proving the doctrine is role-shaped, not vendor-pinned. Note also the sequel: the guard #733 shipped over-reached, and [#737](https://github.com/kckylechen1/tachi/issues/737) is the open bug — a read/write-asymmetry lesson we fold into §8.

**Worked case — the 2026-08-02 bare-verdict batch (why every non-trivial delivery now carries a PR-body review-recording duty).** A 2026-08-02 five-minute merge burst landed eight PRs with zero CI runs (repository Actions was disabled at the time). The batch was not review-free: seven of the eight already named a reviewing model and a bare `MERGE-SAFE` verdict on an exact head (Codex 5.5 xhigh, gpt-5.6-terra xhigh) — an earlier draft of this worked case called the batch "zero recorded review," and a direct read of the eight PR bodies disproves that. What none of the seven recorded was *what the review found or how any finding was resolved* — a bare verdict, and CRIT defects landed anyway. A later adversarial audit (#1568, #1570) found real CRIT defects across that stack, but not confined to the 2026-08-02 batch: some surfaces (corpus-apply races, search-filter scoping, export/publish atomicity, ingest-identity handling) are in the 08-02 PRs; others (contradiction-verdict binding, lock-across-await concurrency) belong to the earlier 2026-07-30 batch #1532-#1538 — `contradiction.rs` and `db/graph.rs` carry zero changes in any 2026-08-02 PR, last touched by `618e5c641` (#1534, 2026-07-30). The 08-02 batch count and the audit's finding count are two different measurements from two different batches — not additive. That is the evidence behind the §2.1 rule (full text there, mirrored verbatim from `AGENTS.md`): a recorded seat, model, and bare verdict is not enough — the PR body must show *what an independent seat found and how it was resolved* — and that duty is independent of, and does not relax, the different-model separation §2.4 already requires.

---

## 2. 怎么派 (Dispatch)

### 2.1 Tiering

Every task is placed on a five-tier ladder; the tier decides the ceremony, not the leader's mood.

| Tier | When | Dispatch shape |
|---|---|---|
| **T0** trivial | conversational, tiny edits, docs batches | leader inline, zero ceremony |
| **T1** lookup / 摸现状 | point questions, current-state maps | one cheap read-only explorer |
| **T2** implementation (default) | any real code change | freeze spec+goldens → implementer on isolated worktree → fresh independent read-only reviewer, distinct from the implementer, checks the actual candidate head and returns numbered-checkpoint verdicts (OK / CONCERN / BUG + evidence + Not-checked) → leader adjudicates, merges, dogfoods |
| **T3** design research | open architecture questions | scout → 2–3 diverse strong lanes on the *same frozen question* → verifiers grep-check every citation → leader trap-scores and synthesizes |
| **T4** capstone (rare) | architecture-changing questions | full model×effort collision grid with kill-test-grade verification |

The tier is a routing-topology decision, not merely a carrier choice. Security, credentials, identity/authority, egress, merge/release gates, and other high-risk work escalate verification depth and may justify dual-track review, but the required diversity boundary remains a fresh different-model reviewer. Cross-vendor routing is optional defense-in-depth unless the owner explicitly freezes it for that delivery.

**Mirrored verbatim from `AGENTS.md`'s "Delivery, review, and evidence" section (owner ruling 2026-08-03) — edit that file first, then copy the paragraph here unchanged:**

> One increment on top of this existing requirement, for every non-trivial delivery with no risk-class carve-out (owner-ratified 2026-08-03): the PR body records what the review found and how each finding was resolved (accepted / rejected / downgraded) — not just which seat and model reviewed and a bare verdict; the evidence below shows seat+model+verdict alone does not work. This documentation duty can be satisfied by an owner-ratified in-house cold-review seat — no external human reviewer or GitHub review-status entry is required — but it does not relax the different-model requirement above: the model named in the record must still differ from the implementer's. A record with a verdict but no enumerated findings is incomplete in the same way an unavailable different-model reviewer is. Evidence: a 2026-08-02 five-minute burst of eight PRs merged with zero CI runs (repository Actions is disabled) and no PR-body record of *what* any review found — seven of the eight named a reviewing model and a MERGE-SAFE verdict on an exact head, none enumerated findings or their disposition. A later adversarial audit (#1568, #1570) still found CRIT defects in that stack.

**Execution-owner invariant.** These topologies describe roles and evidence gates, not a command to use Tachi as the worker launcher. The host harness's native subagent owns ordinary local execution and session lifecycle. Tachi supplies memory, policy, claims, ledger, receipts, and eval—including mirrored native-worker outcomes. Static dispatch-profile diagnostics are operator-only on the local CLI and never grant launch authority. The one launch-capable Tachi surface is `tachi_staff(action='start')` (the retired `tachi_task(action='dispatch')`, `tachi_shell(async_dispatch=true)`, and `tachi_arena(spawn, launch=true)` were deleted in #1319-C2/B7/D2). It is opt-in only for an explicit owner request, durable work that must outlive the current harness session, cross-device/remote pickup, or absence of a usable native subagent, expressed as the closed typed `staffing_reason` before any run artifact is created. `recommend` is advisory and never converts into staffing authority by itself.

Examples: “inspect these three modules in parallel” stays inside the harness's native subagents, even if the leader records their outcomes in Tachi. “Run this overnight after my current harness exits and let another device pick it up” may use managed dispatch with an explicit durable or cross-device reason. Choosing a carrier alone is not a managed-dispatch exception.

### 2.2 Pre-dispatch card consult

Before a T2/T3 packet is frozen, the leader consults the card store (surfaces in §4). The consult produces four things, in order:

1. **operator profile consult** — an operator may inspect the static profile/admission diagnostic with `tachi card list` or `tachi card show <profile-id>`. It is not a model-facing Task action, does not inspect dynamic eval or route overlays, and is not permission or launch approval. Use `tachi_tune(action="route_simulate")` separately when an authorized route-policy simulation is needed.
2. **loadout** — the static dispatch-profile definition: the reviewed skill loadout and evidence contract the profile projects (rendered via the dispatch-profile overlay; `tachi_skill(action="discover"|"run")` serves the reviewed static skills — the retired loadout/bundle capability intelligence was deleted in #1690 C3).
3. **vaccination projection** — the top-N ACT-R-decayed counter-clauses for this `(role, vendor)`, injected verbatim into the packet's frozen-spec section as *additional mandatory clauses* (the wire is PR #738; see §4 and §6).
4. **trust-flag consumption** — if the vendor carries an unresolved `falsified_ci_report` signature, its `self_report_trust` is low and the packet mandates independent re-verification of *every* self-report.

The consult is advisory in the T-tiering sense (the leader can override), but the override is recorded, not silent — the same discipline the risk classifier applies to `risk_override`.

### 2.3 Packet freezing — the issue body IS the spec

A T2 packet is a frozen leaf issue. Its body is the single text both implementer and reviewer read; it carries the goldens, the enumerated CI-gate list, an `Execution:` lane marker (`solo-frozen` | `dual-track` | `mechanical`), and the twelve frozen-spec clauses. These clauses are simultaneously the packet template and the leader-side vaccination rules — they transfer verbatim to machine dispatch (`tachi execute`, #516):

1. **Never weaken a frozen assertion.** If a golden can't pass, STOP and report — a faithfully-executed flawed spec is the spec author's bug, not the lane's.
2. **Do-not-touch zones carry the exception** "consistency fixes may be unlocked by adjudication" (a sealed zone once hid a real bug from the implementer).
3. **Any added guard also freezes the guard's persistent-failure behavior** (a cross-day guard without one became a requery storm), **and names the invariant it protects and confirms the blocked operation actually threatens it** — check both sides of the read/write asymmetry before the guard ships (the #733 write-guard over-blocked reads, locking out cross-library read for two days, #737). For a safety-gated maintenance scan, the regression test proves both halves in one invocation: the unsafe unit remains excluded and an allowed peer progresses; the result carries typed accounting for every examined unit, rather than silently treating exclusions as no work.
4. **Consolidating shared code: enumerate the replaced implementation's input equivalence classes first** (including empty/zero/missing); tautological `wrapper == delegate` tests are banned.
5. **Payload trimming produces a decision-evidence leaf whitelist with presence assertions BEFORE cutting.** Content fields are atomic — kept whole or dropped whole, never truncated.
6. **Resident-process protection windows** — never kill/restart a daemon during its protected window (A-share market hours for this machine's daemons).
7. **Concurrent-tree discipline** — never touch another agent's dirty/untracked files; reconcile by commit tree, not branch name.
8. **The verification list enumerates EVERY CI gate of the repo** (fmt, clippy `-D warnings`, full suite, gitleaks/audit) — a gate absent from the spec is a post-merge surprise.
9. **No flat magic numbers** — size/limit thresholds are per-action, named, marked provisional, and calibrated from telemetry later.
10. **Changing a shared response/behavior surface: enumerate ALL entry routes and legacy params FIRST** (a single global keep-list once broke five variant routes while every targeted suite stayed green).
11. **Every dispatch AND every resume restates the workspace law** — worktree cut from a leader-verified base SHA (never the primary checkout), the shared-or-isolated `CARGO_TARGET_DIR` decision, and the report contract (verbatim `test result:` lines or the delivery is incomplete). Shared `CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target` is a speed path, not a correctness guarantee: concurrent same-crate worktrees can collide on metadata-hashed test binaries and produce phantom failures. Reviewer/discrimination runs for the crate under review use an isolated target dir when another lane may be building the same crate, or the packet explicitly states why shared target reuse is safe.
   - **Disk-hygiene addendum (tachi#1184, 2026-07-17 incident — 17 private targets, ~56G, one campaign).** A lane never self-mints a private `CARGO_TARGET_DIR` to route around a shared-target lock — that is the exact failure this incident traced to: parallel lanes hit lock contention on the shared target, nobody had told them to queue, so each quietly grew its own multi-GB `target/`. The clause is now explicit: **build work either queues through the Oz build seat, or the packet/dispatch explicitly declares shared-target usage on argv** (not env-only — an env-only declaration is invisible to a holder probe that reads `ps` argv; see `crates/tachi-exec-env-reaper/src/lib.rs` doc comment, tachi#1062 BUG 1). For dispatches routed through Tachi's own `dispatch_profile` system, this is now load-bearing profile contract, not just prose: `glm_impl` and `opencode_builder` (`crates/tachi-dispatch/src/profiles.rs`) both carry `forbidden_skills: ["self_managed_cargo_target_dir", …]` and `passive_traits: ["build_through_oz_or_declared_shared_target", …]`, rendered verbatim into every packet's `## Dispatch profile` section (`dispatch_ops/prompt/overlays.rs::render_dispatch_profile_overlay`).
   - **Worktree-location law (tachi#1184 item 3).** A worktree meant to outlive its dispatching session belongs under a durable cache root, never a session scratch dir (`/private/tmp`, `/tmp`, `$TMPDIR`) — those are wiped on reboot and orphan the tree's gitdir. Tachi's own managed-worktree entrypoint (`tachi worktree open`, `tools/cleaner/src/wt_open.rs::open_worktree`) already enforces this at the mechanism level: the default provisioning root is `~/.cache/tachi/worktrees/<repo-slug>/`, and any explicit `--path` outside that managed root is a **hard refusal** (`path_outside_managed_root_reason`) — stricter than a warning, by design, since that boundary doubles as the sweep GC root and path-fence (CP3/CP6). The one gap that boundary cannot close by itself is a caller pointing `TACHI_WORKTREES_ROOT` *itself* at an ephemeral volume (which makes that volume the "managed root" and therefore compliant by the boundary's own definition) — `default_worktrees_root` now emits a **loud, non-fatal warning** in that one case (a hard fail there would wrongly block a deliberate throwaway/test root). None of this reaches a lane that bypasses `tachi worktree open` entirely and runs a raw `git worktree add /private/tmp/...` — that bypass is exactly why this clause exists as packet-carried discipline, not just a CLI-level gate: **every dispatch that opens its own worktree without going through `tachi worktree open` must still land it under a durable cache root** (e.g. `~/.cache/<repo>-worktrees/`), and `tachi doctor`'s build-resource patrol (below) is the backstop that catches an existing violation either way.
   - **Recycle patrol (tachi#1184 item 2).** `tachi doctor` folds in a report-only build-resource section (`crate::doctor::build_resources`): private, dead-looking `CARGO_TARGET_DIR`-shaped directories (name-matched, not on the blessed shared-target allowlist, stale, unheld) and inspection notes on every registered managed worktree (age, existence, attribution) — sizes, ages, and a suggested reclaim command, never an auto-delete. It answers "is there a build-resource orphan on this machine right now", reusing `exec_env_reaper`'s already-certified (#1062) scan/protection/holder-probe machinery rather than re-deriving it. It does **not** compute a worktree "safe to close" verdict — that reconciliation semantic is tachi#1118's frozen scope, not this patrol's.
12. **Leaf issues ARE the bounded dispatch spec** — frozen at dispatch time and carrying the `Execution:` lane marker. Durable architecture remains in a canonical repo doc. During the #1002 migration window, the leader snapshots the exact body/doc revision and records its reproducible hash in the dispatch run artifact; `Spec-Ref` and `Derived-From` are recommended. After the typed resolver/receipt slice lands, every new leaf pins `Spec-Ref: owner/repo:path@commit_sha/blob_sha#section`, `Derived-From: <owner ruling/comment ids@body_hash>`, and `Freeze-Receipt: <append-only receipt id>` per [`issue-refinery-memory-lanes.md`](./issue-refinery-memory-lanes.md). Existing leaves are not retroactively invalidated. A changed issue body hash, source comment body hash, or trusted doc revision requires explicit re-freezing.

### 2.4 Lane selection and review diversity

Review diversity is model- and role-based, not pinned to a carrier assignment. For every non-trivial delivery, the reviewer is fresh, independent, read-only, uses a different model from the implementer, and checks the actual candidate head. The launcher or route receipt must show a distinct model identity; a new session, profile alias, persona, or reasoning-effort change on the same model does not count. Same-vendor, different-model review satisfies the diversity gate for every risk class. Cross-vendor routing may add defense-in-depth or satisfy an owner-frozen delivery requirement, but it is not an automatic release gate. An unavailable different-model reviewer leaves the gate incomplete; self-review or a fabricated verdict never substitutes. Any repair, rebase, merge, or other candidate-changing action invalidates the prior verdict, so the new candidate head receives a fresh review. Whoever implements does not review the same slice. The leader freezes and adjudicates and belongs to neither lane.

This section is the owning section for the review-diversity gate itself; the 2026-08-03 review-recording ruling (§2.1, mirrored from `AGENTS.md`) adds a documentation duty on top of it for every non-trivial delivery, no risk-class carve-out — that duty never substitutes for the different-model separation this section requires.

**Helper-agent routing (owner-ratified 2026-07-20):** a session that needs helper agents spawns them through its own harness's native subagent mechanism. Tachi task dispatch is a leader-level cross-carrier lane mechanism, not a substitute subagent pool — a session that keeps routing helper work through Tachi dispatch instead of its own agents is mis-routing, even when each individual dispatch succeeds. This is a routing rule, not a ban on helpers: different-model independent review still applies to whatever helpers a session spawns.

### 2.5 Candidate freeze and maintainability budget

The repository pays expensive acceptance ceremony once per stable candidate, not once per repair attempt. This is a scheduling rule; it never narrows the command set owed by `AGENTS.md` or the packet.

1. **Implement and discriminate.** While code, conflict resolution, or ancestry is still changing, run the smallest behavioral tests that distinguish the contract plus proportional fmt/check/clippy coverage. A knowingly provisional head does not receive the full workspace gate merely to produce a receipt that the next edit will invalidate.
2. **Integrate before review.** Resolve the intended base, stacked ancestry, conflict decisions, generated artifacts, and frozen fixtures before declaring a candidate. Integration after review changes the candidate and therefore invalidates the verdict.
3. **Freeze and review.** Bind a fresh, independent, different-model review to the exact candidate head. Apply accepted findings through a separate writer and repeat review until the candidate is stable. A review is evidence about one object, not about a branch name.
4. **Run canonical acceptance once.** Run and quote every required `ci.yml` Rust `run:` command on that reviewed exact head. If a failure requires a candidate-changing repair, both the prior review and the gate receipt are stale: repair, re-review, then rerun the canonical gates.

Every non-trivial packet and PR also carries a maintainability budget. The following are review triggers, not automatic rejections:

- **More than 20 changed files:** classify the count separately as production, tests, goldens, generated files, migrations, and deletions, then explain why one bounded contract cannot be split into independently reviewable leaves.
- **More than 2,000 changed production lines:** split by default. Keeping one delivery requires evidence that splitting would break an atomic migration, wire/security boundary, or frozen behavior proof.
- **More than 5 new public types:** perform a vocabulary review, name the production consumers, reject parallel representations of an existing concept, and explain why the contract cannot be split into smaller leaves.
- **A new abstraction:** require at least two real production consumers, or one named external/security boundary whose isolation is itself the contract. This includes, but is not limited to, traits, wrappers, facades, registries, policies, and services. Tests, fixtures, and hypothetical future callers do not count as production consumers.
- **A new proof artifact:** bind it to a production decision branch, public or external wire contract, or concrete mutant that the old suite admits. Extend the canonical census/golden where one exists instead of creating a parallel list that can drift.

When the owner withdraws a product direction, the next change starts with a production-caller census. A zero-consumer experimental surface is deleted by default; Git and the PR retain its history. Compatibility or migration exceptions name the still-live boundary and its removal condition.

Source comments explain the current invariant, threat model, surprising mechanism, or canonical specification. Reviewer identities, review rounds, finding labels, commit anecdotes, and superseded implementation history belong in the PR or an owning design/decision record. Migration-compatibility comments may retain dated history only where the date or former shape is required to operate or remove the compatibility path safely.

A large file is not split by inventing another architecture layer. Once a contract file exceeds 1,500 lines or 20 public types, new invariants go into physical submodules grouped by the existing vocabulary; the move preserves behavior and public paths unless a separate leaf explicitly changes them.

---

## 3. 怎么回 (Return)

The return path is a contract, not a courtesy. A delivery that omits any required element is *incomplete*, and the leader treats it as launch-not-done.

**Report contract.** Verbatim `test result:` lines for every suite the packet enumerated; the exact CI-gate output (not a self-graded checkbox); the base SHA the work was cut from; the exact file scope actually touched; and the candidate head reviewed. A red test reported red is a complete delivery; a green claim without the verbatim line is not. A candidate-changing repair or rebase makes the prior verdict stale.

**Acceptance-command fidelity** (leader-proposed 2026-08-03, owner unopposed). Operative text lives in `AGENTS.md`'s "Delivery, review, and evidence" section — edit that file first; this entry is a pointer plus rationale, not an independent copy, so the two cannot drift into different scopes again. Rationale: `ci.yml` has four jobs, and only the `rust` job's `run:` steps are things a lane can reproduce and quote verbatim — its `uses:` steps (checkout, Rust setup, the nextest installer, the JUnit-artifact upload) are tooling, not commands, and the other three jobs are not runnable-as-quoted at all (`build-seat-setup`'s inline policy script, `physical-db-identity-windows`'s Windows-only runner, `node`'s `${{ matrix.package }}` fan-out) — so those are named-and-excused, not silently dropped, per the AGENTS.md text. This bullet only states command alignment; it does not redefine the CI-gate enumeration in clause 8 of §2.3.

**Red-then-green golden evidence.** Every new or extended test that guards a behavior/security change must be shown to FAIL on the pre-fix code and PASS after — the discrimination check. #733's verification is the model: the reviewer rebuilt the pre-fix commit from scratch and showed `{project:20, global:0}` → `{global:15, project:5}`. Where a structural (not runtime) discrimination is the honest justification, that justification is stated, not skipped.

**Deviation flagging, never self-ratification.** If the lane cannot satisfy the spec, it STOPs and reports the deviation for adjudication. A lane never decides on its own that a frozen assertion was wrong — clause 1 makes that the spec author's problem to fix and re-dispatch.

**Completion-ownership for detached jobs.** A lane that spawns an untracked child job *owns polling it to terminal state*. A reply containing a bare job-id is a protocol violation — treat it as launch-not-done and take over polling. A 40-minute cap returns an explicit `STILL-RUNNING job-id=…` marker rather than a false completion. (This is the two-layer completion trap; today it lives in host-side lane patches, not Tachi code — see §6 gap.)

**Anti-fabrication sentinels.** Failures carry a `LANE-FAILURE` prefix; every report echoes its dispatch id and run-directory so a fabricated report is detectable by *absent artifacts* — the `toolless_fabrication` signature (a toolless lane inventing a whole report when its tools were missing). Presence of the artifact is the check, not the prose.

**判决单三档制 (three-tier verdict protocol).** A reviewer's verdict is routed by severity class, and the routing is doctrine, not discretion:

- **① trivial** (EOF newline, missing import, assertion-wording) → a distinct writer (the implementation lane or leader) may patch under pre-authorization with an attached diff; the reviewer remains read-only. The changed head always receives a fresh different-model review, although no new issue or packet ceremony is required.
- **② prescription** (finding is unambiguous, the fix is unique) → the verdict text goes *verbatim* to an implementation lane with a one-line adjudication header (accept/reject per finding); the leader does not rewrite it into a new spec. This is the #733 rework: review verdict → verbatim → codex → one-shot correct.
- **③ adjudication** (spec right-or-wrong, fix-vs-revert-vs-seal, doctrine conflict, suspected non-bug) → must pass through the leader.

The load-bearing rule underneath all three: **reviewers never self-fix their own findings into main.** Self-fixing is a blind-spot pass-through (the reviewer's own gaps go unreviewed), builds level-2 on a possibly-wrong foundation, and makes the next round review the reviewer's own patch — a conflicted seat. The correct disposition of a review finding is sometimes *revert + seal*, not *build the suggested fix*; only response/execution separation surfaces that.

**Adjudication + merge authority.** Merging is the adjudicator's act, performed *after reading the diff personally*. Implementers open PRs and STOP. Per-goal integration branches (`goal/<issue>`) can bound integration without transferring merge authority; exactly one reviewed PR goes `goal/* → main`. PRs `Refs`/`Related`, never `Closes`, umbrella and no-close issues.

**What gets recorded on complete.** `tachi_task(action="complete")` writes a per-dispatch eval row under `/eval/YYYY-MM-DD/<task_id>` (`category="eval"`, excluded from ordinary recall) — mechanical facts extracted deterministically (test counts, CI conclusion, rework rounds, reviewer OK/CONCERN/BUG tally, wall-clock) plus a judgment distillation (signature classification, per-axis scores, counter-clause proposals) authored by a reasoning seat reading the adjudication trace. This is recorded *even on failure* — a failed dispatch is a labeled training row, not wasted effort. Lore trailers on the merge commit (`Constraint:`, `Rejected:`, `Confidence:`, `Tested:`/`Not-tested:`) are decision records the diff cannot carry.

---

## 4. 怎么load card (Cards)

### 4.1 Ontology — four fields

A card is not flavor text; a card that does not change routing is not a valid card.

1. **Capability** (擅长什么) — a routing score per task type; the machine-readable form of the intuition hexagon.
2. **Failure modes** (哪会出错) — error signatures + risk gates. High-severity domains change verification depth and may add dual-track review, while preserving the same required diversity boundary: a fresh different-model reviewer at the actual candidate head. Cross-vendor routing is optional defense-in-depth.
3. **Constraint interface** (怎么约束) — the carrier layers, ordered by durability (proven 2026-07-05): prompt-layer (decays) < one-line config < per-dispatch packet clauses < structural gates (scripts/CI, cannot be ignored).
4. **Constraint efficacy** (约束有效性) — the most valuable field: *which carrier layer actually works for which failure mode, per vendor*. Evidence: one vendor's false-`Closes` was NOT prompt-fixable (the adjudication comment sat on the issue and was violated anyway) — only structural gates caught it; another vendor's parking urge WAS prompt-fixable by remapping "mandate = the whole ledger" onto its own end-to-end vocabulary. Same disease class, different medicine layer per vendor.

### 4.2 Storage split — three layers, never merged

Owner ruling #1202 (2026-07-17) supersedes the original TOML-as-card-declaration shape. The three truth species remain separate, but the reviewed declaration is now narrative rather than a persisted statistical card:

- **Declaration** (role × vendor positioning, dated failure patterns, packet-ready counter-clauses, thin routing frontmatter) → reviewed Markdown lane cards under the governed dispatch-ledger card source. Leader/owner approval controls atomic append; model/eval output may draft but never writes the declaration directly. Tachi ingests a read-only mirror. TOML remains valid for typed runtime/profile configuration, not as a competing lane-card authority.
- **Evidence** (eval rows, signatures, timestamps, adjudication/run references) → SQLite, append-only, temporal, `(role, vendor)`-queryable. Evidence proposes or supports a card amendment; it does not become declaration merely because it was recorded. The medical record.
- **Projection** (current counter-clauses, card excerpt, routing index) → computed from the reviewed card plus evidence at render/assembly time, *never persisted as a second truth*. The MBIT/statistical summary machinery is **retired by #1690 C3** — where such summaries existed, they were derived evidence only, never an alternate authority. The health report — always reproducible from the reviewed declaration and record, never hand-edited as an alternate authority.

Engineering precedent, user values/goals/habits, Soul disposition, and lane-card operational evidence remain separate content authorities even when they reuse proposal/review/apply machinery (#950, #953, #858, #1202).

### 4.3 Surfaces

The card store is consumed through the existing domain facades — no new `tachi_router` / `tachi_policy` facade is invented (dispatch-policy-learning-spec.md §Public-Facade-Rule):

- `tachi card list [--json]` / `tachi card show <profile-id> [--json]` — operator-only static profile/admission diagnostics (`tachi.operator_profile.v1`); this is not a model-facing MCP surface and is not launch approval.
- `tachi_tune(action="route_simulate")` — profile choice simulation from risk + eval matrix.
- `tachi_tune(action="route_proposals" | "route_review" | "route_apply")` — human-gated route-policy and evidence-contract proposals (admin/operator only since #1426; the loadout-evolution proposals that used to ride this surface are retired by #1690 C3).
- `tachi_skill(action="discover" | "run")` — reviewed static skills only (the retired `loadout`/`bundle` actions and capability-bundle intelligence were deleted in #1690 C3).
- `tachi_task(action="complete")` — writes the eval evidence row that feeds the surviving card surfaces: signature/vaccination evidence and human-gated route-policy/evidence-contract proposals (the loadout-evolution machinery is retired by #1690 C3).

### 4.4 Vaccination projection (landed by PR #738)

The vaccination wire — record a `(vendor, role, signature)` row on adjudication, then project that card's top-N ACT-R-decayed counter-clauses (verbatim from the frozen taxonomy) into the next packet's frozen-spec section — is specified in [`experience-to-card-evolution.md`](./experience-to-card-evolution.md) and landed through PR [#738](https://github.com/kckylechen1/tachi/pull/738). Current source stores signature evidence in `crates/tachi-server/src/signature_evidence.rs`, computes the taxonomy and trust projection in `crates/tachi-dispatch/src/signatures.rs`, and injects it through `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs`. The signature taxonomy remains frozen from the 07-05 campaign (`fake_security_fix`, `zero_discriminating_test`, `falsified_ci_report` [critical — degrades global self-report trust], `self_close_overreach`, `inherited_base_commit`, `stale_rlib_poisoning`, `breadcrumb_violation`, `parking_after_contract`).

**Versioning caveat (OPEN, #734-C3c).** Whether a card should be pinned to a specific model *version* — so a signature earned by `glm-5.1` does not silently vaccinate `glm-5.2` — is unresolved. Cards today key on vendor/backend, not version; a model bump can therefore carry stale counter-clauses or shed real ones. Tracked, not designed here (§8).

---

## 5. The closed loop

One diagram. Each arrow is annotated **[code]** (exists at HEAD with an anchor in §6) or **[doctrine]** (leader-manual, no machine surface yet).

```
   incident (a live dispatch failure or success)
        │  [doctrine] leader runs the T2 loop
        ▼
    adjudication trace  ──[code] tachi_task(action=complete) writes /eval row──►  eval evidence (SQLite, append-only)
         │                                                                    │
         │ [code] distill a typed error_signature                            │ [code] aggregate_live →
         ▼                                                                    ▼  performance matrix (reporting only)
    signature on the (role,vendor) card ──[code] ACT-R decay──►  projection (top-N counter-clauses)
         │                                                                    │
         │ [code] inject verbatim into packet frozen-spec                    │ [code] recommend consumes the
         ▼                                                                    ▼  DecisionFactLedger, NOT the matrix (#1690 C3 S2)
    next packet (12 clauses + vaccines + goldens) ──[doctrine] dispatch──►  outcome ──►  eval row ──► card
```

- **Present at HEAD [code]:** eval-row write on complete, the live performance matrix (`aggregate_live` — a read-only reporting surface, no longer consumed by `recommend`), `recommend` consuming the **DecisionFactLedger** (#1690 C3 S2: live /eval matrix consumption is retired with the MBIT/evolution machinery; #1675 owns the eval future), the deterministic risk classifier, route-policy proposals/apply, `(role, vendor)` signature evidence, counter-clause projection, the `self_report_trust` flag, and the evidence-contract projection (the skill/trait/weak-against overlay projection is retired by #1690 C3).
- **Doctrine-only [doctrine]:** the leader running the loop, the 12-clause packet emission, the three-tier verdict routing, completion-ownership, the anti-fabrication artifact check, first-exam eligibility. These are law carried by files and the leader, not yet by Tachi code.

---

## 6. Current-state map & gap list

Rows changed by the 2026-08-02 amendment were verified at base `274b930a`; untouched historical rows retain the `7c56a130` (Release 1.6.4) snapshot and may require a new census before execution. "GAP" means the doctrine above is real law but no code surface implements it at the row's named anchor; each GAP becomes one bounded child-issue seed — **an issue seed, not a design**.

| Lifecycle step | Doctrine (§) | Code anchor at HEAD, or GAP | Child-issue seed |
|---|---|---|---|
| Deterministic risk classification | §2.2, §2.1 | **Present implementation reality at the 2026-07-19 anchor:** `crates/tachi-dispatch/src/routing.rs:266-286` (risk → required/blocked_profiles; high/critical currently requires named profiles `claude_plan`+`codex_55_review` and blocks `codex_53_fast`). This is a **migration gap** against the carrier-neutral, risk-tiered routing target in §2.1/§2.4; the doctrine amendment does not claim runtime migration. | "Migrate `routing.rs` from named carrier/profile coupling to risk-tiered carrier-neutral policy inputs; preserve explicit high/critical safety gates and add route discrimination coverage before changing the current profile behavior." |
| Profile recommend | §2.2 | `crates/tachi-dispatch/src/routing.rs:288` `recommend_dispatch_profile_candidates`; evidence from the **DecisionFactLedger** (`crates/tachi-server/src/dispatch_profile/routing/recommendation.rs`), NOT the live /eval matrix — matrix consumption is retired by #1690 C3 S2 (the no-evidence path abstains) | — |
| Profile / card definition | §4.1 | `crates/tachi-dispatch/src/profiles.rs:37-60` `DispatchProfileDef` (role-keyed; per-backend profiles from `:62`) | — |
| Card overlay projection (skills/traits/weak-against) | §4.4 | **RETIRED by #1690 C3** — `projected_signature_skills` survives only as an always-empty read-only loadout key; the surviving family is the evidence-contract projection (`crates/tachi-server/src/dispatch_profile/cards.rs` renders the static reviewed loadout + `self_report_trust`; `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs` renders static loadout + `projected_required`) | — |
| Eval row on complete | §3 | `crates/tachi-server/src/complete_ops/eval_record.rs:39` (path `/eval/{date}/{task_id}`), `:283` (`category="eval"`), `:206-223` (subagents); handler `crates/tachi-server/src/complete_ops/handler.rs:14` | — |
| Route-policy proposals / apply | §4.3 | `tachi_tune(action="route_proposals"|"route_review"|"route_apply")` per `crates/tachi-params/src/facade/tune.rs`; rules persisted to `dispatch_route_policy_rules` (dispatch-policy-learning-spec.md:266-274) | — |
| Facade surface (no new router facade) | §4.3 | `crates/tachi-params/src/facade/task.rs:13-48`; `merge`=local worktree only (`:49-50`), PR merges via `tachi_gh(safe_merge)` | — |
| `(role, vendor)` signature key | §4.2, §4.4 | `crates/tachi-server/src/signature_evidence.rs:93` records typed evidence; `crates/tachi-dispatch/src/signatures.rs:277` projects per-role/vendor counter-clauses | — |
| Error-signature extraction from adjudication | §3, §5 | `crates/tachi-server/src/complete_ops/handler.rs:909` records completion signatures through `signature_evidence` | — |
| Counter-clause projection into packet | §2.2, §4.4 | `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs:120-146` renders ACT-R-decayed clauses into the dispatch overlay | — |
| `self_report_trust` flag consumption | §2.2, §3 | `crates/tachi-server/src/dispatch_profile/cards.rs:31-57` and `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs:122-136` surface low-trust evidence | — |
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
- the **evidence store** (`/eval` append-only rows via `tachi_task(action="complete")`);
- the **projection** (card overlay / counter-clause assembly);
- the **eval ledger + performance matrix** (`aggregate_live`);
- **recommend** (profile choice consuming the DecisionFactLedger — evidence-backed, abstains without sufficient evidence; see §2.2).

zeroclaw reaches these through the same `tachi_task` / `tachi_skill` facades any host uses — the host-adapter lifecycle hooks (`before_prompt`, `after_session`) are the neutral wiring.

**(c) What zeroclaw implements NATIVELY.** Its own worker spawning, process transport, sandboxing, and reliability layer (retry / idle-timeout / tool-rejection repair) — the agent-router-spec machinery is Tachi's; zeroclaw has its own equivalents and keeps them. Tachi never spawns zeroclaw's workers.

**Minimum-viable adoption sequence.** Adopt the *contracts* before any *machinery* — value lands immediately with zero Tachi integration:
1. Report contract + the 12-clause packet template (pure text; makes every zeroclaw dispatch self-sufficient and auditable on day one).
2. The verdict protocol + risk-tiered review-diversity law (organizational discipline; no code).
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
