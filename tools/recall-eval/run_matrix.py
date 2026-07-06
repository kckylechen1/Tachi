#!/usr/bin/env python3
"""Run the recall-eval variant matrix over the personal labeled cases
(tachi#708 Phase A, item 4). Drives the live daemon's `recall_simulate` via
mcp_client (cache-bypassing, access-count-safe read path).

Matrix (architecture doc §4.4):
  * baseline             -- current config (the "current" variant is always
                            returned by recall_simulate).
  * or_fallback=0.55     -- flips the disabled OR-fallback FTS factor (M1 fix).
  * fts_heavier          -- reweights default + events/notes groups toward FTS.
  * rrf_k=10             -- UNAVAILABLE: rrf_k is a hardcoded local constant in
                            scorer.rs (`let rrf_k = 60.0;`), NOT a RecallConfig
                            field, so `recall_simulate` variants cannot reach it.
                            Recorded as UNAVAILABLE rather than faked; testing it
                            requires promoting rrf_k to a real config knob first.
  * enable_rerank=true   -- separate pass (rerank is request-global, not per
                            variant), baseline config.

Outputs (both gitignored -- matrix detail carries real ids/queries):
  * reports/matrix.local.jsonl  -- one line per variant, full per-case detail.
  * reports/aggregates.md       -- aggregate numbers ONLY (hit@10, recall@k,
                                   MRR, per-slice hits); privacy-clean, meant
                                   for a human to hand-lift into the PR/issue.

Usage:
    python3 run_matrix.py [--cases cases.local.json] [--top-k 10]
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import MCPClient  # noqa: E402

FTS_HEAVIER = {
    "default_fts": 0.55, "default_semantic": 0.20,
    "default_symbolic": 0.15, "default_decay": 0.10,
    "events_notes_fts": 0.55, "events_notes_semantic": 0.20,
    "events_notes_symbolic": 0.15, "events_notes_decay": 0.10,
}

# Variants applied inside the same call as the current/baseline config.
CONFIG_VARIANTS = [
    {"name": "or_fallback_055", "recall_config": {"or_fallback_fts_score_factor": 0.55}},
    {"name": "fts_heavier", "recall_config": dict(FTS_HEAVIER)},
]


def load_cases(path: str) -> list[dict]:
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def slice_map(cases: list[dict]) -> dict[str, str]:
    return {c["name"]: c.get("slice", "unsliced") for c in cases}


def aggregate(variant_report: dict, smap: dict[str, str]) -> dict:
    cases = variant_report.get("cases", [])
    n = len(cases)
    hits = sum(1 for c in cases if c.get("hit"))
    rr = sum(c.get("reciprocal_rank", 0.0) for c in cases)
    per_slice: dict[str, dict[str, int]] = {}
    for c in cases:
        s = smap.get(c.get("name"), "unsliced")
        d = per_slice.setdefault(s, {"n": 0, "hits": 0})
        d["n"] += 1
        d["hits"] += 1 if c.get("hit") else 0
    return {
        "name": variant_report.get("name"),
        "config_env": variant_report.get("config_env", {}),
        "n": n,
        "hits": hits,
        "recall_at_k": round(hits / n, 4) if n else 0.0,
        "mrr": round(rr / n, 4) if n else 0.0,
        "per_slice": {k: per_slice[k] for k in sorted(per_slice)},
    }


def run(cases: list[dict], top_k: int, url: str) -> tuple[list[dict], list[dict]]:
    """Return (aggregates, detail_records). aggregates is privacy-clean;
    detail_records carry real ids and must stay local."""
    smap = slice_map(cases)
    client = MCPClient(url)
    client.initialize()

    # Pass 1: baseline (current) + config variants, rerank OFF.
    rep = client.recall_simulate(
        cases, variants=CONFIG_VARIANTS, top_k=top_k, enable_rerank=False)
    aggregates: list[dict] = []
    detail: list[dict] = []
    for vr in rep.get("variants", []):
        agg = aggregate(vr, smap)
        # config_env sanity (V1 pit #3: mistyped override fields are silently
        # dropped by serde(default); a non-current variant with empty diff means
        # the override never took effect).
        if agg["name"] != "current" and not agg["config_env"]:
            agg["warning"] = "empty config_env -- override may not have applied"
        aggregates.append(agg)
        detail.append({"variant": vr.get("name"), "config_env": vr.get("config_env", {}),
                       "cases": vr.get("cases", [])})

    # Pass 2: rerank ON, baseline config (rerank is request-global).
    rep_rr = client.recall_simulate(
        cases, variants=None, top_k=top_k, enable_rerank=True)
    rr_current = rep_rr.get("variants", [{}])[0]
    agg_rr = aggregate(rr_current, smap)
    agg_rr["name"] = "enable_rerank"
    aggregates.append(agg_rr)
    detail.append({"variant": "enable_rerank", "config_env": {},
                   "cases": rr_current.get("cases", [])})

    # rrf_k=10 -- structurally unavailable, recorded explicitly.
    aggregates.append({
        "name": "rrf_k_10",
        "status": "UNAVAILABLE",
        "reason": "rrf_k is a hardcoded local constant (scorer.rs `let rrf_k = 60.0;`), "
                  "not a RecallConfig field; recall_simulate variants cannot reach it. "
                  "Promote rrf_k to a config knob before this row can be measured.",
        "n": len(cases), "hits": None, "recall_at_k": None, "mrr": None, "per_slice": {},
    })
    return aggregates, detail


def write_reports(aggregates: list[dict], detail: list[dict], out_dir: str) -> tuple[str, str]:
    os.makedirs(out_dir, exist_ok=True)
    jsonl_path = os.path.join(out_dir, "matrix.local.jsonl")
    md_path = os.path.join(out_dir, "aggregates.md")

    with open(jsonl_path, "w", encoding="utf-8") as f:
        for rec in detail:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    all_slices: list[str] = []
    for a in aggregates:
        for s in a.get("per_slice", {}):
            if s not in all_slices:
                all_slices.append(s)
    all_slices.sort()

    lines: list[str] = []
    lines.append("# Recall-eval variant matrix (aggregates)\n")
    lines.append("Aggregate numbers only -- no query text, no memory ids. "
                 "Detail lives in matrix.local.jsonl (gitignored).\n")
    n = next((a["n"] for a in aggregates if a.get("n")), 0)
    lines.append(f"- cases: {n}\n")
    header = "| variant | hit@10 | recall@10 | MRR | " + " | ".join(all_slices) + " |"
    sep = "|---|---|---|---|" + "".join("---|" for _ in all_slices)
    lines.append(header)
    lines.append(sep)
    for a in aggregates:
        if a.get("status") == "UNAVAILABLE":
            cells = " | ".join("—" for _ in all_slices)
            lines.append(f"| {a['name']} | UNAVAILABLE | — | — | {cells} |")
            continue
        slice_cells = []
        for s in all_slices:
            d = a.get("per_slice", {}).get(s)
            slice_cells.append(f"{d['hits']}/{d['n']}" if d else "—")
        warn = " ⚠" if a.get("warning") else ""
        lines.append(
            f"| {a['name']}{warn} | {a['hits']}/{a['n']} | "
            f"{a['recall_at_k']} | {a['mrr']} | " + " | ".join(slice_cells) + " |")
    lines.append("")
    for a in aggregates:
        if a.get("status") == "UNAVAILABLE":
            lines.append(f"> **{a['name']}: {a['status']}** — {a['reason']}")
        if a.get("warning"):
            lines.append(f"> **{a['name']}**: {a['warning']}")
        if a.get("config_env"):
            lines.append(f"> `{a['name']}` config_env: `{json.dumps(a['config_env'])}`")
    lines.append("")
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
    aggregates, detail = run(cases, args.top_k, args.url)
    jsonl_path, md_path = write_reports(aggregates, detail, args.out_dir)
    print(f"wrote {jsonl_path}")
    print(f"wrote {md_path}")
    # Aggregate-only stdout.
    for a in aggregates:
        if a.get("status") == "UNAVAILABLE":
            print(f"  {a['name']:16s} UNAVAILABLE")
        else:
            print(f"  {a['name']:16s} hit@10={a['hits']}/{a['n']} "
                  f"recall={a['recall_at_k']} mrr={a['mrr']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
