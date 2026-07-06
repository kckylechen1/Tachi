#!/usr/bin/env python3
"""Build a personal labeled-case set for the recall-eval matrix (tachi#708
Phase A, personal-corpus item). Read-only against the live ~/.tachi memory.db.

Corpus alignment: the candidate pool is the EXACT default-retrievable set
`tachi search` would return -- we reuse `tools/zvec-shadow/export_snapshot.py`'s
`tachi_default_retrievable()` predicate verbatim (no home-grown filter), so a
labeled expected_id is always something a default recall could actually return.

Each case's `expected_id` is the source memory's own id; the query is derived
from that memory (summary sentence, or its keyword bag). Slices stress the
mechanisms the architecture doc calls out:
  * summary_en / summary_mixed / summary_cjk -- summary-derived queries;
    the pure-CJK slice is the M1 discriminator (FTS conjunctive all-or-nothing
    + CJK-blocked OR fallback should starve these on current main).
  * keyword_bag / keyword_bag_cjk -- multi-token bag queries; stress the
    symbolic/lexical channels and coverage.

Query construction fixes the compare.py truncation bug: compare.py did a raw
`summary[:60]` char slice, which splits an ASCII word mid-token (".. architec")
and hands FTS a garbage prefix term. Here we truncate on a WORD boundary --
ASCII words are kept whole; CJK is per-char (libsimple segmentation) so a cut
between CJK chars is fine.

Privacy: the output embeds real memory queries + ids, so cases.local.json is
gitignored (PR #700 snapshot precedent). Only aggregate counts print to stdout.

Usage:
    python3 build_cases.py [--db ~/.tachi/global/memory.db]
        [--out cases.local.json] [--seed 708] [--limit-per-slice ...]
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import sqlite3
import sys
import types

# Reuse the zvec-shadow exclusion predicate without pulling in its sqlite_vec
# dependency (we never touch the vec table here -- cases need no embeddings).
_ZVEC = os.path.join(os.path.dirname(__file__), "..", "zvec-shadow")
sys.path.insert(0, os.path.abspath(_ZVEC))
sys.modules.setdefault("sqlite_vec", types.ModuleType("sqlite_vec"))
import export_snapshot as es  # noqa: E402

CJK_RE = re.compile(r"[㐀-鿿豈-﫿]")
ASCII_WORD_RE = re.compile(r"[A-Za-z]{2,}")
SENTENCE_SPLIT_RE = re.compile(r"[。.!?！？\n]")


def has_cjk(s: str) -> bool:
    return bool(CJK_RE.search(s))


def has_ascii_word(s: str) -> bool:
    return bool(ASCII_WORD_RE.search(s))


def _is_ascii_word_char(ch: str) -> bool:
    return ch.isascii() and (ch.isalnum() or ch == "_")


def truncate_word_boundary(s: str, max_len: int = 60) -> str:
    """Truncate to <= max_len chars WITHOUT splitting an ASCII word.

    CJK characters are individually tokenized by libsimple, so a cut between
    two CJK chars is a legitimate token boundary and is left as-is. Only an
    ASCII word straddling the cut is retreated to its start."""
    s = s.strip()
    if len(s) <= max_len:
        return s
    cut = max_len
    if _is_ascii_word_char(s[cut - 1]) and _is_ascii_word_char(s[cut]):
        j = cut
        while j > 0 and _is_ascii_word_char(s[j - 1]):
            j -= 1
        if j > 0:  # keep the whole-word prefix; drop the split trailing word
            cut = j
    return s[:cut].strip()


def summary_query(summary: str, max_len: int = 60) -> str:
    s = summary.strip()
    first = SENTENCE_SPLIT_RE.split(s, maxsplit=1)[0].strip()
    base = first if first else s
    return truncate_word_boundary(base, max_len)


def keyword_query(keywords: list[str], k: int = 4) -> str:
    picked = [str(w).strip() for w in keywords if str(w).strip()][:k]
    return " ".join(picked)


def load_rows(db_path: str) -> list[dict]:
    uri = f"file:{db_path}?mode=ro"
    conn = sqlite3.connect(uri, uri=True, timeout=10.0)
    conn.execute("PRAGMA busy_timeout = 10000")
    sql = """
        SELECT m.id, m.path, m.summary, m.text, m.keywords, m.category,
               m.topic, m.source, m.metadata, m.superseded_by
        FROM memories m
        WHERE m.archived = 0
        ORDER BY m.created_at DESC
    """
    rows: list[dict] = []
    for r in conn.execute(sql):
        (mid, path, summary, text, kw, cat, topic, src, meta, sup) = r
        probe = {
            "id": mid, "path": path or "", "topic": topic or "",
            "source": src or "", "category": cat or "",
            "superseded_by": sup, "metadata": meta,
        }
        if es.tachi_default_retrievable(probe) is not None:
            continue
        try:
            kwl = json.loads(kw) if kw else []
        except json.JSONDecodeError:
            kwl = []
        if not isinstance(kwl, list):
            kwl = []
        rows.append({
            "id": mid,
            "path": path or "",
            "summary": (summary or "").strip(),
            "keywords": [str(w) for w in kwl if str(w).strip()],
        })
    conn.close()
    return rows


def build_cases(rows: list[dict], seed: int, quota: dict[str, int]) -> list[dict]:
    rng = random.Random(seed)

    def pool_summary(pred, min_len):
        p = [r for r in rows if len(r["summary"]) >= min_len and pred(r["summary"])]
        rng.shuffle(p)
        return p

    def pool_keywords(cjk: bool):
        out = []
        for r in rows:
            kws = r["keywords"]
            if len(kws) < 3:
                continue
            joined = " ".join(kws)
            if cjk and not has_cjk(joined):
                continue
            if (not cjk) and has_cjk(joined):
                continue
            out.append(r)
        rng.shuffle(out)
        return out

    slices = {
        "summary_en": pool_summary(
            lambda s: not has_cjk(s) and has_ascii_word(s), 20),
        "summary_mixed": pool_summary(
            lambda s: has_cjk(s) and has_ascii_word(s), 20),
        "summary_cjk": pool_summary(
            lambda s: has_cjk(s) and not has_ascii_word(s), 12),
        "keyword_bag": pool_keywords(cjk=False),
        "keyword_bag_cjk": pool_keywords(cjk=True),
    }

    cases: list[dict] = []
    used_ids: set[str] = set()
    for slice_name, want in quota.items():
        pool = slices.get(slice_name, [])
        taken = 0
        for r in pool:
            if taken >= want:
                break
            if r["id"] in used_ids:
                continue
            if slice_name.startswith("keyword"):
                q = keyword_query(r["keywords"])
            else:
                q = summary_query(r["summary"])
            if len(q.strip()) < 4:
                continue
            used_ids.add(r["id"])
            cases.append({
                "name": f"{slice_name}_{taken+1}",
                "slice": slice_name,
                "query": q,
                "expected_id": r["id"],
            })
            taken += 1
    return cases


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default=os.path.expanduser("~/.tachi/global/memory.db"))
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "cases.local.json"))
    ap.add_argument("--seed", type=int, default=708)
    ap.add_argument("--summary-en", type=int, default=10)
    ap.add_argument("--summary-mixed", type=int, default=10)
    ap.add_argument("--summary-cjk", type=int, default=8)
    ap.add_argument("--keyword-bag", type=int, default=8)
    ap.add_argument("--keyword-bag-cjk", type=int, default=6)
    ap.add_argument("--smoke", type=int, default=0,
                    help="If >0, keep only the first N cases (smoke test).")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"error: source db not found: {args.db}", file=sys.stderr)
        return 2

    rows = load_rows(args.db)
    quota = {
        "summary_en": args.summary_en,
        "summary_mixed": args.summary_mixed,
        "summary_cjk": args.summary_cjk,
        "keyword_bag": args.keyword_bag,
        "keyword_bag_cjk": args.keyword_bag_cjk,
    }
    cases = build_cases(rows, args.seed, quota)
    if args.smoke > 0:
        # Keep a spread across slices: round-robin first N.
        by_slice: dict[str, list[dict]] = {}
        for c in cases:
            by_slice.setdefault(c["slice"], []).append(c)
        picked: list[dict] = []
        while len(picked) < args.smoke and any(by_slice.values()):
            for s in list(by_slice):
                if by_slice[s]:
                    picked.append(by_slice[s].pop(0))
                    if len(picked) >= args.smoke:
                        break
        cases = picked

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)

    # Aggregate-only stdout (no query text / ids).
    slice_counts: dict[str, int] = {}
    for c in cases:
        slice_counts[c["slice"]] = slice_counts.get(c["slice"], 0) + 1
    print(json.dumps({
        "retrievable_pool": len(rows),
        "cases_built": len(cases),
        "per_slice": dict(sorted(slice_counts.items())),
        "out": args.out,
    }, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
