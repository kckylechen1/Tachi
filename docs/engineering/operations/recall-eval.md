# Personal Recall Eval

`tachi eval recall` is the native operator gate for the private personal recall
corpus. It runs labeled `/eval` cases through the retained `recall_simulate` core
in `memory_search_ops`; the retired `tachi_tune` MCP facade and proposal handlers
are removed. Replay bypasses recall-cache short-circuiting and does not mutate
memory access counters.

## Corpus Format

Store local-only labeled cases as memory rows under `/eval/...`. The row
`metadata` must contain either a `recall_eval` object or the fields directly:

```json
{
  "recall_eval": {
    "query": "private query text",
    "expected_id": "target-memory-id",
    "slice": "summary_cjk",
    "path_prefix": "/notes"
  }
}
```

Supported fields are `query`, `expected_id`, `expected_ids`, `slice`, `scope`,
`project`, `domain`, `path_prefix`, `as_of`, `top_k`, `label_kind`, `expectation`,
`forbidden_ids`, and `enabled`. Set
`enabled=false` to keep a case in the DB but exclude it from the gate.

| Expectation | Contract | Metric treatment |
| --- | --- | --- |
| `any` (default) | At least one expected ID appears within the case's top-k | Positive Recall@k and MRR@k use the first matching exact ID; aggregate floors decide tolerated misses |
| `all` | Every expected ID appears within top-k | Same positive metrics, plus a mandatory contract failure if any required evidence is missing |
| `empty` | No rows returned; expected IDs must be absent | Separate negative denominator; any returned row fails the gate |

`forbidden_ids` adds a mandatory exclusion to any case. An ID cannot be both
expected and forbidden. Search errors never satisfy an empty expectation.
Matching is exact-ID-only; lineage-aware coverage remains in
`memcore::recall_coverage` and is not silently substituted here.

## Run

```bash
tachi eval recall
tachi eval recall --min-recall 0.95 --min-mrr 0.5 --json
tachi eval recall --cases reviewed-cases.json --reviewed-only --min-recall 1 --min-mrr 0.9 --json
```

The command writes `$TACHI_HOME/status/recall_eval.latest.json` and exits
non-zero when the configured recall/MRR gate fails. `tachi status --json` and
the `tachi_status` MCP surface expose that latest aggregate health under
`recall_eval`.

The status artifact is aggregate-only. It intentionally omits raw queries,
expected ids, returned ids, and per-case result detail.

## Labels and ranking diagnostics

Use `label_kind: "reviewed"` for cases whose expected answer has been checked
against source evidence. This is an operator-supplied evaluation label, not a
Wiki approval or a machine attestation. Missing labels remain `unspecified`.
Automatic capture records `label_kind: "weak"`: its expected ID is the first
eligible search hit, not an independently verified answer. Legacy capture
markers (`auto_captured`, `source: "record_access"`,
`signal: "recalled_and_used"`, or `slice: "auto_capture"`) also classify a case
as weak, even if an inconsistent reviewed label is supplied. Create a separate
source-checked case for the reviewed corpus rather than relabeling a capture.

Without `--reviewed-only`, the gate retains `all_loaded_cases` behavior. With
that flag, only source-reviewed cases contribute to the gate, and an explicit
positive `--min-mrr` ranking floor is required. Weak captures stay weak even
when they carry a conflicting reviewed label. Keep unseen queries separate from
tuning queries; an operator label is not independent answer-quality evidence.

Status exposes selected `label_counts`, `loaded_label_counts`, `excluded_by_label`,
`skipped_rows`, and `disabled_rows`, including through `tachi_status`. An empty
selection is `incomplete`. Reviewed mode also fails incomplete when malformed
DB corpus rows were skipped; explicitly disabled rows are reported separately.
Invalid files, contradictory labels, and replay failures replace stale green
status with `error`. Every non-`ok` status exits nonzero. Positive metric floors
and mandatory all/empty/forbidden contracts must all pass; negative-only corpora
report null Recall/MRR rather than a manufactured perfect positive score.

Replay and aggregate status also report `recall_at_1` and `recall_at_3`.
The latter uses only cases requesting `top_k >= 3`, with its denominator in
`recall_at_3_case_count`; it is null when none qualify. A top-one replay cannot
establish recall at three. `mrr_at_k` makes the existing MRR cutoff explicit;
`rank_cutoffs` lists effective per-case cutoffs when a corpus mixes top-k values.
Positive metric denominators exclude empty-expectation cases. Baseline search
errors are excluded from rerank flip counts and fail the CLI gate.

The 50-query golden corpus now measures the production-returned order without
a test-side re-sort, with MRR truncated at 10. Existing ranking and Recall@10
floors remain unchanged. Its ignored reporting helper prints production and
legacy re-sorted ranks side by side to make this metric migration inspectable:

```bash
cargo test -p memcore golden_corpus -- --nocapture
cargo test -p memcore golden_corpus_report -- --ignored --nocapture
cargo test -p tachi-server --test http_dogfood_recall_eval
```

For ordinary Memory queries, `tachi_memory(action="search", format="full")`
returns JSON with row metadata and `rerank_diagnostics`; compact/default JSON
keeps its existing payload. Internal search handlers attach the same diagnostics
when metadata is requested. Diagnostics contain the actual adaptive
gate policy, post-filter candidate count, requested top-k at that gate, and the
top-first minus top-third score gap (null with fewer than three candidates).
The facade can request a wider intermediate pool than its final response size.
These counts describe the rerank input, not the underlying vector/lexical pool.
Empty results carry no row diagnostics; replay still reports its per-case policy.
Cache hits retain the diagnostics of the cached computation, not a new rerank run.

The local CLI's `recall_simulate` core provides per-case ranks, returned source DBs, scores,
rerank policy counts and baseline rank flips. Its per-case `rerank` now also
contains the same input score gap and requested top-k. Detailed replay remains
operator-scoped; the shared latest-status artifact stays aggregate-only.
Replay's `returned[].scores` is populated from the search row's canonical
`score` object, and `returned[].source` retains its source label for analysis.

## Cache-aware latency measurements

### Private run receipts and paired gates

`--report <new-path>` retains a private `tachi.recall_eval.run.v1` receipt with
binary SHA-256 and build revision, a canonical input digest, source labels,
effective requests, complete RecallConfig, routing and provider selectors,
store generations before/after, per-case returned evidence and scores,
rerank input IDs, ranks, and elapsed replay time. The receipt uses create-only
publication and owner-only Unix permissions. Raw queries and IDs belong in
this explicit local file; the shared latest status remains aggregate-only.

`--compare <baseline-receipt>` requires `--report`. It refuses a different corpus,
label policy, cutoff, threshold, store generation, routing configuration or
effective request. Case errors cannot become miss→hit improvements. Code and
RecallConfig may differ as the experiment's treatment. Any previously passing
contract that fails, hit→miss, or worse first-evidence rank fails the paired
gate even if the overall Recall/MRR floor still passes. Per-case differences
remain in the private receipt.

The first automatic paired gate is deliberately scoped to **offline lexical
replay**: query embeddings must be disabled and rerank must be off in both runs.
Online embedding/rerank runs can produce diagnostic receipts, but automatic
comparison returns `incomplete` until provider execution evidence is available.
Provider/model fields are configured selectors, not attestations of a successful
model call. A local rerank server's model is unknown to this client.

Run both binaries on the **same stationary copy** of the stores with writers
stopped. Generation identity includes physical DB identity; another copy or
another host is not automatically comparable. This is a mutation guard, not a
portable content hash or a database backup. Use the existing backup mechanism
to prepare a private fixture; do not run calibration against the live store.

```bash
TACHI_SEARCH_DISABLE_QUERY_EMBEDDING=1 /path/to/baseline/tachi eval recall \
  --cases reviewed-cases.json --reviewed-only --min-recall 1 --min-mrr 0.9 \
  --report baseline-1.json --json
TACHI_SEARCH_DISABLE_QUERY_EMBEDDING=1 /path/to/candidate/tachi eval recall \
  --cases reviewed-cases.json --reviewed-only --min-recall 1 --min-mrr 0.9 \
  --compare baseline-1.json --report candidate-1.json --json
```

Both binaries must contain the receipt instrumentation. Establish the baseline
after this instrumentation lands, then compare future ranking candidates.
Repeat at least three times with new output paths and retain every run. Report
the observed range and individual regressions; do not replace the baseline with
the best run. `as_of` freezes visibility filtering, **not wall-clock decay**.
Elapsed time covers the current search/rerank replay, excluding the optional
no-rerank diagnostic baseline, hashing and receipt writes. It is a single-run
observation, not a p95 or provider latency estimate.

This receipt measures retrieval through the native replay path. End-to-end
answer synthesis, citation correctness, full host context budgets, real-account
provider behavior, and the external answer benchmarks still require their own
acceptance evidence.

### Cache layers

Keep recall quality and latency measurements separate. The replay above bypasses
the stored result cache and access-counter writes, but it can still reuse Voyage
query embeddings through the same `LlmClient`.

| Layer | Default and scope | What a hit avoids |
| --- | --- | --- |
| Voyage query-embedding LRU | `TACHI_QUERY_EMBEDDING_CACHE` defaults on; process-local, shared by client clones; capacity defaults to 512 | The embedding HTTP request for the same model, input type and exact text; search and optional rerank still run |
| Recall result cache | `TACHI_ENABLE_RECALL_CACHE` defaults off; eligible search handlers store serialized result rows in the global DB | Hybrid search and optional rerank, after request-key and authoritative-generation validation |
| Recall replay | Always bypasses the result-cache short-circuit | Nothing at the result layer; query-embedding reuse remains possible |

The result cache is ineligible when the handler records access, the request has a
nonempty sandbox role, the query is trivial, or automatic query embedding would
run. An embedding LRU hit does not change that last condition. Ordinary Memory
facade search records access, so enabling the result-cache flag does not make
that path a result-cache hit. A cacheable explicit query vector must contain only
finite values and participates in the request key in full. Eligible reads still
need a matching database generation and a fresh cache row; the default TTL is
900 seconds (`TACHI_RECALL_CACHE_TTL_SECS`).

For a latency receipt, use a controlled local fixture and record the exact
commit, host, request parameters, store scope, provider/model, cache settings,
sample count and provider-call count. Report these cases separately:

1. First query after a fresh process start, with a query absent from its LRU.
2. A different query on the warmed client, still absent from the LRU.
3. The same query repeated on that client, to measure embedding reuse.
4. An eligible result-cache miss and repeat, after confirming the handler's
   access policy and embedding path actually permit result caching.

Measure wall time at the same request boundary in every case. Do not attribute
the whole first-versus-warm difference to TLS without a transport measurement,
or report an embedding-cache hit as a result-cache hit. Concurrent identical
queries should also report actual provider requests so singleflight reuse is
distinguishable from independent warm requests. Use `TACHI_QUERY_EMBEDDING_CACHE=0`
before constructing the client when measuring the network path without LRU
reuse; record this setting rather than treating it as a production default.

Run the reviewed recall corpus alongside performance comparisons and retain the
existing ranking and visibility checks. Latency alone does not establish result
freshness, cross-process invalidation or permission correctness. Keep measurement
receipts in PR comments or CI artifacts; the shared latest-status artifact remains
aggregate-only.

## Decision-prior calibration (#2352)

The synthetic diagnostic keeps the production decision multiplier at `1.55`.
It compares actual returned order with fixed returned-pool sensitivity rows for
removing pair-exemption-dependent candidates and replacing the observed decision
boost with `1.40`, `1.25`, or `1.0`. Run it alongside the unchanged corpus gates:

```bash
cargo test -p memcore --lib ops_audit_corpus
cargo test -p memcore --lib golden_corpus
cargo test -p memcore --lib decision_prior_fixture_exercises_pair_exemption -- --ignored --nocapture
cargo test -p memcore --lib decision_prior_calibration_report -- --ignored --nocapture
```

The ignored fixture preflight checks the current keep policy's pair-exemption
distinction. It is not a permanent quality guard; a future calibration may
intentionally remove that exemption. The existing ops/golden gates remain
mandatory.

JSON lines tagged `decision_prior_calibration` contain per-query ranks, token
overlap, boost attribution, and explicit `PASS`/`FAIL` targets. The four additional
cases cover a one-token decision collision, repeated query tokens, five-of-eight
coverage, and a topical decision competing with a draft. Their rank-1 targets are
diagnostic labels, not established CI guards; a successful report invocation
does not mean those targets passed. Summaries retain failure names and report
MRR separately for each corpus and variant.

The sensitivity rows freeze the IDs actually returned by production after all
filtering. They neither restore filtered or tail candidates nor simulate pool
boundary changes after dropping an exemption. They do not recollect candidates
or recompute RRF channel ranks, lexical document frequency, or typo activation.
The report exposes both attributed and frozen returned-pool counts. These rows
identify changes to test, not measured quality for a new algorithm. Select keep,
tighten, or retune only after full pipeline replay preserves the existing gates
and independently reviewed local cases. Keep the exact commit and report output
in PR/CI evidence; this diagnostic does not read the personal memory store.
