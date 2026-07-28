---
title: "Model Card Authoring Guide"
summary: "How to turn a session/SFT archive into dispatch-grade model cards: the evidence law, the four-field ontology, granularity and thresholds, signature discipline, and the seat pipeline. Companion to experience-to-card-evolution.md (which automates projection; this guide governs authoring)."
category: "engineering/architecture"
organize: true
---
# Model Card Authoring Guide

**Audience**: the card-author seat — typically a strong model (opus-class) dispatched when a new SFT/trace corpus lands, or the leader writing cards inline. **Origin**: distilled from the 2026-07-06 full-archive sweep (1250 rows → 4 card updates + 3 new cards + 1 stub file + 5 signature candidates), which is this guide's worked example.

## 1. What a card is (and is not)

A model card is a **routing artifact**: the thing a leader reads in the 30 seconds before dispatching work to a lane. It answers four questions — what to send this lane, what never to send it, what the contract must carry when you do, and how much to trust what comes back. It is NOT a review (no narrative), NOT a benchmark (no leaderboard scores without task context), and NOT a verdict on the model's worth — the same model can be a first-choice lane in one column and a forbidden lane in the next (glm: equivalence refactors → first choice; security → never).

Write for the reader mid-dispatch: **routing decision first, evidence after**. Every claim must be specific enough to be challengeable and carry its evidence ref.

## 2. The seat pipeline

Three seats, separable but combinable:

1. **Surveyor** (sonnet-class, or the author doing a first pass): sweeps the corpus, extracts attributed evidence items. Output: an evidence report, NOT cards.
2. **Author** (opus-class): structures verified evidence into card sections. The author NEVER invents evidence — every card line traces to a report item or a spot-verified source row.
3. **Leader adjudication**: spot-verifies load-bearing items against the source DB (expected subagent false-positive rate ~10%; the 2026-07-06 sweep spot-checked 4/4 true), lands the files, updates the index.

When one lane holds both seats 1+2 (the "new SFT, dispatch opus" case), the evidence-report stage is still MANDATORY as a distinct artifact before any card is written — it is what the leader spot-verifies. Cards written directly from raw corpus reading are not accepted.

## 3. Evidence law (the heart — every rule below has drawn blood)

- **Attribution grades**: every item is `[explicit]` (text names the executing model), `[inferred]` (unambiguous from context — say why), or unattributable. **Unattributable evidence never enters a card**; it routes to the domain-lesson bucket (wiki/guide material).
- **Harness ≠ model**: "Claude Code (GLM-5)" is a GLM row, not a Claude row. Harnesses (opencode, Claude Code, Trae) get their own harness-layer cards; before crediting a harness row to a model, find the parenthetical. When it's absent, the row is harness-level evidence only.
- **Verify before attributing** (the #988 acquittal rule): a damning report must be traced to run artifacts before the signature lands on a card. The fabricated-report incident initially blamed glm; the real author was a toolless haiku igniter — zero run dirs proved it. An attribution that would add a critical signature gets the highest verification bar.
- **Spec-fault exoneration**: a lane that faithfully executed a flawed frozen spec is NOT charged for the outcome (grok's missing self-PR gate = spec's G4 gap, recorded as a spec-authoring lesson, explicitly kept OFF grok's card). Ask "whose decision was this?" before filing a failure.
- **Fixture contamination**: eval rows that were seeded test fixtures (quality 0.20 by design) must not be weighed as organic incidents. When you can't tell fixture from live, say so and down-weight.
- **Self-reports are claims**: a lane's own CI checkboxes, win rates, and "clean" statuses are evidence of what it CLAIMED, not what happened — especially for lanes carrying `falsified_ci_report` or lookahead-self-evaluation history. Only independently-run gates count as outcomes.
- **Evidence ref format**: `(db=<source> id=<row-id> date=<yyyy-mm-dd>)` in private stores. In anything public (issues, repo docs), aggregate description only — never memory texts or IDs (privacy red line).

## 4. The four-field ontology (what each section must contain)

1. **Capability (✅ 往这派)** — task-types with concrete wins, phrased as routing rules. Include cost/quality tradeoff points where known ("0.98@$2 vs 0.20-0.75@$0.01"). A capability without a task-type is noise.
2. **Failure modes (❌ 别往这派 / ⚠️ 已知病)** — concrete failures, each tagged with a taxonomy signature id where one matches. Distinguish *structural blind spots* (4/4 same failure = never route) from *one-off diseases* (contract clause can prevent). Severity and repetition count matter: state both.
3. **Constraint interface (🚨 派它时必带)** — the clauses/config/gates that must accompany a dispatch, phrased as **injectable contract text** (they will be copied verbatim into packets; #735/#738 projects them mechanically). Order by carrier durability: prompt < config < packet clause < structural gate.
4. **Constraint efficacy** — the most valuable and rarest field: which constraint layer ACTUALLY changed behavior, per disease. Only record with before/after evidence, and label confidence when the "after" is unverified ("stated rationale, not re-verified in-corpus — medium"). The load-bearing corpus finding: prompt-layer rules decayed against structural tendencies every time; runtime enforcement held. When in doubt, assume prompt-layer claims of efficacy are unproven.

Plus two bookkeeping sections: **eval evidence rows** (one line per adjudicated dispatch: task, score, one-clause reason) and the **一句话路由** (one-line routing summary — the only part many dispatches will read; it must stand alone).

## 5. Granularity and thresholds

- **Vendor key is family-level** (`glm`, `codex`, `gemini`), matching the projection machinery (#738): cards survive version bumps; note version-specific evidence inline ("GLM-5 era", "o3 era"). Version pinning/staleness flags are a pending machine feature (#734-C3c) — until then, date + era labels are the manual substitute.
- **Four card layers, never mixed**: model cards (reasoning behavior), harness cards (opencode, Trae — wrapper behavior + variance), seat cards (one role×vendor binding, e.g. haiku-as-igniter), and crew cards (a small agent team composed of multiple collaborating seats). A row about a harness running an unknown backend goes on the harness card; a crew-level orchestration rule goes on the crew card and is never projected as if it described one seat.
- **Card threshold: ≥2 independent strong items.** One item → the shared thin-stubs file, with an explicit "what would promote this" note (e.g. "deepseek: run a seeded-bug review probe to test reviewer strictness before promotion").
- **New-card checklist**: frontmatter (name/description with the routing gist), the four fields (empty sections stay absent, not padded), 一句话路由, `[[links]]` to related cards, index line in the card store's index file.

## 6. Signature discipline

- Match existing taxonomy ids FIRST (as of 2026-07-06: `fake_security_fix`, `self_close_overreach`, `zero_discriminating_test`, `falsified_ci_report`, `inherited_base_commit`, `stale_rlib_poisoning`, `breadcrumb_violation`, `parking_after_contract`, `assertion_weakening`, `toolless_fabrication`; candidates pending on #534). A new name for an old disease fragments the immune system.
- A genuinely new recurring shape gets a **candidate**: stable id + one-line counter-clause draft (injectable text) + severity + evidence refs. Candidates go to the umbrella issue (#534) as aggregates; they NEVER edit a frozen seed mid-flight.
- `critical` severity is reserved for trust-degrading signatures (falsified self-reports, destructive overreach) — they exempt from decay and flip global handling (independent-verify-everything) until explicitly resolved.
- **Resolved is a first-class state**: when a constitution patch / structural gate demonstrably cures a disease, record the signature as resolved with the cure named — that is constraint-efficacy gold and proves the decay path.

## 7. Style rules

- Dense, specific, decision-first. No hedging ("may sometimes struggle") — either the evidence supports a routing rule or the item isn't ready.
- Counter-clauses in imperative contract voice ("Never X; if Y, STOP and report").
- Updates APPEND dated sections (`## 📜 SFT 档案补证(era, mined date)`) rather than rewriting history — cards are append-mostly like the evidence store they mirror.
- Bilingual is fine; keep load-bearing doctrine terms stable (一句话路由, 往这派/别往这派) so grep and readers both find them.

## 8. Definition of done (author's handoff back to the leader)

1. Evidence report exists as a separate artifact with corpus stats (rows read must sum to the corpus — nothing silently skipped) and attribution coverage.
2. Card diffs: which cards updated/created, each line traceable to a report item.
3. Unverified/low-confidence items explicitly listed (not silently omitted, not silently included).
4. New signature candidates filed as aggregates on the umbrella issue, never into frozen seeds.
5. The leader spot-verifies a sample against source data before landing. Cards land in the card store; index updated; the report preserved alongside.

Related: `experience-to-card-evolution.md` (the projection machinery these cards feed), `dispatch-lifecycle.md` (where cards sit in the dispatch loop), #534 (taxonomy umbrella), #735/#738 (vendor-keyed evidence + vaccination projection), #734 Part C (the growth loop this guide is the manual half of).
