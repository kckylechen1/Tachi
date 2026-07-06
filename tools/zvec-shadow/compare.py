#!/usr/bin/env python3
"""Tachi vs zvec-shadow-sidecar comparison harness (tachi#683 Phase 0, G4).

Builds a small "simple labeled set" of 20 queries from a real snapshot
(each query is derived from one memory's own summary, so that memory's id
is the expected hit), fires each query at:

  1. the real `tachi search` CLI (read-only path -- `search` never writes),
     run from $HOME rather than the current worktree, because `tachi search`
     auto-detects a per-worktree project DB scope when run from inside a
     worktree checkout, which routes to an empty per-project DB instead of
     the ~/.tachi/global/memory.db this snapshot was exported from (observed
     directly during this spike: identical query returned real hits from
     $HOME but `{"rows": []}` from inside the worktree cwd -- see README.md
     "CLI scope quirk").
  2. the zvec-shadow sidecar's POST /query endpoint (FTS-only by default;
     pass --hybrid to additionally send each query memory's own stored
     embedding, reusing doc-side vectors as a smoke test of the dense path
     -- see sidecar.py module docstring for why query-side embedding is not
     computed here).

...and reports, per query: both engines' top-10 ids, overlap@10 between the
two engines, whether each engine's top-10 contains the expected id
(hit@10), and each engine's latency. Emits a JSONL detail file and a
Markdown summary table+aggregates under tools/zvec-shadow/reports/.

This does NOT touch recall_simulate (Phase 1 concern -- see README for the
suggested integration point).

Usage:
    python3 compare.py --snapshot snapshot.jsonl --sidecar-url http://127.0.0.1:8791 \\
        [--n-queries 20] [--hybrid] [--out-prefix reports/compare]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

SENTENCE_SPLIT_RE = re.compile(r"[。.!?！？\n]")


def build_query_text(summary: str, max_len: int = 60) -> str:
    s = (summary or "").strip()
    first = SENTENCE_SPLIT_RE.split(s, maxsplit=1)[0].strip()
    q = first if first else s
    return q[:max_len]


def load_query_set(snapshot_path: str, n: int, min_summary_len: int = 20) -> list[dict]:
    picked = []
    with open(snapshot_path, "r", encoding="utf-8") as f:
        for line in f:
            rec = json.loads(line)
            summary = rec.get("summary") or ""
            if len(summary.strip()) < min_summary_len:
                continue
            picked.append(rec)
            if len(picked) >= n:
                break
    return picked


def query_tachi(query_text: str, top_k: int, timeout_s: float = 20.0) -> dict:
    home = os.path.expanduser("~")
    # `cwd=` alone is NOT enough: tachi's daemon-scope routing was observed
    # (during this spike) to key off the inherited `PWD` env var rather than
    # the process's actual working directory (confirmed directly: `env -C
    # ~ tachi search ...` with a stale PWD from inside this worktree still
    # routed to the empty per-worktree project DB; only overriding PWD too
    # made it hit ~/.tachi/global/memory.db). Both must be set together.
    env = dict(os.environ)
    env["PWD"] = home
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            ["tachi", "search", query_text, "--top-k", str(top_k)],
            cwd=home, env=env,  # avoid worktree-scoped project DB routing
            capture_output=True, text=True, timeout=timeout_s,
        )
    except subprocess.TimeoutExpired:
        return {"error": "timeout", "ids": [], "latency_ms": timeout_s * 1000}
    latency_ms = (time.perf_counter() - t0) * 1000
    if proc.returncode != 0:
        return {"error": f"exit {proc.returncode}: {proc.stderr.strip()[:300]}", "ids": [], "latency_ms": latency_ms}
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        return {"error": f"bad JSON: {e}", "ids": [], "latency_ms": latency_ms, "raw": proc.stdout[:300]}
    ids = []
    malformed = 0
    # Defensive: `tachi search` was observed (during this spike) to sometimes
    # return `rows` entries as plain strings instead of the usual row dict,
    # apparently under a different execution path (daemon burst/loop-detection
    # fallback to in-process execution was seen in the CLI's stderr log at the
    # same time). Skip anything that isn't the expected shape rather than
    # crashing the whole comparison run -- see README.md "CLI response-shape
    # flakiness" for the verbatim evidence.
    for section in data.get("sections", []):
        if not isinstance(section, dict) or section.get("name") != "Memory":
            continue
        for row in section.get("rows", []):
            if not isinstance(row, dict):
                malformed += 1
                continue
            if row.get("db") == "global" and "id" in row:  # only compare against the DB our snapshot came from
                ids.append(row["id"])
    err = f"{malformed} malformed row(s) skipped" if malformed else None
    return {"error": err, "ids": ids[:top_k], "latency_ms": latency_ms}


def query_sidecar(base_url: str, query_text: str, top_k: int, embedding: list[float] | None, timeout_s: float = 20.0) -> dict:
    body = {"text": query_text, "top_k": top_k}
    if embedding is not None:
        body["embedding"] = embedding
    req = urllib.request.Request(
        f"{base_url}/query", data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            data = json.loads(resp.read())
    except (urllib.error.URLError, TimeoutError) as e:
        return {"error": str(e), "ids": [], "latency_ms": (time.perf_counter() - t0) * 1000}
    latency_ms = (time.perf_counter() - t0) * 1000
    ids = [r["id"] for r in data.get("results", [])]
    return {"error": None, "ids": ids, "latency_ms": latency_ms, "server_latency_ms": data.get("latency_ms"), "mode": data.get("mode")}


def run(snapshot_path: str, sidecar_url: str, n_queries: int, top_k: int, use_hybrid: bool) -> list[dict]:
    query_set = load_query_set(snapshot_path, n_queries)
    rows = []
    for rec in query_set:
        qtext = build_query_text(rec["summary"])
        expected_id = rec["id"]
        tachi_res = query_tachi(qtext, top_k)
        embedding = rec.get("embedding") if use_hybrid else None
        sidecar_res = query_sidecar(sidecar_url, qtext, top_k, embedding)

        tachi_ids = tachi_res["ids"]
        sidecar_ids = sidecar_res["ids"]
        overlap = len(set(tachi_ids) & set(sidecar_ids))
        rows.append({
            "expected_id": expected_id,
            "query_text": qtext,
            "tachi_top10": tachi_ids,
            "tachi_latency_ms": round(tachi_res["latency_ms"], 2),
            "tachi_error": tachi_res["error"],
            "tachi_hit_at_10": expected_id in tachi_ids,
            "sidecar_top10": sidecar_ids,
            "sidecar_latency_ms": round(sidecar_res["latency_ms"], 2),
            "sidecar_server_latency_ms": sidecar_res.get("server_latency_ms"),
            "sidecar_mode": sidecar_res.get("mode"),
            "sidecar_error": sidecar_res["error"],
            "sidecar_hit_at_10": expected_id in sidecar_ids,
            "overlap_at_10": overlap,
        })
    return rows


def write_reports(rows: list[dict], out_prefix: str) -> tuple[str, str]:
    jsonl_path = out_prefix + ".jsonl"
    md_path = out_prefix + ".md"
    os.makedirs(os.path.dirname(jsonl_path) or ".", exist_ok=True)

    with open(jsonl_path, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    n = len(rows)
    tachi_hits = sum(1 for r in rows if r["tachi_hit_at_10"])
    sidecar_hits = sum(1 for r in rows if r["sidecar_hit_at_10"])
    mean_overlap = sum(r["overlap_at_10"] for r in rows) / n if n else 0.0
    mean_tachi_lat = sum(r["tachi_latency_ms"] for r in rows) / n if n else 0.0
    mean_sidecar_lat = sum(r["sidecar_latency_ms"] for r in rows) / n if n else 0.0
    n_tachi_err = sum(1 for r in rows if r["tachi_error"])
    n_sidecar_err = sum(1 for r in rows if r["sidecar_error"])

    lines = []
    lines.append("# zvec-shadow compare report\n")
    lines.append(f"- queries: {n}")
    lines.append(f"- tachi hit@10 (expected id present): {tachi_hits}/{n}")
    lines.append(f"- sidecar hit@10 (expected id present): {sidecar_hits}/{n}")
    lines.append(f"- mean overlap@10 (tachi vs sidecar top-10 agreement): {mean_overlap:.2f} / 10")
    lines.append(f"- mean tachi latency: {mean_tachi_lat:.1f} ms (subprocess round trip, includes CLI process startup)")
    lines.append(f"- mean sidecar latency: {mean_sidecar_lat:.1f} ms (HTTP round trip on localhost)")
    lines.append(f"- tachi errors: {n_tachi_err}/{n}, sidecar errors: {n_sidecar_err}/{n}\n")
    lines.append("| # | expected_id | query | tachi hit | sidecar hit | overlap@10 | tachi ms | sidecar ms |")
    lines.append("|---|---|---|---|---|---|---|---|")
    for i, r in enumerate(rows, 1):
        q = r["query_text"].replace("|", "\\|")[:40]
        lines.append(
            f"| {i} | `{r['expected_id'][:12]}...` | {q} | "
            f"{'yes' if r['tachi_hit_at_10'] else 'no'} | {'yes' if r['sidecar_hit_at_10'] else 'no'} | "
            f"{r['overlap_at_10']} | {r['tachi_latency_ms']:.1f} | {r['sidecar_latency_ms']:.1f} |"
        )
    with open(md_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return jsonl_path, md_path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--snapshot", required=True)
    ap.add_argument("--sidecar-url", default="http://127.0.0.1:8791")
    ap.add_argument("--n-queries", type=int, default=20)
    ap.add_argument("--top-k", type=int, default=10)
    ap.add_argument("--hybrid", action="store_true", help="Also send each query memory's own stored embedding to the sidecar (dense+FTS)")
    ap.add_argument("--out-prefix", default=os.path.join(os.path.dirname(__file__), "reports", "compare"))
    args = ap.parse_args()

    rows = run(args.snapshot, args.sidecar_url, args.n_queries, args.top_k, args.hybrid)
    jsonl_path, md_path = write_reports(rows, args.out_prefix)
    print(f"wrote {jsonl_path}")
    print(f"wrote {md_path}")
    print(open(md_path).read())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
