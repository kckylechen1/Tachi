---
title: "Recall Quality Architecture"
summary: "Evidence-gated repair of hybrid recall's precision failure (coverage-graded lexical channel, full-coverage boost, rerank promotion) plus a permanent golden-corpus evaluation system that gates every future recall change."
category: "engineering/architecture"
organize: true
---
# Recall Quality Architecture

Owner-approved design (2026-07-06). Companion evidence: PR #700 fair-corpus eval
(tachi hit@10 = 8/20 vs zvec sidecar FTS-only 20/20), same-day pipeline autopsy
(this doc's §1). Recall is the memory kernel's core competence; this spec fixes
its measured precision failure and installs the harness that prevents relapse.

## 1. Verified failure mechanisms (2026-07-06 autopsy)

All claims verified against source the same day; line refs may drift.

| # | Mechanism | Evidence | Status |
|---|---|---|---|
| M1 | **FTS channel is all-or-nothing.** Primary query runs `simple_query()` conjunctive semantics; the OR-fallback ships disabled (`DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR = 0.0`, `recall_config.rs:12`) and its term filter admits ASCII only (`expansion.rs:120`), so **CJK queries can never trigger the fallback** even when enabled. One missing/truncated token zeroes the whole channel for the target. | code + eval | CONFIRMED |
| M2 | **RRF k=60 flattens rank differences.** Rank-1 vs rank-20 differ ~30%; cross-channel agreement dominates single-channel excellence. A single-channel rank-1 precise hit scores ≈0.004 while a three-channel middling row scores ≈0.014. Precision queries (one right answer) structurally lose to broad wordy rows. | `scorer.rs:327-355` | CONFIRMED |
| M3 | **Wiki rows get ×1.15 quality boost in unscoped search** (guide ×1.12) — the "wiki dilution" has a concrete multiplier. | `filtering.rs:119-149` | CONFIRMED |
| M4 | "Decay suppresses old rows" — in RRF mode decay is an additive bonus ≤ ~0.003 (`weights.decay × ds / rrf_k`), incapable of suppressing anything. | `scorer.rs:355` | **DISPROVEN** |

Why the zvec sidecar scored 20/20 with FTS alone: its lexical retrieval is
BM25-ranked-OR — coverage is rewarded, not required. That property, not the
engine, is the biggest single lever — and SQLite FTS5 provides it natively.

## 2. Principles

1. **Don't replace the hybrid engine; give it a precision floor.** When all of
   a query's tokens are present in one entry, that entry belongs in top-k.
2. **The harness gates everything.** No recall config default or scoring change
   ships without golden-corpus evidence via `recall_simulate` → `recall_proposals`
   → `apply_recall_proposals` (the loop already exists; this spec makes it law).
3. **Every new threshold is a provisional config knob** (flat-magic-number
   clause): `recall_config` + `TACHI_RECALL_*` env, calibrated by eval.
4. **Attribution numbers, not mechanism intuition, order the fixes.** Phase A's
   variant matrix decides Phase B/C/D/E scope — phases shrink or die if the
   data says so.

## 3. Not building (re-examined 2026-07-06 against current state)

- **zvec as candidate engine — not in this spec, WITH an escape gate.** The
  sidecar's win was ranked-OR BM25, which Phase B delivers in-process with zero
  new dependency. zvec's real differentiators (0.26ms dense, scale, one-engine
  hybrid) stay on #683's evidence-gated track behind the writer-queue seam.
  **Escape gate:** Phase A's matrix keeps a zvec-sidecar comparison column; if
  the REPAIRED FTS channel still systematically loses to zvec BM25 (e.g.
  tokenization quality), stop polishing FTS5 and escalate #683 instead.
- **Decay/half-life changes** — M4 disproven; untouched.
- **Query-side LLM rewriting as a default path** — per-search latency/cost on
  the wrong side of the asymmetry. See Phase E for the write-side realization;
  ask/deep-recall surfaces may take an opt-in rewrite param later.

Re-examined and **promoted** (previously under-weighted on stale memory):

- **Voyage rerank**: built, probe-green, adaptive-gated (fires only when top
  hybrid scores are close), exposed as `enable_rerank` end-to-end. It reorders
  the candidate pool — and the symbolic wide net (~10× candidates) means the
  target is usually IN the pool, so it is directly on-target for M2. Promoted
  to a first-class Phase A variant and a candidate default-on (interactive
  surfaces first). Sober limits: external-API dependency (needs keyless/offline
  degradation) and it cannot rescue a row that never entered the candidate set
  — rerank is fast relief, Phase B is the cure. **Prerequisite for default-on:
  #568 secret-pattern scrub** (rerank ships query+doc text to an external API).

## 4. Phases (each independently mergeable and useful)

### Phase A — Golden-corpus evaluation system (no kernel code)

1. **Synthetic corpus fixture in-repo**: seed builder (~60 entries: zh/en
   mixed, wiki/guide/plain, fresh/old, entities/keywords) + ~50 labeled queries
   (summary-derived, keyword-bag, id-like, pure-CJK, wiki-scoped) with
   recall@10 / MRR floor assertions. **Discriminating**: current main must fail
   the known-8/20-class slices (red baseline recorded, floors ratchet upward
   with each landed phase).
2. **Personal corpus**: real-DB labeled cases encoded as `recall_simulate`
   cases stored under the `/eval` namespace (already excluded from normal
   recall); runs in the daily pipeline; regressions surface as `tachi_status`
   health deductions. Privacy line: personal cases never leave the machine
   (PR #700 precedent).
3. **Pipeline verifications (acceptance items, not open questions)**:
   `recall_simulate` reads must bypass `recall_cache` (this workstation runs
   `TACHI_ENABLE_RECALL_CACHE=1`; a cached eval is a void eval), and must
   exercise the same server-side path as `tachi search` (rows.rs exclusions +
   quality multipliers inside the eval loop).
4. **First variant matrix run** (attribution decides everything downstream):
   baseline / `or_fallback=0.55` / fts-heavier weights / `rrf_k=10` /
   **`enable_rerank=true`** / **zvec-sidecar comparison column** (tools/zvec-shadow).

### Phase B — Coverage-graded lexical channel

- Fix the OR-fallback CJK exclusion (`expansion.rs:120`): ASCII tokens keep
  `term*` OR; contiguous CJK runs join as quoted phrases (matches libsimple
  segmentation; per-char OR would over-match) — final granularity (phrase vs
  bigram) decided by Phase A data.
- Flip `or_fallback_fts_score_factor` default 0.0 → eval-calibrated value
  (starting point 0.55, provisional knob).
- Gate: affected synthetic slices improve materially; zero regression elsewhere.

### Phase C — Full-coverage precision boost

- Ride the existing `generic_precision_multiplier` path (RRF-clamp semantics
  already built for the id-like boost): `symbolic_score ≥ coverage_threshold`
  (provisional 0.85) → `full_coverage_boost` (provisional 4.0; id-like is 12.0).
  Both knobs in `recall_config` + env.
- **Self-shrink clause**: if Phase A shows the or_fallback variant alone
  recovers most misses, this phase shrinks or dies.

### Phase D — Wiki boost scoping (eval decides existence)

- Unscoped search: wiki ×1.15 → 1.0; keep 1.15 only when `path_prefix` targets
  /wiki. Skipped entirely if Phase A attribution shows wiki dilution is noise.

### Phase E — Write-side enrichment (candidate; the cheap side of the asymmetry)

- At save time, foundry asynchronously generates synonym/bilingual keywords
  into `keywords` — one-time cost per memory, permanently widens the FTS and
  symbolic match surface, zero query-time latency. Uses existing enrichment
  plumbing. **Prerequisite: #568 scrub** (memory text goes to an external LLM).
- Gate: same golden corpus; also measure index bloat and noise-keyword harm.

### If rerank wins Phase A

Adaptive rerank default-on for interactive surfaces is a config-only ship and
may land ahead of Phase B — gated on #568 and a documented keyless/offline
degradation path.

## 5. Scheduling constraint

Phases B/C/D are small surgical diffs on `scorer.rs`/`search/` — the same hot
zone as the #501 memory-server split and the #697 convergence surface. They
must land **entirely before or entirely after** #501's restructure, never
interleaved. Recommendation: before (tiny diffs, high value, and they give
#501 a regression net). Phase A touches no kernel code and starts immediately
without violating the debt-first ruling.

## 6. Premise collapse

This spec assumes the 12/20 misses decompose into M1+M2+M3. If Phase A's
matrix shows misses persisting with or_fallback ON + rerank ON + weight
variants, a fourth mechanism exists (candidate starvation? server-side filter
misfire?) — return to diagnosis. The design survives: Phase A stands alone as
regression infrastructure, and each later phase carries its own gate.

## 7. Verification & rollback

- Phase A: synthetic suite must be red on current main (self-proving
  discrimination); cache-bypass proven; matrix report archived to the umbrella
  issue.
- Phases B-E: synthetic green + personal recall@10/MRR non-regressing + fmt /
  clippy -D warnings / full suite + discrimination (new assertions fail on
  prior phase's code).
- Rollback: config knobs + small functions; clean `git revert`; no data
  migration anywhere.

## 8. Related

- #683 zvec adoption (shares this harness; escape gate feeds it)
- #568 secret scrub (prerequisite for rerank-default and Phase E)
- #702 malformed response shape under contention (must be fixed or worked
  around before Phase A's personal-corpus automation trusts CLI output)
- #701 workspace scope routing (eval harness must pin project/global DB
  explicitly — the compare.py PWD lesson)
- Quant #769 (downstream deployment shows the same class of symptoms; consumes
  these fixes post-convergence per #697)
