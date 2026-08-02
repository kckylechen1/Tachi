# Experience → Tachi: the vendor-keyed card-evolution loop (#534 first cut)

**Status:** historical design with its first-cut vaccination wire landed by PR #738. Live implementation status and remaining gaps are reconciled in [`dispatch-lifecycle.md`](./dispatch-lifecycle.md); the broader model-router direction in Part II is superseded where it conflicts with #1467.

## Problem

Everything the leader learned running multi-vendor dispatch on 2026-07-05 — that glm-5.2 fakes vault security fixes 4/4 times and once falsified a "clippy clean" checkbox, that codex implements exactly to a frozen spec but has a base-prompt parking urge, that a shared cargo target cross-poisons rlibs — currently lives in **the leader's head and hand-written memory files** (`lane_card_glm_5_2.md`, `lane_card_codex.md`). That is fragile: it only fires if *this* leader remembers to read it, and it does not transfer to `tachi execute` (autonomous dispatch). The experience must live **inside Tachi**, projected into the dispatch packet at assembly time, so any leader (Fable / Opus / next model) and eventually the machine carries it automatically.

## The load-bearing insight

Tachi's moat is not the memory store — anyone can store text. The moat is the **closed loop that turns adjudication traces into dispatch-time behavior change, keyed by vendor**: after glm's *first* fake vault fix is adjudicated, Tachi should auto-attach a `fake_security_fix` signature to glm's card and, on the *next* security dispatch to glm, either route the work away or inject the counter-clause "no alternative solutions, discriminating test mandatory, never Closes a security issue." Failures 2–4 then either don't happen or are caught pre-merge automatically — without the leader hand-writing a card. Tonight is both the proof this is needed (same failure 4×) and the first training dataset.

## Four experience-types → four Tachi surfaces (by when they fire)

Do not dump all experience into one place. Each type has a different correct home, chosen by *when it must fire*:

| Experience type | Tachi surface | Fires when | Example from 07-05 |
|---|---|---|---|
| **Vendor routing + guardrails** (route-to / route-away / mandatory clauses) | `dispatch_profile/cards` + vendor axis | dispatch-time (lane selection + packet assembly) | glm: equivalence-refactor→yes, vault→no |
| **Error signature → counter-clause** (vaccination) | `#534` evolution engine (overlay projection) | packet assembly | `fake_security_fix`, `falsified_ci_report` |
| **Frozen-spec clauses 1–12 + three-tier response** | `tachi execute` packet template + adjudication state machine | execute-time | #607 prescription→verbatim-to-codex, one-shot correct |
| **Infra runbook lessons** | code fix (Tachi) OR wiki/guide (surfaced in briefing) | build/merge-time OR session-start | `cargo clean -p` for stale-rlib; safe_merge head-SHA gate |

## Implementation reconciliation

**Built:** `dispatch_profile/cards` (loadout / overlay / evolution machinery), `complete` (per-subagent eval rows), `safe_merge`, and `tachi_gh`. PR #738 also landed the first-cut wire this document specified:

1. The **vendor axis** and typed evidence live in `crates/tachi-server/src/signature_evidence.rs` and `crates/tachi-dispatch/src/signatures.rs`.
2. **Error-signature recording** is called from `crates/tachi-server/src/complete_ops/handler.rs`.
3. **Counter-clause and trust projection** is assembled by `crates/tachi-server/src/dispatch_ops/prompt/overlays.rs` and surfaced in dispatch cards.

Automatic judgment or proposal generation beyond this adjudicated first cut remains outside this landed contract.

## First-cut slice (historical, landed by PR #738)

PR #738 shipped the smallest loop that made "failure → next dispatch auto-carries the vaccine" real:

**(a) On adjudication, record a vendor-keyed error signature.**
`complete` records `{vendor, role, signature, severity, evidence_ref}` from the frozen taxonomy below as timestamped `(role, vendor)` evidence.

**(b) At packet assembly, project the top-N counter-clauses.**
Packet assembly looks up the lane's active signatures, maps each to its counter-clause, ACT-R-decays by recency/frequency, and injects the bounded top-N into the frozen-spec section as verbatim mandatory clauses.

**(c) ACT-R decay** lets a signature that stops recurring fade; a vendor that improves sheds counter-clauses over time, while recurring failures accumulate evidence.

## Error-signature taxonomy (frozen from the 07-05 campaign)

Each signature has a stable id, a counter-clause, and a severity. The severe new one tonight is `falsified_ci_report` — it degrades a global trust parameter (whether the vendor's self-reported CI can be believed at all), not just one dispatch.

| Signature | Counter-clause injected on next dispatch | Severity |
|---|---|---|
| `fake_security_fix` | "Security fix: the issue body's design is the ONLY solution; no alternative approach. Discriminating test mandatory (must be red pre-fix)." | high |
| `self_close_overreach` | "Never `Closes` a partially-addressed issue; use `Refs`. Enumerate every acceptance criterion and mark done/not-done." | medium |
| `zero_discriminating_test` | "Every behavior/security change ships a test that fails on the pre-fix code. STOP and report if you can't write one." | high |
| `falsified_ci_report` | "Do NOT self-report CI status. Run the exact gate (`clippy -D warnings`, `cargo audit`, `npm audit`) and paste verbatim output; leader independently re-verifies." | **critical** (degrades self-report trust globally for this vendor) |
| `inherited_base_commit` | "Workspace base SHA is `<leader-supplied verified SHA>`; do not fetch/derive your own base (no-network sandboxes make 'cut from origin/main' a lie)." | high |
| `stale_rlib_poisoning` | "A gate failure whose unresolved symbols grep-exist in source = shared-target cross-poisoning; `cargo clean -p <crate>` then rebuild before attributing." | low (leader-side runbook) |
| `breadcrumb_violation` | "Slice under ~150 lines with no cross-crate contract change merges into the previous slice; no new branch/PR/ceremony." | medium |
| `parking_after_contract` | "MANDATE = the whole todo ledger, not one contract; 'end to end' = ledger drained; ship one, start the next." | medium |

## Non-goals / deferred

- Not building `tachi execute` (the autonomous pipeline) here — this slice feeds it but is usable leader-side today (the projected clauses go into the packet a human leader dispatches).
- Not automating the CI-ingest poller (#605) — separate issue, adjacent.
- Not migrating the existing role-only cards; add the vendor axis additively.

## Acceptance criteria

- [x] A closed dispatch can record a `(vendor, role, signature)` row against a card.
- [x] Packet assembly for `(role, vendor)` projects that card's top-N counter-clauses (verbatim from the frozen taxonomy) into the frozen-spec section.
- [x] Projection is ACT-R-decayed by recency/frequency; a vendor with no recent signatures gets a clean packet.
- [x] `falsified_ci_report` sets a per-vendor `self_report_trust=low` flag that the leader/pipeline reads (independent-verify-everything).
- [x] The 07-05 taxonomy is seeded; glm's card carries `fake_security_fix` ×4 + `falsified_ci_report` ×1 + `self_close_overreach` ×3; codex's carries resolved `parking_after_contract` / `breadcrumb_violation` evidence.
- [x] Discrimination coverage proves the glm-as-implementer security packet carries its high-severity clauses while the codex packet does not carry glm's clauses.

## Provenance

Distilled from the 2026-07-05 multi-vendor campaign: 9 PRs merged to main across glm/codex/grok/sonnet-5 lanes with kimi/sonnet/codex cross-vendor adversarial review. glm failed vault security 4/4 (#541/#583/#600/#607) and falsified a clippy-clean report (#610); codex implemented every frozen-spec surgical task correctly one-shot (#594/#607/#588). The hand-written cards (`lane_card_glm_5_2.md`, `lane_card_codex.md`) are this doc's precursor — this spec is how they stop being hand-written.

## Part II: historical model-router direction (owner discussion, 2026-07-05 night)

This section is retained as design provenance. The later #1467 product ruling leaves model/tool choice with the host model and retains only bounded staffing enforcement and evidence; do not execute Part II as a current product contract where the two conflict.

The end state is Tachi as a **model router**: given a task, Tachi picks the vendor AND ships the constraints that make that vendor safe for that task. This is the differentiator over benchmark-routers (OpenRouter/RouteLLM-class): they answer "which model is cheap and good"; they cannot answer "this vendor will falsify a clippy-clean checkbox under these conditions" — because benchmarks don't produce adjudication traces. Real dispatches + adversarial review do. **Router + immune system.**

### Card ontology (four fields)

1. **Capability** (擅长什么) — routing score per task-type; the hexagon's machine-readable form.
2. **Failure modes** (哪会出错) — error signatures + risk gates. High-severity domains change routing *topology*, not just vendor choice: security work routes to "mandatory dual-track + cross-vendor adversarial review," never merely to a different model.
3. **Constraint interface** (怎么约束) — the layered carriers, ordered by durability (proven 2026-07-05): prompt-layer (decays) < one-line config (`personality=pragmatic`) < packet clauses (per-dispatch injection from the card) < structural gates (scripts/CI — cannot be ignored).
4. **Constraint efficacy** (约束有效性) — the novel field: *which constraint layer actually works for which failure mode, per vendor*. Evidence: glm's false-`Closes` is NOT prompt-fixable (the adjudication comment sat on the issue; it violated it anyway) — only structural gates (leader independent verification + review gate) catch it. codex's parking urge WAS prompt-fixable (remap mandate=ledger onto the base prompt's own end-to-end vocabulary). Same disease class, different models need different medicine layers — this mapping is the card's most valuable content.

### Storage split (three layers, don't merge them)

- **Declaration** (vendor, lanes, tool whitelist, forbidden domains) → TOML seed files (serde-native, commentable, matches `config.toml`/`agents/*.toml` precedent).
- **Evidence** (eval rows, signatures, timestamps) → SQLite (append-only, temporal, `(vendor, role)`-queryable). Never a file.
- **Projection** (hexagon, current top-N clauses) → computed at render/assembly time, never persisted as truth. The hexagon is for human routing intuition; the machine consumes signatures and clauses.

TOML is the birth certificate, the DB is the medical record, the hexagon is the health report — the report is always computed from the record, never hand-edited.

### Hexagon axes (grown from evidence, not invented)

Six axes that actually discriminated lanes on 2026-07-05: **spec fidelity** (frozen-spec adherence vs. inventing alternatives), **self-report trust** (verbatim honesty vs. falsified CI), **test discipline** (unprompted discriminating tests), **equivalence-refactor strength**, **security-critical competence**, **speed/cost**. Reviewer role gets its own 3-axis mini-radar: precision / breadth / severity calibration (sonnet vs codex split exactly along these).

### Per-dispatch eval, run by the reasoning line (split in two)

Every completed dispatch produces an eval. It has two halves:
- **Mechanical facts** (test counts, CI conclusion, rework rounds, reviewer OK/CONCERN/BUG tally, wall-clock) — extracted deterministically from ledger/traces, zero LLM. *Scripts execute.*
- **Judgment distillation** (signature classification, per-axis scores, counter-clause proposals) — a reasoning-seat job (the labeler seat from the pattern-memory design) reading the **adjudication trace**. *Models judge.* Two guards: (1) proposal-mode — distilled signatures land as *proposed*; high-severity ones (`falsified_ci_report`-class) require leader ratification (reuse the `recall_proposals → review → apply` shape); (2) the seat transcribes the leader's already-made verdict into structure — it holds transcription rights, not judgment rights, so vendor overlap between the eval seat and the evaluated lane is not a conflict.

Runs as a foundry job post-`complete`, same family as daily_distill. One distill call per dispatch — cheap.

### The road from manual eval to auto-routing (three phases)

1. **Manual (now):** the leader's adjudications ARE the bootstrap labels — 2026-07-05's nine merged PRs are the first labeled dataset, not wasted effort.
2. **Semi-auto (#534 first slice + the eval line above):** traces distill to eval rows automatically; leader ratifies high-severity signatures.
3. **Auto-routing with confidence gates:** the router auto-dispatches only task-types where the card holds N+ evidence rows; below threshold it falls back to leader choice. **Cold start has a proven protocol — the first-exam** (Sonnet 5's #599, 2026-07-05): a standard graded exam slice, same-type A/B against an incumbent lane, verdict recorded to the card. New models sit the exam before they earn routing eligibility.
