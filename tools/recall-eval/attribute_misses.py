#!/usr/bin/env python3
"""Attribute baseline recall misses to the architecture doc's mechanisms
(tachi#708 Phase A, item 3). Drives the live daemon via recall_simulate.

Why counterfactual, not per-channel scores: recall_simulate's returned rows
expose only the final `relevance` (RRF-fused score) -- the per-channel
`scores`/`match_type` fields are null on the normal search path (they are only
populated for exact-token rows). So we attribute each miss by COUNTERFACTUAL
probing -- which concrete fix (or structural fact) recovers or explains it --
which is exactly what Phase B/C/D need to know anyway:

For every baseline miss (expected id NOT in top-k) we measure:
  * or_fb_rank -- rank under or_fallback_fts_score_factor=0.55 (top-10).
  * fts_rank   -- rank under an FTS-heavier weighting (top-10).
  * wide_rank  -- rank under baseline config at top_k=100 (candidate visibility).
  * wiki_above -- how many /wiki rows outrank the target inside that top-100.

Classification (single primary bucket, priority order):
  M1  OR-fallback coverage recovers the target into top-10
      -> the FTS all-or-nothing / CJK-blocked fallback was the block.
  OTHER  target absent even from top-100 under baseline
      -> not a candidate; unexplained by M1/M2/M3 (candidate starvation /
         server-side filter -- the §6 "fourth mechanism" watch).
  M3  target IS a top-100 candidate AND enough /wiki rows outrank it that
      removing them would lift it into top-10 (wiki_above >= wide_rank-10).
  M2  target IS a top-100 candidate but ranked out of top-10 with no wiki
      explanation -> RRF k=60 rank-flattening pushed a real candidate below
      the cut (the rrf_k fix that Phase A cannot vary -- see run_matrix).

Outputs (gitignored -- detail keyed by case name, aggregates privacy-clean):
  * reports/attribution.local.jsonl -- per-miss detail.
  * reports/attribution.md          -- bucket counts + per-slice breakdown.

Usage:
    python3 attribute_misses.py [--cases cases.local.json] [--top-k 10]
"""
from __future__ import annotations

import argparse
import copy
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import MCPClient  # noqa: E402
from run_matrix import CONFIG_VARIANTS  # noqa: E402  (or_fallback_055 + fts_heavier)

WIDE_TOP_K = 100


def load_cases(path: str) -> list[dict]:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def _case_report_index(variant_report: dict) -> dict[str, dict]:
    return {c.get("name"): c for c in variant_report.get("cases", [])}


def _wiki_above(case_report: dict) -> int | None:
    """Count /wiki rows ranked above the target in a wide case report.
    Returns None if the target is absent from the returned window."""
    rank = case_report.get("rank")
    returned = case_report.get("returned", [])
    if rank is None:
        return None
    above = 0
    for row in returned[: rank - 1]:  # rows strictly above the target
        path = (row.get("path") or "")
        if path.startswith("/wiki"):
            above += 1
    return above


def classify(or_fb_rank, fts_rank, wide_rank, wiki_above) -> str:
    if or_fb_rank is not None:
        return "M1"
    if wide_rank is None:
        return "OTHER"
    gap = wide_rank - 10  # rows the target must climb to enter top-10
    if wiki_above is not None and wiki_above > 0 and wiki_above >= gap:
        return "M3"
    return "M2"


def run(cases: list[dict], top_k: int, url: str) -> tuple[dict, list[dict]]:
    client = MCPClient(url)
    client.initialize()

    baseline = client.recall_simulate(cases, variants=None, top_k=top_k,
                                       enable_rerank=False)
    base_cases = baseline.get("variants", [{}])[0].get("cases", [])
    misses = [c for c in base_cases if not c.get("hit")]
    miss_names = {c.get("name") for c in misses}
    if not misses:
        return ({"baseline_misses": 0, "buckets": {}, "per_slice": {}}, [])

    miss_case_specs = [c for c in cases if c.get("name") in miss_names]
    slice_of = {c["name"]: c.get("slice", "unsliced") for c in cases}

    # Diagnostic pass: or_fallback + fts_heavier at top-10.
    diag = client.recall_simulate(miss_case_specs, variants=CONFIG_VARIANTS,
                                  top_k=top_k, enable_rerank=False)
    diag_by_name = {v.get("name"): _case_report_index(v) for v in diag.get("variants", [])}

    # Wide pass: baseline config, top_k=100 (per-case override).
    wide_specs = []
    for c in miss_case_specs:
        w = copy.deepcopy(c)
        w["top_k"] = WIDE_TOP_K
        wide_specs.append(w)
    wide = client.recall_simulate(wide_specs, variants=None, top_k=WIDE_TOP_K,
                                  enable_rerank=False)
    wide_idx = _case_report_index(wide.get("variants", [{}])[0])

    detail: list[dict] = []
    buckets: dict[str, int] = {"M1": 0, "M2": 0, "M3": 0, "OTHER": 0}
    per_slice: dict[str, dict[str, int]] = {}
    for c in misses:
        name = c.get("name")
        or_fb_rank = diag_by_name.get("or_fallback_055", {}).get(name, {}).get("rank")
        fts_rank = diag_by_name.get("fts_heavier", {}).get(name, {}).get("rank")
        wcase = wide_idx.get(name, {})
        wide_rank = wcase.get("rank")
        wiki_above = _wiki_above(wcase)
        bucket = classify(or_fb_rank, fts_rank, wide_rank, wiki_above)
        buckets[bucket] += 1
        s = slice_of.get(name, "unsliced")
        d = per_slice.setdefault(s, {"M1": 0, "M2": 0, "M3": 0, "OTHER": 0})
        d[bucket] += 1
        detail.append({
            "name": name, "slice": s, "bucket": bucket,
            "or_fb_rank": or_fb_rank, "fts_rank": fts_rank,
            "wide_rank": wide_rank, "wiki_above": wiki_above,
        })

    summary = {
        "case_count": len(cases),
        "baseline_misses": len(misses),
        "buckets": buckets,
        "per_slice": {k: per_slice[k] for k in sorted(per_slice)},
    }
    return summary, detail


def write_reports(summary: dict, detail: list[dict], out_dir: str) -> tuple[str, str]:
    os.makedirs(out_dir, exist_ok=True)
    jsonl_path = os.path.join(out_dir, "attribution.local.jsonl")
    md_path = os.path.join(out_dir, "attribution.md")

    with open(jsonl_path, "w", encoding="utf-8") as f:
        for rec in detail:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    lines: list[str] = []
    lines.append("# Recall-eval miss attribution (aggregates)\n")
    lines.append("Counterfactual attribution of BASELINE misses. Aggregate counts "
                 "only; per-miss detail in attribution.local.jsonl (gitignored).\n")
    lines.append(f"- cases: {summary.get('case_count')}")
    lines.append(f"- baseline misses: {summary.get('baseline_misses')}\n")
    buckets = summary.get("buckets", {})
    lines.append("| mechanism | misses |")
    lines.append("|---|---|")
    for b in ("M1", "M2", "M3", "OTHER"):
        lines.append(f"| {b} | {buckets.get(b, 0)} |")
    lines.append("")
    per_slice = summary.get("per_slice", {})
    if per_slice:
        lines.append("| slice | M1 | M2 | M3 | OTHER |")
        lines.append("|---|---|---|---|---|")
        for s in sorted(per_slice):
            d = per_slice[s]
            lines.append(f"| {s} | {d['M1']} | {d['M2']} | {d['M3']} | {d['OTHER']} |")
        lines.append("")
    lines.append("Legend: M1=FTS all-or-nothing (OR-fallback recovers); "
                 "M2=RRF k=60 rank-flatten (candidate present, ranked out); "
                 "M3=wiki ×1.15 crowding; OTHER=absent from top-100 (candidate starvation).")
    with open(md_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return jsonl_path, md_path


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cases", default=os.path.join(os.path.dirname(__file__), "cases.local.json"))
    ap.add_argument("--top-k", type=int, default=10)
    ap.add_argument("--url", default="http://127.0.0.1:6919/mcp")
    ap.add_argument("--out-dir", default=os.path.join(os.path.dirname(__file__), "reports"))
    args = ap.parse_args()

    if not os.path.exists(args.cases):
        print(f"error: cases file not found: {args.cases} (run build_cases.py first)",
              file=sys.stderr)
        return 2

    cases = load_cases(args.cases)
    summary, detail = run(cases, args.top_k, args.url)
    jsonl_path, md_path = write_reports(summary, detail, args.out_dir)
    print(f"wrote {jsonl_path}")
    print(f"wrote {md_path}")
    print(json.dumps({"baseline_misses": summary.get("baseline_misses"),
                      "buckets": summary.get("buckets")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
