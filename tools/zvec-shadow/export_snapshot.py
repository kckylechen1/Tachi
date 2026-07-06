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


def export(db_path: str, out_path: str, include_archived: bool, limit: int | None) -> dict:
    conn = open_source_readonly(db_path)
    where = "" if include_archived else "WHERE m.archived = 0"
    limit_sql = f"LIMIT {int(limit)}" if limit else ""
    sql = f"""
        SELECT m.id, m.path, m.summary, m.text, m.keywords, m.category, m.topic,
               m.importance, m.timestamp, m.created_at, m.archived, v.embedding
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
    with open(out_path, "w", encoding="utf-8") as f:
        for row in cur:
            (mid, path, summary, text, keywords, category, topic,
             importance, timestamp, created_at, archived, emb_blob) = row
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
