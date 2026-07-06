#!/usr/bin/env python3
"""Build the adversarial recall-eval case set (tachi#708 Phase A follow-up).

`build_cases.py`'s cases are all *substrings* of the target memory's own
summary/keywords -- a query built that way is, almost by definition, a
conjunctive FTS win: every term is drawn from the document itself, so the
AND-all-terms default has nothing to fail on. That construction cannot
exercise the failure modes the architecture doc predicts:

  * M1 (FTS conjunctive all-or-nothing / CJK-blocked OR-fallback) needs a
    query whose words do NOT all co-occur verbatim in the target.
  * M2 (RRF k=60 rank-flattening) and M3 (wiki ×1.15 crowding) need queries
    that are *findable* but not a trivial top hit, so nearby candidates can
    out-rank the target.
  * The adaptive rerank gate (score_gap_too_wide) needs top hybrid scores
    close enough together that reranking has something to adjudicate --
    substring queries tend to produce one dominant exact-ish match instead.

This script fixes the case-construction problem, not the corpus problem: it
reuses the exact same read-only export + `tachi_default_retrievable`
exclusion set as `build_cases.py` (imported, not reimplemented), but instead
of deriving each query mechanically from its own memory, it assembles queries
from a hand-authored, gitignored seed file
(`adversarial_seed.local.json`) that pairs a memory id with a *paraphrased*
query -- reworded, partially-keyword, cross-document-ambiguous, or mixed
difficulty -- written by reading the memory's real content once, offline.
That is unavoidable: an adversarial case needs a human (or an agent standing
in read-only for one) to judge what counts as a genuine paraphrase, which
substring truncation cannot do.

Determinism: the seed file is the single source of truth for which memory
ids were chosen and what queries were written for them. Re-running this
script against the same live DB reproduces the same case file byte-for-byte
(modulo id churn -- see below), because no randomness is involved.

Slices (see seed file `slice` field):
  * paraphrase    -- summary reworded with non-overlapping vocabulary
                      (English and Chinese).
  * cjk_partial   -- half the memory's own keywords plus a synonym/rewrite
                      for the other half, so the query is not a strict
                      keyword-bag subset.
  * cross_doc     -- built from words that recur across MULTIPLE memories
                      (found by corpus word-frequency analysis) so several
                      candidates are lexically plausible and only one is
                      the labeled answer.
  * hard_mixed    -- fallback bucket mixing moderate paraphrase + partial
                      keyword difficulty, EN/CJK mixed.

Each seed id is re-validated against the live default-retrievable pool at
build time (an id that has since been archived/superseded/excluded is
dropped with a warning printed to stderr -- never silently swapped for a
different memory, which would break the "same seed -> same cases"
determinism promise).

Privacy: cases.adversarial.local.json and adversarial_seed.local.json both
embed real memory ids and real query text (paraphrased from real memory
content) -- both are gitignored. Only aggregate counts print to stdout.

Usage:
    python3 build_adversarial_cases.py
        [--seed adversarial_seed.local.json]
        [--out cases.adversarial.local.json]
        [--db ~/.tachi/global/memory.db]
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import types

# Reuse the zvec-shadow exclusion predicate verbatim -- same import trick as
# build_cases.py (stub sqlite_vec so we never touch the vec table / need the
# real dependency; this script reads memories only, no embeddings).
_ZVEC = os.path.join(os.path.dirname(__file__), "..", "zvec-shadow")
sys.path.insert(0, os.path.abspath(_ZVEC))
sys.modules.setdefault("sqlite_vec", types.ModuleType("sqlite_vec"))
import export_snapshot as es  # noqa: E402


def load_seed(path: str) -> list[dict]:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def load_retrievable_ids(db_path: str) -> dict[str, dict]:
    """Return {id: row} for every memory currently in the default-retrievable
    pool (archived=0 and not excluded by tachi_default_retrievable) -- the
    same predicate build_cases.py uses, imported not reimplemented."""
    uri = f"file:{db_path}?mode=ro"
    conn = sqlite3.connect(uri, uri=True, timeout=10.0)
    conn.execute("PRAGMA busy_timeout = 10000")
    sql = """
        SELECT m.id, m.path, m.summary, m.category, m.topic, m.source,
               m.metadata, m.superseded_by
        FROM memories m
        WHERE m.archived = 0
    """
    by_id: dict[str, dict] = {}
    for r in conn.execute(sql):
        (mid, path, summary, cat, topic, src, meta, sup) = r
        probe = {
            "id": mid, "path": path or "", "topic": topic or "",
            "source": src or "", "category": cat or "",
            "superseded_by": sup, "metadata": meta,
        }
        if es.tachi_default_retrievable(probe) is not None:
            continue
        by_id[mid] = {"id": mid, "path": path or "", "summary": (summary or "").strip()}
    conn.close()
    return by_id


def build_cases(seed: list[dict], retrievable: dict[str, dict]) -> tuple[list[dict], list[str]]:
    cases: list[dict] = []
    dropped: list[str] = []
    per_slice_seq: dict[str, int] = {}
    for entry in seed:
        mid = entry["id"]
        if mid not in retrievable:
            dropped.append(mid)
            continue
        slice_name = entry.get("slice", "unsliced")
        query = entry["query"].strip()
        if len(query) < 4:
            dropped.append(mid)
            continue
        per_slice_seq[slice_name] = per_slice_seq.get(slice_name, 0) + 1
        cases.append({
            "name": f"{slice_name}_{per_slice_seq[slice_name]}",
            "slice": slice_name,
            "query": query,
            "expected_id": mid,
        })
    return cases, dropped


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default=os.path.expanduser("~/.tachi/global/memory.db"))
    ap.add_argument("--seed", default=os.path.join(
        os.path.dirname(__file__), "adversarial_seed.local.json"))
    ap.add_argument("--out", default=os.path.join(
        os.path.dirname(__file__), "cases.adversarial.local.json"))
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"error: source db not found: {args.db}", file=sys.stderr)
        return 2
    if not os.path.exists(args.seed):
        print(f"error: seed file not found: {args.seed}", file=sys.stderr)
        return 2

    seed = load_seed(args.seed)
    retrievable = load_retrievable_ids(args.db)
    cases, dropped = build_cases(seed, retrievable)

    if dropped:
        print(f"warning: {len(dropped)} seed id(s) no longer default-retrievable, "
              f"dropped (not swapped): {len(dropped)} ids", file=sys.stderr)

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)

    slice_counts: dict[str, int] = {}
    for c in cases:
        slice_counts[c["slice"]] = slice_counts.get(c["slice"], 0) + 1
    print(json.dumps({
        "seed_entries": len(seed),
        "retrievable_pool": len(retrievable),
        "cases_built": len(cases),
        "dropped": len(dropped),
        "per_slice": dict(sorted(slice_counts.items())),
        "out": args.out,
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
