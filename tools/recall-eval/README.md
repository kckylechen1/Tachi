# recall-eval — personal-corpus recall evaluation toolchain

Phase A tooling for the Recall Quality Architecture (`docs/engineering/
architecture/recall-quality-architecture.md`, tachi#708). Pure Python, **zero
kernel code**. It measures hybrid recall quality on the operator's *real*
memory corpus by driving the live daemon's `recall_simulate` facade, and
attributes misses to the doc's failure mechanisms (M1 proven, M2/M3 heuristic)
so Phase B/C/D scope is decided by numbers, not intuition.

Companion: `tools/zvec-shadow/` (the fair-corpus FTS BM25 sidecar, PR #700) —
this toolchain reuses its default-retrievability predicate and its sidecar as
the escape-gate comparison column.

## Why `recall_simulate` (and never `tachi search` for batches)

`recall_simulate` is the only live-DB read path used here because it:

- **bypasses the recall-cache short-circuit** — this workstation runs
  `TACHI_ENABLE_RECALL_CACHE=1`; a cached eval is a void eval. `recall_simulate`
  calls `search_memory_rows_with_recall_config` directly, never the
  cache-guarded handler.
- **shares the same server-side pipeline as `tachi search`** — the same
  `rows.rs` exclusions and the same `memory-core` quality multipliers (wiki
  ×1.15 etc.) apply inside the eval loop, so results are faithful.
- **does not mutate `access_count`** — `record_access` is hardcoded `false` in
  the simulate runner, so running the full matrix leaves the live corpus
  signal untouched. (`tachi search` bumps access counts — using it for batches
  would corrupt the very thing we measure.)

## Scripts

| script | does |
|---|---|
| `build_cases.py` | Read-only export from the live DB → ~40 stratified labeled cases → `cases.local.json`. |
| `run_matrix.py` | Drives the variant matrix via `recall_simulate` → `reports/matrix.local.jsonl` + `reports/aggregates.md`. |
| `attribute_misses.py` | Classifies each baseline miss to M1/M2_heuristic/M3_heuristic/OTHER (M1 counterfactually proven, M2/M3 are heuristics -- no config knob to re-run) → `reports/attribution.{local.jsonl,md}`. |
| `zvec_column.py` | Fires the same cases at the zvec-shadow sidecar for a comparison column → `reports/zvec_column.{local.jsonl,md}`. |
| `mcp_client.py` | Shared streamable-HTTP MCP client (single source of truth for the transport). |

### `build_cases.py`

Candidate pool = the **exact** default-retrievable set `tachi search` returns.
We import `tools/zvec-shadow/export_snapshot.py`'s `tachi_default_retrievable()`
**verbatim** (no home-grown filter) so a labeled `expected_id` is always
something a default recall could actually surface. No embeddings are read, so
its `sqlite_vec` dependency is stubbed out.

Each case's `expected_id` is a memory's own id; the query is derived from that
memory. Slices stress the doc's mechanisms:

- `summary_en` / `summary_mixed` / `summary_cjk` — summary-derived queries.
  **`summary_cjk` is the M1 discriminator**: FTS conjunctive all-or-nothing +
  the CJK-blocked OR fallback should starve pure-CJK queries on current main
  (the known 8/20-class red baseline).
- `keyword_bag` / `keyword_bag_cjk` — multi-token keyword-bag queries; stress
  the lexical/symbolic channels and coverage.

**Query-construction fix (the compare.py truncation bug):** `compare.py`
truncated with a raw `summary[:60]` char slice, which splits an ASCII word
mid-token (`".. architec"`) and hands FTS a garbage prefix term.
`truncate_word_boundary()` here truncates on a **word boundary** — ASCII words
are kept whole; CJK is per-char (libsimple segmentation) so a cut between CJK
chars is a legitimate token boundary.

```
python3 build_cases.py                 # ~40 cases with default quotas
python3 build_cases.py --smoke 3 --out cases.smoke.local.json   # 3-case smoke
```

### `run_matrix.py`

Variant matrix (architecture doc §4.4):

- `current` — baseline config (always returned by `recall_simulate`).
- `or_fallback_055` — `or_fallback_fts_score_factor = 0.55` (the M1 fix).
- `fts_heavier` — reweights the default + events/notes groups toward FTS.
- `enable_rerank` — a **separate pass** (rerank is request-global, not
  per-variant), baseline config. `recall_simulate` exposes a per-variant
  `rerank.policy_counts` map (`crates/memory-server/src/facade_memory_ops/
  recall_simulate_ops/runner.rs:184-187`, backed by `SearchRerankPolicy` in
  `crates/memory-server/src/memory_search_ops/rerank.rs:9-30`), so this row
  reports how many cases actually got `applied` vs `not_needed` /
  `score_gap_too_wide` / `fallback` / `skipped_exact_token` — proof rerank
  participated in ranking, not just that the request carried
  `enable_rerank=true`. If a future kernel response shape drops that field,
  the aggregate prints `RERANK_POLICY: UNAVAILABLE_IN_RESPONSE (kernel gap)`
  instead of silently claiming rerank ran.
- `rrf_k_10` — **UNAVAILABLE, recorded not faked.** `rrf_k` is a hardcoded
  local constant in `scorer.rs` (`let rrf_k = 60.0;`), *not* a `RecallConfig`
  field, so `recall_simulate` variants cannot reach it. Measuring it requires
  first promoting `rrf_k` to a real config knob (a Phase-B-adjacent change).

Each non-`current` variant's `config_env` diff is checked to be non-empty — a
mistyped override field is silently dropped by `serde(default)`, so an empty
diff (flagged ⚠) means the override never took effect.

```
python3 run_matrix.py --cases cases.local.json
```

### `attribute_misses.py`

`recall_simulate`'s returned rows expose only the final fused `relevance`; the
per-channel `scores`/`match_type` fields are null on the normal search path
(populated only for exact-token rows). So attribution is **counterfactual** —
which concrete fix or structural fact recovers/explains each miss — which is
what Phase B/C/D need to know anyway. Per baseline miss:

- `or_fb_rank` — rank under `or_fallback=0.55` (top-10).
- `fts_rank` — rank under the FTS-heavier weighting (top-10).
- `wide_rank` — rank under baseline config at `top_k=100` (candidate visibility).
- `wiki_above` — `/wiki` rows outranking the target inside that top-100.

Buckets (single primary, priority order). **Only M1 is counterfactually
proven** — `or_fallback_055` is a real `RecallConfig` knob, so the case is
actually re-run under it and observed to climb into top-10. `rrf_k` and the
wiki quality-multiplier have **no config knob** to dial down and re-run
(`rrf_k` is a hardcoded local constant in `scorer.rs`; see `run_matrix.py`'s
`rrf_k_10` UNAVAILABLE row), so `M2_heuristic` and `M3_heuristic` are
proximity heuristics inferred by elimination, not measured mechanisms — treat
their counts as hypotheses for Phase B/C/D scoping, not proof of which fix
to make:

- **M1** — OR-fallback recovers the target into top-10 → FTS all-or-nothing /
  CJK-blocked fallback was the block. Counterfactually proven.
- **OTHER** — target absent even from top-100 → candidate starvation / filter
  misfire (the doc §6 "fourth mechanism" watch).
- **M3_heuristic** — target is a top-100 candidate and enough `/wiki` rows
  outrank it that removing them would lift it into top-10
  (`wiki_above >= wide_rank - 10`). No counterfactual: the wiki boost was
  never actually dialed down and re-run.
- **M2_heuristic** — target is a top-100 candidate but ranked out with no wiki
  explanation → attributed by elimination to RRF k=60 rank-flattening. No
  counterfactual: `rrf_k` was never actually lowered and re-run.

```
python3 attribute_misses.py --cases cases.local.json
```

### zvec comparison column

Stand up the existing sidecar (one-time; all local, no paid API), then align it
to the same cases:

```
cd ../zvec-shadow
pip install -r requirements.txt                       # sqlite-vec, zvec, numpy
python3 export_snapshot.py --out snapshot.jsonl        # read-only DB export
python3 sidecar.py --snapshot snapshot.jsonl --port 8791 &
cd ../recall-eval
python3 zvec_column.py --cases cases.local.json --sidecar-url http://127.0.0.1:8791
```

The sidecar snapshot must come from the same DB `build_cases.py` read (both use
the identical exclusion set), so the `expected_id`s exist in the sidecar corpus.
If the repaired Tachi FTS channel still systematically loses to the sidecar's
BM25 here, that trips the doc §3 escape gate toward #683.

## Full workflow

```
pip install -r requirements.txt
python3 build_cases.py                              # -> cases.local.json
python3 run_matrix.py --cases cases.local.json      # -> reports/aggregates.md
python3 attribute_misses.py --cases cases.local.json# -> reports/attribution.md
# optional escape-gate column:
python3 zvec_column.py --cases cases.local.json
```

Then hand-lift the aggregate numbers from `reports/*.md` into the umbrella
issue (#708). Nothing under `reports/` or `cases*.local.json` is committed.

## Privacy line

The toolchain reads the operator's real `~/.tachi/global/memory.db`. Cases and
report detail embed real query text and memory ids and are **gitignored**
(`cases*.local.json`, `reports/*.jsonl`, `reports/*.md` — mirrors
`tools/zvec-shadow/.gitignore`, PR #700 precedent). Only aggregate numbers —
hit@10, recall@k, MRR, bucket/slice counts — ever leave the machine, and only
by a human copying them into the issue. Scripts print aggregate-only stdout.

## MCP transport notes (verified live against daemon :6919)

`mcp_client.py` handles three rmcp streamable-HTTP quirks: the `initialize`
handshake mints an `mcp-session-id` echoed on every later call; responses are
`text/event-stream` that `requests` mis-decodes as ISO-8859-1 (we decode raw
bytes as UTF-8); and the JSON-RPC envelope carries literal newlines inside
string values (memory text), so it's extracted with a string-aware
balanced-brace scan and parsed with `strict=False`.
