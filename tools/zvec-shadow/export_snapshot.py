#!/usr/bin/env python3
"""Export a read-only snapshot of a Tachi memory.db for the zvec shadow sidecar
(tachi#683 Phase 0, G3).

This NEVER writes to the source database and never holds it open for longer
than the export query. It opens the source with `mode=ro` (SQLite read-only
connection, URI form) so it cannot corrupt or lock out the live daemon that
owns that DB -- it takes a momentary shared read lock exactly like any other
SQLite reader, which is safe to run against a live WAL-mode writer.

Schema note (verified by reading crates/memory-core source, not guessed):
  - `memories` table: crates/memory-core/src/db/schema/ddl.rs -- there is no
    `domain` column on `memories` itself (a separate `domains` config table
    exists for per-domain GC settings, unrelated to per-row tagging). The
    per-row taxonomy fields actually present are `category` and `topic`.
    This script exports the REAL columns, not the ones a spec might guess.
  - Vector storage: `memories_vec` is a sqlite-vec `vec0` virtual table
    (crates/memory-core/src/db/sqlite_vec.rs), schema
    `vec0(id TEXT PRIMARY KEY, embedding float[1024])`, joined to `memories`
    on `id` (no separate FK column). Embedding blob format is raw
    little-endian float32, no header (`serialize_f32` in the same file) --
    i.e. exactly `numpy.frombuffer(blob, dtype='<f4')`.
  - Embedding dimension is 1024 (Voyage-4), per
    crates/memory-server/src/status_ops/mod.rs EXPECTED_EMBEDDING_DIM.

Corpus-alignment note (adjudication of PR #700, CP1/CP3): the exported
snapshot must match what `tachi search` can actually return BY DEFAULT,
otherwise the compare harness treats rows Tachi deliberately excludes as
"expected hits" and Tachi's hit rate is artificially depressed. Filtering
`archived = 0` alone is NOT equivalent to Tachi's default retrievable
corpus. The full default exclusion set, translated predicate-by-predicate
from the Rust source (see `tachi_default_retrievable()` below for per-line
citations):
  - superseded rows (`superseded_by IS NOT NULL`)
  - training-seed rows (/sft namespace, sft-memory topic, sft_seed source,
    metadata.training_sample)
  - recall-cache rows (foundry_recall_rerank_cache source/topic/path/etc.)
  - eval rows (/eval namespace, eval category)
  - namespace search noise (wiki_log, kanban, handoff rows) -- excluded by
    the core ranking layer for any unscoped search
Excluded rows are dropped from the snapshot entirely (per-reason counts are
reported in the stats output), so the sidecar corpus and the compare
labeled set are both aligned with Tachi's default retrievable corpus.

Querying `memories_vec` from Python requires the sqlite-vec extension to be
loaded on the connection (it's a virtual table module; the pip package
`sqlite-vec` provides a loadable macOS/Linux binary via `sqlite_vec.load()`).
Without it SQLite will refuse to even prepare a statement that references
the table ("no such module: vec0").

Output: JSONL, one memory per line, with keys:
  id, path, summary, text, keywords (raw JSON string as stored), category,
  topic, importance, timestamp, created_at, archived, embedding (list[1024
  floats], or null if the memory has no vector row).

Usage:
    python3 export_snapshot.py \\
        --db ~/.tachi/global/memory.db \\
        --out snapshot.jsonl \\
        [--include-archived] [--limit N]
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time

try:
    import sqlite_vec
except ImportError:
    print(
        "error: the 'sqlite-vec' pip package is required to read the memories_vec "
        "vec0 virtual table (pip install sqlite-vec).",
        file=sys.stderr,
    )
    raise

import struct


def open_source_readonly(db_path: str) -> sqlite3.Connection:
    """Open the source memory.db strictly read-only, then load the sqlite-vec
    extension so the memories_vec vec0 virtual table can be queried. Never
    writes to db_path; never uses SQLITE_OPEN_READWRITE."""
    uri = f"file:{db_path}?mode=ro"
    # busy_timeout: if the daemon is mid-write we'd rather wait briefly than
    # fail immediately with "database is locked" (observed once during the
    # tachi#683 probe when hitting the DB right as the daemon was writing).
    conn = sqlite3.connect(uri, uri=True, timeout=10.0)
    conn.execute("PRAGMA busy_timeout = 10000")
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    return conn


def unpack_embedding(blob: bytes | None) -> list[float] | None:
    if blob is None:
        return None
    n = len(blob) // 4
    return list(struct.unpack(f"<{n}f", blob))


# ---------------------------------------------------------------------------
# Tachi default-retrievability predicate.
#
# Python translation of the exclusion filters Tachi applies to every default
# (unscoped, no opt-in flags) search. Each helper cites the Rust source it
# mirrors; if those files change, this predicate must be re-verified against
# them. All *_ci comparisons mirror Rust's eq_ignore_ascii_case; metadata
# boolean checks mirror metadata_bool() (serde as_bool -- strict JSON true,
# not truthiness), and metadata string checks mirror metadata_str_eq().
# ---------------------------------------------------------------------------

def _meta_bool(metadata: dict, key: str) -> bool:
    # crates/memory-core/src/namespace.rs:10-16 (metadata_bool)
    return metadata.get(key) is True


def _meta_str_eq(metadata: dict, key: str, expected: str) -> bool:
    # crates/memory-core/src/namespace.rs:18-24 (metadata_str_eq)
    v = metadata.get(key)
    return isinstance(v, str) and v.lower() == expected.lower()


def _path_in_namespace(path: str, namespace: str) -> bool:
    # crates/memory-core/src/namespace.rs:26-29 (path_in_namespace)
    namespace = namespace.rstrip("/")
    return path == namespace or path.startswith(namespace + "/")


def _path_contains_recall_cache(path: str) -> bool:
    # crates/memory-core/src/namespace.rs:31-36 (path_contains_recall_cache)
    return (
        path == "/recall-cache"
        or path.endswith("/recall-cache")
        or "/recall-cache/" in path
        or "foundry_recall_rerank_cache" in path
    )


def _is_training_seed(rec: dict, metadata: dict) -> bool:
    # crates/memory-server/src/memory_search_ops/auto_link.rs:16-27
    # (is_training_seed), applied to every non-opted-in search at
    # crates/memory-server/src/memory_search_ops/search_memory/rows.rs:359-361.
    return (
        rec["path"] == "/sft"
        or rec["path"].startswith("/sft/")
        or rec["topic"].lower() == "sft-memory"
        or rec["source"].lower() == "sft_seed"
        or _meta_bool(metadata, "training_sample")
    )


def _is_recall_cache_entry(rec: dict, metadata: dict) -> bool:
    # crates/memory-core/src/namespace.rs:45-57 (is_recall_cache_entry),
    # applied at .../search_memory/rows.rs:362-364.
    src = "foundry_recall_rerank_cache"
    return (
        rec["source"].lower() == src
        or rec["topic"].lower() in (src, "recall_rerank_cache")
        or rec["id"].startswith("foundry:recall-cache:")
        or _path_contains_recall_cache(rec["path"])
        or _meta_bool(metadata, "recall_rerank_cache")
        or _meta_str_eq(metadata, "cache_key", src)
    )


def _is_eval_entry(rec: dict) -> bool:
    # crates/memory-core/src/namespace.rs:82-84 (is_eval_entry) UNION
    # crates/memory-server/src/memory_search_ops/search_memory/filters.rs:9-15
    # (is_eval_path wrapper), applied at .../search_memory/rows.rs:365-366.
    return _path_in_namespace(rec["path"], "/eval") or rec["category"].lower() == "eval"


def _is_namespace_search_noise(rec: dict, metadata: dict) -> bool:
    # crates/memory-core/src/namespace.rs:88-99 (is_namespace_search_noise)
    # for the UNSCOPED case (path_prefix=None -- the compare harness never
    # passes a path prefix), applied inside core ranking at
    # crates/memory-core/src/search/ranking.rs:57 via
    # crates/memory-core/src/search/filtering.rs:115-117. NOTE: this filter
    # is NOT part of the rows.rs:359-366 set the PR #700 adjudication cited,
    # but it demonstrably makes kanban/handoff/wiki_log rows non-retrievable
    # in a default search, and the adjudication's own labeled-set invariant
    # ("expected hits must all be Tachi-default-retrievable") requires
    # excluding them too (the live global DB has 6 handoff rows).
    wiki_log = (
        rec["path"] == "/wiki/_log"
        or _meta_bool(metadata, "wiki_log")
        or rec["topic"].lower() == "wiki_log"
    )
    # crates/memory-core/src/namespace.rs:70-74 (is_kanban_entry)
    kanban = (
        _path_in_namespace(rec["path"], "/kanban")
        or rec["source"].lower() == "kanban"
        or rec["category"].lower() == "kanban"
    )
    # crates/memory-core/src/namespace.rs:76-80 (is_handoff_entry)
    handoff = (
        _path_in_namespace(rec["path"], "/handoff")
        or rec["source"].lower() == "handoff"
        or rec["category"].lower() == "handoff"
    )
    return wiki_log or kanban or handoff


def tachi_default_retrievable(rec: dict) -> str | None:
    """Return None if the row is retrievable by a default `tachi search`,
    else the exclusion reason. `rec` must carry id/path/topic/source/
    category/superseded_by/metadata(raw JSON str)."""
    try:
        metadata = json.loads(rec.get("metadata") or "{}")
        if not isinstance(metadata, dict):
            metadata = {}
    except json.JSONDecodeError:
        metadata = {}
    # Default include_superseded=false: crates/memory-core/src/search.rs:74-92
    # (SearchOptions::default); enforced in SQL at
    # crates/memory-core/src/db/memory_crud/search.rs:30-35.
    if rec.get("superseded_by") is not None:
        return "superseded"
    if _is_training_seed(rec, metadata):
        return "training_seed"
    if _is_recall_cache_entry(rec, metadata):
        return "recall_cache"
    if _is_eval_entry(rec):
        return "eval"
    if _is_namespace_search_noise(rec, metadata):
        return "namespace_noise"
    return None


def export(db_path: str, out_path: str, include_archived: bool, limit: int | None) -> dict:
    conn = open_source_readonly(db_path)
    where = "" if include_archived else "WHERE m.archived = 0"
    limit_sql = f"LIMIT {int(limit)}" if limit else ""
    sql = f"""
        SELECT m.id, m.path, m.summary, m.text, m.keywords, m.category, m.topic,
               m.importance, m.timestamp, m.created_at, m.archived,
               m.source, m.metadata, m.superseded_by, v.embedding
        FROM memories m
        LEFT JOIN memories_vec v ON v.id = m.id
        {where}
        ORDER BY m.created_at DESC
        {limit_sql}
    """
    t0 = time.time()
    cur = conn.execute(sql)
    n_total = 0
    n_with_vec = 0
    excluded: dict[str, int] = {}
    with open(out_path, "w", encoding="utf-8") as f:
        for row in cur:
            (mid, path, summary, text, keywords, category, topic,
             importance, timestamp, created_at, archived,
             source, metadata, superseded_by, emb_blob) = row
            probe = {
                "id": mid, "path": path or "", "topic": topic or "",
                "source": source or "", "category": category or "",
                "superseded_by": superseded_by, "metadata": metadata,
            }
            reason = tachi_default_retrievable(probe)
            if reason is not None:
                excluded[reason] = excluded.get(reason, 0) + 1
                continue
            embedding = unpack_embedding(emb_blob)
            if embedding is not None:
                n_with_vec += 1
            rec = {
                "id": mid,
                "path": path,
                "summary": summary,
                "text": text,
                "keywords": keywords,
                "category": category,
                "topic": topic,
                "importance": importance,
                "timestamp": timestamp,
                "created_at": created_at,
                "archived": bool(archived),
                "embedding": embedding,
            }
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            n_total += 1
    conn.close()
    wall = time.time() - t0
    return {
        "db_path": db_path,
        "out_path": out_path,
        "n_exported": n_total,
        "n_with_embedding": n_with_vec,
        "n_excluded_not_default_retrievable": dict(sorted(excluded.items())),
        "wall_s": round(wall, 3),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default=os.path.expanduser("~/.tachi/global/memory.db"),
                     help="Source memory.db path (opened read-only; default: ~/.tachi/global/memory.db)")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "snapshot.jsonl"),
                     help="Output JSONL snapshot path")
    ap.add_argument("--include-archived", action="store_true",
                     help="Include archived=1 memories (default: excluded)")
    ap.add_argument("--limit", type=int, default=None, help="Cap number of exported rows (debug convenience)")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"error: source db not found: {args.db}", file=sys.stderr)
        return 2

    stats = export(args.db, args.out, args.include_archived, args.limit)
    print(json.dumps(stats, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
