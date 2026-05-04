#!/usr/bin/env python3
"""Tune LongMemEval hybrid-search weights from saved Tachi candidates.

Reads the LongMemEval oracle plus the benchmark SQLite DB, builds top-50
candidate pools with raw channel scores, and grid-searches weights that
maximize R@3 overall and per question_type.
"""

from __future__ import annotations

import json
import math
import os
import re
import sqlite3
from collections import defaultdict
from datetime import datetime, timezone
from typing import Any

ORACLE_PATH = os.path.expanduser(
    "~/Desktop/tachitest/LongMemEval/data/longmemeval_oracle.json"
)
DB_PATH = os.path.expanduser("~/.tachi/projects/longmemeval-bench/memory.db")
TOP_N = 50

STOPWORDS = {
    "about",
    "after",
    "also",
    "and",
    "are",
    "before",
    "been",
    "did",
    "for",
    "from",
    "had",
    "has",
    "have",
    "how",
    "into",
    "its",
    "like",
    "more",
    "most",
    "that",
    "the",
    "their",
    "then",
    "there",
    "this",
    "was",
    "what",
    "when",
    "which",
    "with",
    "you",
}


def tokens(text: str) -> list[str]:
    return [t for t in re.findall(r"[a-z0-9]{2,}", text.lower()) if t not in STOPWORDS]


def norm(scores: dict[str, float]) -> dict[str, float]:
    if not scores:
        return {}
    max_score = max(scores.values())
    if max_score <= 0:
        return {k: 0.0 for k in scores}
    return {k: v / max_score for k, v in scores.items()}


def parse_keywords(raw: str) -> list[str]:
    try:
        parsed = json.loads(raw or "[]")
        return [str(v) for v in parsed if str(v).strip()]
    except Exception:
        return []


def parse_time(raw: str) -> datetime:
    try:
        return datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except Exception:
        return datetime.now(timezone.utc)


def decay_score(timestamp: str) -> float:
    age_days = max(
        0.0,
        (datetime.now(timezone.utc) - parse_time(timestamp)).total_seconds() / 86400.0,
    )
    return 0.5 ** (age_days / 30.0)


def fts_scores(conn: sqlite3.Connection, query: str) -> dict[str, float]:
    try:
        rows = conn.execute(
            """
            SELECT memories_fts.id, -bm25(memories_fts) AS score
            FROM memories_fts
            JOIN memories m ON m.id = memories_fts.id
            WHERE memories_fts MATCH simple_query(?1)
              AND m.path LIKE '/bench/%'
              AND m.archived = 0
            ORDER BY bm25(memories_fts)
            LIMIT ?2
            """,
            (query, TOP_N),
        ).fetchall()
    except sqlite3.Error:
        rows = []
    return norm({row[0]: float(row[1]) for row in rows})


def load_bench_rows(conn: sqlite3.Connection) -> list[sqlite3.Row]:
    conn.row_factory = sqlite3.Row
    return conn.execute(
        """
        SELECT id, path, text, summary, keywords, timestamp
        FROM memories
        WHERE path LIKE '/bench/%' AND archived = 0
        """
    ).fetchall()


def symbolic_scores(query: str, rows: list[sqlite3.Row]) -> dict[str, float]:
    q_tokens = set(tokens(query))
    if not q_tokens:
        return {}
    scored: dict[str, float] = {}
    for row in rows:
        doc_tokens = set(tokens(f"{row['summary']} {row['text']} {' '.join(parse_keywords(row['keywords']))}"))
        if not doc_tokens:
            continue
        overlap = len(q_tokens & doc_tokens)
        if overlap:
            scored[row["id"]] = overlap / math.sqrt(len(q_tokens) * len(doc_tokens))
    return dict(sorted(norm(scored).items(), key=lambda item: item[1], reverse=True)[:TOP_N])


def answer_indices(item: dict[str, Any]) -> set[int]:
    answer_ids = set(item.get("answer_session_ids", []))
    return {
        idx
        for idx, sid in enumerate(item.get("haystack_session_ids", []))
        if sid in answer_ids
    }


def is_answer(path: str, item: dict[str, Any]) -> bool:
    match = re.search(r"/bench/([^/]+)/sess(\d+)", path)
    return bool(
        match
        and match.group(1) == item["question_id"]
        and int(match.group(2)) in answer_indices(item)
    )


def build_examples(conn: sqlite3.Connection, oracle: list[dict[str, Any]]) -> list[dict[str, Any]]:
    rows = load_bench_rows(conn)
    by_id = {row["id"]: row for row in rows}
    examples = []
    for item in oracle:
        fts = fts_scores(conn, item["question"])
        sym = symbolic_scores(item["question"], rows)
        ids = set(fts) | set(sym)
        candidates = []
        for cid in ids:
            row = by_id.get(cid)
            if row is None:
                continue
            candidates.append(
                {
                    "id": cid,
                    "path": row["path"],
                    "vec": 0.0,  # Query embeddings are not in the oracle; keep channel explicit.
                    "fts": fts.get(cid, 0.0),
                    "symbolic": sym.get(cid, 0.0),
                    "decay": decay_score(row["timestamp"]),
                    "answer": is_answer(row["path"], item),
                }
            )
        candidates.sort(
            key=lambda c: 0.4 * c["vec"] + 0.3 * c["fts"] + 0.2 * c["symbolic"] + 0.1 * c["decay"],
            reverse=True,
        )
        examples.append(
            {
                "qid": item["question_id"],
                "question_type": item.get("question_type", "unknown"),
                "candidates": candidates[:TOP_N],
            }
        )
    return examples


def r_at_3(examples: list[dict[str, Any]], weights: tuple[float, float, float, float]) -> float:
    hits = 0
    for ex in examples:
        ranked = sorted(
            ex["candidates"],
            key=lambda c: weights[0] * c["vec"]
            + weights[1] * c["fts"]
            + weights[2] * c["symbolic"]
            + weights[3] * c["decay"],
            reverse=True,
        )
        hits += any(c["answer"] for c in ranked[:3])
    return hits / len(examples) if examples else 0.0


def grid() -> list[tuple[float, float, float, float]]:
    values = [i / 10 for i in range(11)]
    out = []
    for vec in values:
        for fts in values:
            for sym in values:
                decay = round(1.0 - vec - fts - sym, 10)
                if decay < 0:
                    continue
                out.append((vec, fts, sym, decay))
    return out


def best_weights(examples: list[dict[str, Any]]) -> tuple[tuple[float, float, float, float], float]:
    best = ((0.4, 0.3, 0.2, 0.1), -1.0)
    for weights in grid():
        score = r_at_3(examples, weights)
        if score > best[1]:
            best = (weights, score)
    return best


def main() -> None:
    with open(ORACLE_PATH, "r", encoding="utf-8") as f:
        oracle = json.load(f)
    conn = sqlite3.connect(DB_PATH)
    examples = build_examples(conn, oracle)

    by_type: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for ex in examples:
        by_type[ex["question_type"]].append(ex)

    print("dimension,n,best_vec,best_fts,best_symbolic,best_decay,r_at_3")
    weights, score = best_weights(examples)
    print(f"overall,{len(examples)},{weights[0]:.1f},{weights[1]:.1f},{weights[2]:.1f},{weights[3]:.1f},{score:.4f}")
    for qtype, items in sorted(by_type.items()):
        weights, score = best_weights(items)
        print(f"{qtype},{len(items)},{weights[0]:.1f},{weights[1]:.1f},{weights[2]:.1f},{weights[3]:.1f},{score:.4f}")


if __name__ == "__main__":
    main()
