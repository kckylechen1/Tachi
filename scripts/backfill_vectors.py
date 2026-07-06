#!/usr/bin/env python3
"""Backfill missing vector embeddings for Tachi memory DBs.

Usage:
    python3 scripts/backfill_vectors.py <db_path> [--dry-run] [--batch-size 64]

Requires: VOYAGE_API_KEY env var.
"""

import sqlite3
import json
import os
import sys
import time
import struct
import argparse
import urllib.request
import urllib.error

VOYAGE_API_URL = "https://api.voyageai.com/v1/embeddings"
VOYAGE_MODEL = "voyage-4"
EMBEDDING_DIM = 1024
MAX_BATCH = 64  # conservative; API supports 128


def get_missing_entries(conn: sqlite3.Connection) -> list[dict]:
    """Find all memories that lack a vector embedding."""
    cur = conn.execute("""
        SELECT m.rowid, m.id, m.text, m.summary
        FROM memories m
        WHERE m.rowid NOT IN (SELECT rowid FROM memories_vec_rowids)
        ORDER BY m.rowid
    """)
    return [{"rowid": r[0], "id": r[1], "text": r[2], "summary": r[3]} for r in cur.fetchall()]


def embed_batch(texts: list[str], api_key: str) -> list[list[float]]:
    """Call Voyage-4 API to embed a batch of texts."""
    body = json.dumps({
        "model": VOYAGE_MODEL,
        "input": texts,
        "input_type": "document",
    }).encode("utf-8")

    req = urllib.request.Request(
        VOYAGE_API_URL,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
    )

    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                data = json.loads(resp.read())
            return [item["embedding"] for item in data["data"]]
        except urllib.error.HTTPError as e:
            if e.code == 429:
                wait = 2 ** (attempt + 1)
                print(f"  Rate limited, waiting {wait}s...")
                time.sleep(wait)
                continue
            body_text = e.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"Voyage API error {e.code}: {body_text}") from e
        except urllib.error.URLError:
            if attempt < 2:
                time.sleep(2)
                continue
            raise
    raise RuntimeError("Voyage API failed after 3 retries")


def serialize_f32_vec(vec: list[float]) -> bytes:
    """Serialize a float vector to the format sqlite-vec expects (little-endian f32 array)."""
    return struct.pack(f"<{len(vec)}f", *vec)


def insert_vectors(conn: sqlite3.Connection, entries: list[dict], vectors: list[list[float]]):
    """Insert vectors into memories_vec virtual table."""
    for entry, vec in zip(entries, vectors):
        blob = serialize_f32_vec(vec)
        try:
            conn.execute(
                "INSERT INTO memories_vec (rowid, id, embedding) VALUES (?, ?, ?)",
                (entry["rowid"], entry["id"], blob),
            )
        except sqlite3.IntegrityError:
            # Already exists (race condition), skip
            pass
    conn.commit()


def main():
    parser = argparse.ArgumentParser(description="Backfill missing vector embeddings")
    parser.add_argument("db_path", help="Path to the SQLite memory DB")
    parser.add_argument("--dry-run", action="store_true", help="Only count, don't embed")
    parser.add_argument("--batch-size", type=int, default=MAX_BATCH, help="Batch size for API calls")
    args = parser.parse_args()

    api_key = os.environ.get("VOYAGE_API_KEY")
    if not api_key and not args.dry_run:
        print("ERROR: VOYAGE_API_KEY not set", file=sys.stderr)
        sys.exit(1)

    db_path = os.path.expanduser(args.db_path)
    if not os.path.exists(db_path):
        print(f"ERROR: DB not found: {db_path}", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(db_path)
    missing = get_missing_entries(conn)

    total_in_db = conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
    total_with_vec = conn.execute("SELECT COUNT(*) FROM memories_vec_rowids").fetchone()[0]

    print(f"DB: {db_path}")
    print(f"Total memories: {total_in_db}")
    print(f"With vectors:   {total_with_vec}")
    print(f"Missing:        {len(missing)}")

    if args.dry_run or not missing:
        if not missing:
            print("✅ All entries have vectors!")
        conn.close()
        return

    print(f"\nBackfilling {len(missing)} entries (batch size {args.batch_size})...\n")

    processed = 0
    for i in range(0, len(missing), args.batch_size):
        batch = missing[i:i + args.batch_size]
        # Use text for embedding; fall back to summary if text is very short
        texts = []
        for entry in batch:
            t = entry["text"].strip()
            if len(t) < 10 and entry["summary"]:
                t = entry["summary"]
            # Truncate very long texts to ~8000 chars (Voyage token limit)
            if len(t) > 8000:
                t = t[:8000]
            texts.append(t)

        try:
            vectors = embed_batch(texts, api_key)
            insert_vectors(conn, batch, vectors)
            processed += len(batch)
            print(f"  [{processed}/{len(missing)}] embedded batch of {len(batch)}")
        except Exception as e:
            print(f"  ERROR at batch {i}: {e}", file=sys.stderr)
            print(f"  Stopping. {processed} entries saved successfully.")
            break

        # Small delay to respect rate limits
        if i + args.batch_size < len(missing):
            time.sleep(0.3)

    # Final stats
    final_with_vec = conn.execute("SELECT COUNT(*) FROM memories_vec_rowids").fetchone()[0]
    print(f"\n✅ Done! Vectors: {total_with_vec} → {final_with_vec} / {total_in_db}")
    conn.close()


if __name__ == "__main__":
    main()
