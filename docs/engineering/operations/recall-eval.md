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
keeps its existing payload. Raw `search_memory` callers can request
`include_metadata=true, format="json"`. Diagnostics contain the actual adaptive
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
