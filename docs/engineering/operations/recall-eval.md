# Personal Recall Eval

`tachi eval recall` is the native operator gate for the private personal recall
corpus. It runs labeled `/eval` cases through the same `recall_simulate` kernel
used by the MCP facade, so the replay bypasses recall-cache short-circuiting and
does not mutate memory access counters.

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
`project`, `domain`, `path_prefix`, `as_of`, `top_k`, `label_kind`, and `enabled`. Set
`enabled=false` to keep a case in the DB but exclude it from the gate.

## Run

```bash
tachi eval recall
tachi eval recall --min-recall 0.95 --min-mrr 0.5 --json
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

The aggregate status includes `label_counts` and
`gate_label_policy: "all_loaded_cases"`. The existing recall/MRR gate continues
to evaluate all loaded cases; it does not imply independent answer quality for
weak or unspecified labels. Use a separate `--cases` file of reviewed cases as
the quality gate and keep unseen queries apart from tuning queries.

Replay and aggregate status also report `recall_at_1` and `recall_at_3`.
The latter uses only cases requesting `top_k >= 3`, with its denominator in
`recall_at_3_case_count`; it is null when none qualify. A top-one replay cannot
establish recall at three. Existing `recall_at_k` and MRR semantics are unchanged.

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

`recall_simulate` already provides per-case ranks, returned source DBs, scores,
rerank policy counts and baseline rank flips. Its per-case `rerank` now also
contains the same input score gap and requested top-k. Detailed replay remains
operator-scoped; the shared latest-status artifact stays aggregate-only.
Replay's `returned[].scores` is populated from the search row's canonical
`score` object, and `returned[].source` retains its source label for analysis.

## Cache-aware latency measurements

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
