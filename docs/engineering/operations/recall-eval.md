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
`project`, `domain`, `path_prefix`, `as_of`, `top_k`, and `enabled`. Set
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
