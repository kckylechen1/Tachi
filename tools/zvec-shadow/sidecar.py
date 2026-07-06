#!/usr/bin/env python3
"""zvec shadow sidecar (tachi#683 Phase 0, G3).

A standalone process (never in-process with the memory-server daemon -- see
tools/zvec-shadow/README.md for why) that loads a JSONL snapshot produced by
export_snapshot.py into an in-process zvec collection and serves it over a
plain HTTP endpoint on localhost. It never opens the live memory.db; it only
ever reads the snapshot file and writes to its own private zvec collection
directory.

Query modes:
  - FTS-only (default): matches `text`/`--embedding` not supplied. Uses
    zvec's jieba-tokenized full-text index on a merged `search_text` field
    (summary + text), which is the CJK-aware textual retrieval path
    (comparable in spirit to Tachi's own memories_fts).
  - Hybrid dense+FTS: if the caller supplies a 1024-dim `embedding` in the
    request body, dense KNN on `embedding` is combined with the FTS query
    via zvec's RrfReRanker (reciprocal rank fusion), matching the pattern
    already validated in the tachi#683 spike (~/.cache/zvec-spike/smoke.py
    step 2e).

This sidecar does NOT call any embedding API (no query-side embedding is
computed here -- see README for why: the frozen spec forbids paid external
API calls from the sidecar). Query-side dense search is only exercised if
the caller already has an embedding vector to hand it (e.g. reusing the
same Voyage vector a doc already has, for smoke-testing the dense path).

Usage:
    python3 sidecar.py --snapshot snapshot.jsonl --port 8791 \\
        [--collection-dir /tmp/zvec-shadow-col] [--host 127.0.0.1]

Endpoints:
    GET  /health               -> {"status": "ok", "doc_count": N}
    POST /query                -> body: {"text": str, "top_k": int=10, "embedding": [float]*1024?}
                                   resp: {"results": [...], "latency_ms": float, "mode": "fts"|"hybrid"}
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import zvec
from zvec import (
    CollectionSchema, VectorSchema, FieldSchema, DataType,
    MetricType, FlatIndexParam, FtsIndexParam, Fts, Query, Doc,
    create_and_open, RrfReRanker,
)

EMBEDDING_DIM = 1024

# zvec doc ids reject some characters that real Tachi memory ids use (observed:
# handoff-lane ids look like "handoff:<uuid>" and zvec raised "ValueError:
# Invalid doc: ... contains invalid characters" on the literal colon during
# the first real-snapshot load in this spike -- 6/345 docs in the live
# ~/.tachi/global/memory.db snapshot). Rather than dropping those memories
# (which would silently bias the compare.py overlap numbers), we sanitize the
# zvec-side id and keep the real Tachi id in a stored `tachi_id` field, which
# is what query results report back as `id` -- callers never see the
# sanitized form.
_UNSAFE_ID_CHARS = re.compile(r"[^A-Za-z0-9_-]")


def sanitize_id(tachi_id: str) -> str:
    return _UNSAFE_ID_CHARS.sub("_", tachi_id)


def build_schema() -> "CollectionSchema":
    return CollectionSchema(
        name="tachi_shadow",
        fields=[
            FieldSchema(name="tachi_id", data_type=DataType.STRING),
            FieldSchema(name="path", data_type=DataType.STRING),
            FieldSchema(name="category", data_type=DataType.STRING),
            FieldSchema(name="topic", data_type=DataType.STRING),
            FieldSchema(name="summary", data_type=DataType.STRING),
            # Merged FTS field: summary + text, jieba-tokenized (CJK-aware,
            # same tokenizer choice validated in the tachi#683 spike).
            FieldSchema(name="search_text", data_type=DataType.STRING,
                        index_param=FtsIndexParam(tokenizer_name="jieba")),
        ],
        vectors=[
            VectorSchema(name="embedding", data_type=DataType.VECTOR_FP32,
                         dimension=EMBEDDING_DIM,
                         index_param=FlatIndexParam(metric_type=MetricType.COSINE)),
        ],
    )


# Ownership marker dropped inside every collection dir this tool creates.
# load_snapshot() refuses to rmtree a pre-existing directory that does not
# contain it, so a mistyped --collection-dir (e.g. pointing at a real data
# directory) errors out instead of being silently deleted (PR #700
# adjudication, CP4).
OWNERSHIP_MARKER = ".zvec-shadow-owned"


def load_snapshot(collection_dir: str, snapshot_path: str, chunk_size: int = 500) -> "zvec.Collection":
    marker = os.path.join(collection_dir, OWNERSHIP_MARKER)
    zvec_dir = os.path.join(collection_dir, "collection")
    if os.path.exists(collection_dir):
        if os.path.isfile(marker):
            shutil.rmtree(collection_dir)  # our own previous run's data
        elif os.listdir(collection_dir):
            raise SystemExit(
                f"refusing to delete {collection_dir}: it exists, is non-empty, "
                f"and has no {OWNERSHIP_MARKER} marker, so it was not created by "
                "this tool. Pass a fresh directory (or remove it yourself if you "
                "are sure)."
            )
        # else: exists but empty (e.g. freshly mkdtemp'd) -- safe to adopt.
    os.makedirs(collection_dir, exist_ok=True)
    with open(marker, "w") as f:
        f.write("created by tools/zvec-shadow/sidecar.py; safe to delete\n")
    col = create_and_open(zvec_dir, build_schema())

    docs = []
    n_total = 0
    n_missing_vec = 0
    with open(snapshot_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            emb = rec.get("embedding")
            if emb is None or len(emb) != EMBEDDING_DIM:
                n_missing_vec += 1
                emb = [0.0] * EMBEDDING_DIM
            search_text = f"{rec.get('summary') or ''}\n{rec.get('text') or ''}"
            docs.append(Doc(
                id=sanitize_id(rec["id"]),
                vectors={"embedding": np.array(emb, dtype=np.float32)},
                fields={
                    "tachi_id": rec["id"],
                    "path": rec.get("path") or "",
                    "category": rec.get("category") or "",
                    "topic": rec.get("topic") or "",
                    "summary": rec.get("summary") or "",
                    "search_text": search_text,
                },
            ))
            n_total += 1
            if len(docs) >= chunk_size:
                col.insert(docs)
                docs = []
    if docs:
        col.insert(docs)
    col.flush()
    return col, {"n_total": n_total, "n_missing_vec": n_missing_vec}


class Handler(BaseHTTPRequestHandler):
    collection = None  # set by main() before serving
    stats = None

    def log_message(self, fmt, *args):  # quieter default logging
        sys.stderr.write("[sidecar] " + (fmt % args) + "\n")

    def _send_json(self, payload: dict, code: int = 200):
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._send_json({
                "status": "ok",
                "doc_count": self.collection.stats.doc_count,
                "load_stats": self.stats,
            })
        else:
            self._send_json({"error": "not found"}, code=404)

    def do_POST(self):
        if self.path != "/query":
            self._send_json({"error": "not found"}, code=404)
            return
        # All request parsing/validation stays inside error handling: any
        # malformed input must produce a 4xx JSON response, never an
        # unhandled exception in the handler thread (PR #700 adjudication,
        # CP4).
        try:
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or b"{}")
        except (ValueError, json.JSONDecodeError) as e:
            self._send_json({"error": f"invalid request body: {e}"}, code=400)
            return
        if not isinstance(body, dict):
            self._send_json({"error": "request body must be a JSON object"}, code=400)
            return

        text = body.get("text", "")
        if not isinstance(text, str):
            self._send_json({"error": "'text' must be a string"}, code=400)
            return
        try:
            top_k = int(body.get("top_k", 10))
        except (TypeError, ValueError):
            self._send_json({"error": "'top_k' must be an integer"}, code=400)
            return
        if not (1 <= top_k <= 1000):
            self._send_json({"error": "'top_k' must be between 1 and 1000"}, code=400)
            return
        embedding = body.get("embedding")

        t0 = time.perf_counter()
        try:
            if embedding is not None:
                if not isinstance(embedding, list) or len(embedding) != EMBEDDING_DIM:
                    got = len(embedding) if isinstance(embedding, list) else type(embedding).__name__
                    self._send_json({"error": f"embedding must be a list of {EMBEDDING_DIM} numbers, got {got}"}, code=400)
                    return
                try:
                    qvec = np.array(embedding, dtype=np.float32)
                except (TypeError, ValueError) as e:
                    self._send_json({"error": f"embedding must contain only numbers: {e}"}, code=400)
                    return
                results = self.collection.query(
                    queries=[
                        Query(field_name="embedding", vector=qvec),
                        Query(field_name="search_text", fts=Fts(match_string=text)),
                    ],
                    topk=top_k,
                    output_fields=["tachi_id", "path", "category", "topic", "summary"],
                    reranker=RrfReRanker(rank_constant=60),
                )
                mode = "hybrid"
            else:
                results = self.collection.query(
                    queries=Query(field_name="search_text", fts=Fts(match_string=text)),
                    topk=top_k,
                    output_fields=["tachi_id", "path", "category", "topic", "summary"],
                )
                mode = "fts"
        except Exception as e:  # surface zvec errors verbatim to the caller
            self._send_json({"error": f"{type(e).__name__}: {e}"}, code=500)
            return
        latency_ms = (time.perf_counter() - t0) * 1000

        out = [
            {
                "id": r.fields.get("tachi_id", r.id),
                "score": r.score,
                "path": r.fields.get("path"),
                "category": r.fields.get("category"),
                "topic": r.fields.get("topic"),
                "summary": r.fields.get("summary"),
            }
            for r in results
        ]
        self._send_json({"results": out, "latency_ms": latency_ms, "mode": mode})


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--snapshot", required=True, help="JSONL snapshot produced by export_snapshot.py")
    ap.add_argument("--collection-dir", default=None,
                     help="zvec collection directory (default: a fresh temp dir, removed is NOT automatic on exit)")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8791)
    args = ap.parse_args()

    collection_dir = args.collection_dir or tempfile.mkdtemp(prefix="zvec-shadow-col-")
    zvec.init(log_level=zvec.LogLevel.WARN)

    print(f"[sidecar] loading snapshot={args.snapshot} into collection_dir={collection_dir}", flush=True)
    t0 = time.time()
    col, load_stats = load_snapshot(collection_dir, args.snapshot)
    print(f"[sidecar] loaded {load_stats} in {time.time()-t0:.2f}s; doc_count={col.stats.doc_count}", flush=True)

    Handler.collection = col
    Handler.stats = load_stats
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"[sidecar] serving on http://{args.host}:{args.port} (GET /health, POST /query)", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
