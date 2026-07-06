#!/usr/bin/env python3
"""Mechanical per-case flip report between two variants of a recall-eval
matrix run (tachi#708 Phase A follow-up; PR #723 CP3).

Why this exists: run_matrix.py writes per-variant detail (gitignored) and a
privacy-clean aggregate table, but any claim of the form "N cases flipped
hit->miss under rerank" was previously hand-computed from the gitignored
detail -- unreproducible by a review lane that (correctly) cannot see the
detail file. This script IS that computation, checked in, so the flip
numbers in a PR body can be regenerated mechanically from a local rerun.

It aligns the baseline variant's per-case results with a comparison
variant's by case name and emits ONLY aggregates:

  * hit->miss count (baseline hit, variant miss)
  * miss->hit count (baseline miss, variant hit)
  * rank worsened / improved / unchanged counts, over cases where BOTH
    variants ranked the target somewhere in their returned window
  * the same, broken down per slice

Privacy line (same as the rest of the toolchain): the INPUT is the
gitignored matrix.local.jsonl detail; the OUTPUT never contains query text,
expected ids, returned ids, or any per-case row -- aggregate counts and
slice names only. Slice is derived from the case name prefix
("<slice>_<n>", the shape both build_cases.py and build_adversarial_cases.py
emit), so no cases file is needed.

Usage:
    python3 flip_report.py --variant enable_rerank
    python3 flip_report.py --variant or_fallback_055
    python3 flip_report.py [--detail reports/matrix.local.jsonl]
        [--baseline current] [--variant enable_rerank] [--top-k 10]
"""
from __future__ import annotations

import argparse
import json
import os
import sys


def load_detail(path: str) -> dict[str, dict[str, dict]]:
    """Return {variant_name: {case_name: case_report}}."""
    out: dict[str, dict[str, dict]] = {}
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            out[rec["variant"]] = {c.get("name"): c for c in rec.get("cases", [])}
    return out


def slice_of(case_name: str) -> str:
    """Slice from the "<slice>_<n>" case-name shape both case builders emit."""
    head, sep, tail = case_name.rpartition("_")
    if sep and head and tail.isdigit():
        return head
    return "unsliced"


def compare(base: dict[str, dict], var: dict[str, dict]) -> dict:
    def bucket() -> dict:
        return {"n": 0, "hit_to_miss": 0, "miss_to_hit": 0,
                "rank_worsened": 0, "rank_improved": 0, "rank_unchanged": 0,
                "rank_comparable": 0}

    total = bucket()
    per_slice: dict[str, dict] = {}
    for name, bc in base.items():
        vc = var.get(name)
        if vc is None:
            continue
        for d in (total, per_slice.setdefault(slice_of(name), bucket())):
            d["n"] += 1
            if bc.get("hit") and not vc.get("hit"):
                d["hit_to_miss"] += 1
            elif not bc.get("hit") and vc.get("hit"):
                d["miss_to_hit"] += 1
            br, vr = bc.get("rank"), vc.get("rank")
            if br is not None and vr is not None:
                d["rank_comparable"] += 1
                if vr > br:
                    d["rank_worsened"] += 1
                elif vr < br:
                    d["rank_improved"] += 1
                else:
                    d["rank_unchanged"] += 1
    return {"total": total, "per_slice": {k: per_slice[k] for k in sorted(per_slice)}}


def render(baseline: str, variant: str, cmp: dict) -> str:
    t = cmp["total"]
    lines: list[str] = []
    lines.append(f"## flips: {baseline} -> {variant}\n")
    lines.append("Aggregate-only (no query text, no ids). rank_* counts cover "
                 "cases where BOTH variants ranked the target in their "
                 "returned window.\n")
    header = ("| scope | n | hit→miss | miss→hit | rank worsened | "
              "rank improved | rank unchanged | rank comparable |")
    lines.append(header)
    lines.append("|---|---|---|---|---|---|---|---|")

    def row(label: str, d: dict) -> str:
        return (f"| {label} | {d['n']} | {d['hit_to_miss']} | {d['miss_to_hit']} | "
                f"{d['rank_worsened']} | {d['rank_improved']} | "
                f"{d['rank_unchanged']} | {d['rank_comparable']} |")

    lines.append(row("TOTAL", t))
    for s, d in cmp["per_slice"].items():
        lines.append(row(s, d))
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--detail", default=os.path.join(
        os.path.dirname(__file__), "reports", "matrix.local.jsonl"))
    ap.add_argument("--baseline", default="current")
    ap.add_argument("--variant", default="enable_rerank")
    ap.add_argument("--out", default=None,
                    help="Optional path to also write the markdown report "
                         "(default: reports/flips.md next to --detail).")
    args = ap.parse_args()

    if not os.path.exists(args.detail):
        print(f"error: detail file not found: {args.detail} (run run_matrix.py first)",
              file=sys.stderr)
        return 2

    detail = load_detail(args.detail)
    for v in (args.baseline, args.variant):
        if v not in detail:
            print(f"error: variant '{v}' not in {args.detail} "
                  f"(have: {sorted(detail)})", file=sys.stderr)
            return 2

    cmp = compare(detail[args.baseline], detail[args.variant])
    md = render(args.baseline, args.variant, cmp)
    print(md)

    out = args.out or os.path.join(os.path.dirname(args.detail), "flips.md")
    mode = "a" if os.path.exists(out) else "w"
    with open(out, mode, encoding="utf-8") as f:
        if mode == "w":
            f.write("# Recall-eval flip report (aggregates)\n\n")
        f.write(md + "\n")
    print(f"appended to {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
