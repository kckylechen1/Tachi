# Experience → Tachi: the vendor-keyed card-evolution loop (#534 first cut)

**Status:** design, implementable. Freezes the first slice of #534 (card evolution) from the 2026-07-05 dispatch campaign.

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

## What is built vs. the missing wire

**Built:** `dispatch_profile/cards` (loadout / overlay / evolution machinery), `complete` (per-subagent eval rows), `safe_merge`, `tachi_gh`. The overlay already *projects* skills into a profile.

**Missing — the wire this doc specs:**
1. A **vendor axis** on cards. Cards today are role-keyed (reviewer / implementer); they need a `(role, vendor)` key so glm-as-implementer and codex-as-implementer carry different overlays.
2. **Error-signature extraction** from adjudication traces. `complete` records an eval row; it does not yet distill a typed `error_signature` from the adjudication verdict.
3. **Counter-clause projection** into the packet. The overlay projects skills today; extend it to project the top-N counter-clauses for `(role, vendor)` into the dispatch packet's frozen-spec section.

## First-cut slice (implementable, minimal end-to-end)

Ship the smallest loop that makes "failure → next dispatch auto-carries the vaccine" real:

**(a) On adjudication, record a vendor-keyed error signature.**
Extend `complete` (or a new `record_signature` verb) so a closed dispatch carries `{vendor, role, signature, severity, evidence_ref}`. Signature is drawn from a frozen taxonomy (below). Stored on the `(role, vendor)` card as accumulating evidence with a timestamp.

**(b) At packet assembly, project the top-N counter-clauses.**
When assembling a dispatch packet for `(role, vendor)`, look up that card's active signatures, map each to its counter-clause, ACT-R-decay by recency/frequency, and inject the top-N into the packet's frozen-spec section — verbatim, as additional mandatory clauses. Provisional N=3; calibrate from telemetry (never a flat magic number — per the frozen-spec law).

**(c) ACT-R decay** so a signature that stops recurring fades (already the design intent of the overlay evolution). A vendor that improves sheds its counter-clauses over time; one that keeps failing accumulates them.

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

- [ ] A closed dispatch can record a `(vendor, role, signature)` row against a card via a facade verb.
- [ ] Packet assembly for `(role, vendor)` projects that card's top-N counter-clauses (verbatim from the frozen taxonomy) into the frozen-spec section.
- [ ] Projection is ACT-R-decayed by recency/frequency; a vendor with no recent signatures gets a clean packet.
- [ ] `falsified_ci_report` sets a per-vendor `self_report_trust=low` flag that the leader/pipeline reads (independent-verify-everything).
- [ ] The 07-05 taxonomy is seeded; glm's card carries `fake_security_fix` ×4 + `falsified_ci_report` ×1 + `self_close_overreach` ×3; codex's carries `parking_after_contract` / `breadcrumb_violation` (already vaccinated via constitution — record as *resolved* signatures to prove decay).
- [ ] Discrimination check: a dispatch to glm-as-implementer on a security issue produces a packet containing the three high-severity counter-clauses; the same dispatch to codex does not.

## Provenance

Distilled from the 2026-07-05 multi-vendor campaign: 9 PRs merged to main across glm/codex/grok/sonnet-5 lanes with kimi/sonnet/codex cross-vendor adversarial review. glm failed vault security 4/4 (#541/#583/#600/#607) and falsified a clippy-clean report (#610); codex implemented every frozen-spec surgical task correctly one-shot (#594/#607/#588). The hand-written cards (`lane_card_glm_5_2.md`, `lane_card_codex.md`) are this doc's precursor — this spec is how they stop being hand-written.
