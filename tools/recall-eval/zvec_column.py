#!/usr/bin/env python3
"""zvec-sidecar comparison column for the recall-eval cases (tachi#708 Phase A,
architecture doc §3 escape gate + §4.4 "zvec-sidecar comparison column").

Reuses the EXISTING tools/zvec-shadow sidecar (BM25 ranked-OR FTS) rather than
reimplementing anything: it fires each recall-eval case's query at the sidecar's
POST /query endpoint and reports whether the case's expected_id is in the
sidecar's top-10 -- the same hit@10 metric the Tachi matrix reports, over the
SAME labeled cases, so the two columns are directly comparable.

Prerequisite -- stand up the sidecar first (one-time, all local, no paid API):

    cd ../zvec-shadow
    pip install -r requirements.txt          # sqlite-vec, zvec, numpy
    python3 export_snapshot.py --out snapshot.jsonl     # read-only DB export
    python3 sidecar.py --snapshot snapshot.jsonl --port 8791 &   # serves /query

Then, from this directory:

    python3 zvec_column.py --cases cases.local.json --sidecar-url http://127.0.0.1:8791

The sidecar's snapshot MUST be exported from the same DB build_cases.py read, so
the expected_ids exist in the sidecar corpus (both use the identical
default-retrievable exclusion set -- export_snapshot.tachi_default_retrievable).

Outputs (gitignored -- detail carries real ids):
  * reports/zvec_column.local.jsonl -- per-case sidecar hit detail.
  * reports/zvec_column.md          -- aggregate hit@10 + per-slice (privacy-clean).
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request


def load_cases(path: str) -> list[dict]:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def query_sidecar(base_url: str, query_text: str, top_k: int,
                  timeout_s: float = 20.0) -> dict:
    body = {"text": query_text, "top_k": top_k}
    req = urllib.request.Request(
        f"{base_url}/query", data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"}, method="POST")
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            data = json.loads(resp.read())
    except (urllib.error.URLError, TimeoutError) as e:
        return {"error": str(e), "ids": [], "latency_ms": (time.perf_counter() - t0) * 1000}
    ids = [r["id"] for r in data.get("results", [])]
    return {"error": None, "ids": ids[:top_k],
            "latency_ms": (time.perf_counter() - t0) * 1000, "mode": data.get("mode")}


def run(cases: list[dict], sidecar_url: str, top_k: int) -> tuple[dict, list[dict]]:
    detail: list[dict] = []
    hits = 0
    errors = 0
    per_slice: dict[str, dict[str, int]] = {}
    for c in cases:
        res = query_sidecar(sidecar_url, c["query"], top_k)
        if res["error"]:
            errors += 1
        hit = c["expected_id"] in res["ids"]
        hits += 1 if hit else 0
        s = c.get("slice", "unsliced")
        d = per_slice.setdefault(s, {"n": 0, "hits": 0})
        d["n"] += 1
        d["hits"] += 1 if hit else 0
        detail.append({"name": c.get("name"), "slice": s, "hit": hit,
                       "error": res["error"], "mode": res.get("mode")})
    n = len(cases)
    summary = {
        "engine": "zvec-shadow-sidecar (FTS BM25 ranked-OR)",
        "n": n, "hits": hits, "errors": errors,
        "recall_at_k": round(hits / n, 4) if n else 0.0,
        "per_slice": {k: per_slice[k] for k in sorted(per_slice)},
    }
    return summary, detail


def write_reports(summary: dict, detail: list[dict], out_dir: str) -> tuple[str, str]:
    os.makedirs(out_dir, exist_ok=True)
    jsonl_path = os.path.join(out_dir, "zvec_column.local.jsonl")
    md_path = os.path.join(out_dir, "zvec_column.md")
    with open(jsonl_path, "w", encoding="utf-8") as f:
        for rec in detail:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    lines = ["# zvec-sidecar comparison column (aggregates)\n",
             f"- engine: {summary['engine']}",
             f"- cases: {summary['n']}",
             f"- hit@10: {summary['hits']}/{summary['n']} (recall={summary['recall_at_k']})",
             f"- sidecar errors: {summary['errors']}\n",
             "| slice | hit@10 |", "|---|---|"]
    for s in sorted(summary["per_slice"]):
        d = summary["per_slice"][s]
        lines.append(f"| {s} | {d['hits']}/{d['n']} |")
    with open(md_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return jsonl_path, md_path


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cases", default=os.path.join(os.path.dirname(__file__), "cases.local.json"))
    ap.add_argument("--sidecar-url", default="http://127.0.0.1:8791")
    ap.add_argument("--top-k", type=int, default=10)
    ap.add_argument("--out-dir", default=os.path.join(os.path.dirname(__file__), "reports"))
    args = ap.parse_args()

    if not os.path.exists(args.cases):
        print(f"error: cases file not found: {args.cases} (run build_cases.py first)",
              file=sys.stderr)
        return 2

    cases = load_cases(args.cases)
    # Fail loudly with the standup instructions if the sidecar is unreachable.
    probe = query_sidecar(args.sidecar_url, "healthcheck", 1)
    if probe["error"]:
        print(f"error: zvec sidecar not reachable at {args.sidecar_url} "
              f"({probe['error']}).\nStand it up first -- see this file's module "
              f"docstring / README.md 'zvec comparison column'.", file=sys.stderr)
        return 3

    summary, detail = run(cases, args.sidecar_url, args.top_k)
    jsonl_path, md_path = write_reports(summary, detail, args.out_dir)
    print(f"wrote {jsonl_path}")
    print(f"wrote {md_path}")
    print(json.dumps({"zvec_hit_at_10": f"{summary['hits']}/{summary['n']}",
                      "recall_at_k": summary["recall_at_k"],
                      "errors": summary["errors"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
