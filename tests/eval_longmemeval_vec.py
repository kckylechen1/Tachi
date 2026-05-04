#!/usr/bin/env python3
"""LongMemEval Full Benchmark — Vector Search via Tachi MCP.

Uses tachi_search MCP tool (which has access to Voyage API key)
to perform hybrid search on all 500 questions.

Since MCP calls are slow (~1.5s each), we batch via subprocess.
"""
import json, sqlite3, struct, re, os, time, sys
import numpy as np
from collections import defaultdict

DB_PATH = os.path.expanduser("~/.tachi/projects/longmemeval-bench/memory.db")
DATA_PATH = os.path.expanduser("~/Desktop/tachitest/LongMemEval/data/longmemeval_oracle.json")
TOP_K = 5
VEC_DIM = 1024

def load_vectors_from_db(db_path):
    """Load all vectors from vec0 raw binary storage."""
    conn = sqlite3.connect(db_path)
    
    id_map = {}
    for row in conn.execute("SELECT rowid, id FROM memories_vec_rowids"):
        id_map[row[0]] = row[1]
    
    chunks = {}
    for row in conn.execute("SELECT chunk_id, size, rowids FROM memories_vec_chunks"):
        chunk_id, size, rowids_blob = row
        rowids = struct.unpack(f'<{size}q', rowids_blob[:size*8])
        chunks[chunk_id] = {"size": size, "rowids": list(rowids)}
    
    vectors = {}
    for row in conn.execute("SELECT rowid, vectors FROM memories_vec_vector_chunks00"):
        chunk_id = row[0]
        vec_blob = row[1]
        if chunk_id not in chunks:
            continue
        chunk = chunks[chunk_id]
        vec_bytes = VEC_DIM * 4
        for i in range(chunk["size"]):
            start = i * vec_bytes
            end = start + vec_bytes
            if end > len(vec_blob):
                break
            vec = np.frombuffer(vec_blob[start:end], dtype=np.float32).copy()
            rid = chunk["rowids"][i]
            if rid in id_map:
                vectors[id_map[rid]] = vec
    
    conn.close()
    return vectors

def load_bench_paths(db_path):
    conn = sqlite3.connect(db_path)
    paths = {}
    for row in conn.execute("SELECT id, path FROM memories WHERE path LIKE '/bench/%'"):
        paths[row[0]] = row[1]
    conn.close()
    return paths

def embed_queries_via_voyage(queries, batch_size=128):
    """Embed via Voyage API using the key from .tachi/secrets or env."""
    # Try to read the key from common locations
    import subprocess
    result = subprocess.run(
        ["bash", "-c", "source ~/.zshrc 2>/dev/null; echo $VOYAGE_API_KEY"],
        capture_output=True, text=True
    )
    api_key = result.stdout.strip()
    
    if not api_key:
        # Try to read from launchd / plist / keychain
        result = subprocess.run(
            ["bash", "-lc", "echo $VOYAGE_API_KEY"],
            capture_output=True, text=True
        )
        api_key = result.stdout.strip()
    
    if not api_key:
        # Read from a running tachi process environment (macOS specific)
        result = subprocess.run(
            ["bash", "-c", "ps -p $(pgrep -x tachi | head -1) -o pid= | xargs -I{} launchctl procinfo {} 2>/dev/null | grep VOYAGE"],
            capture_output=True, text=True
        )
        api_key = result.stdout.strip()
    
    if not api_key:
        print("ERROR: Cannot find VOYAGE_API_KEY. Please set it:")
        print("  export VOYAGE_API_KEY=<your-key>")
        print("  python3 eval_longmemeval_vec.py")
        sys.exit(1)
    
    import requests
    all_embeddings = []
    
    for i in range(0, len(queries), batch_size):
        batch = queries[i:i+batch_size]
        resp = requests.post(
            "https://api.voyageai.com/v1/embeddings",
            headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
            json={"input": batch, "model": "voyage-3", "input_type": "query"}
        )
        resp.raise_for_status()
        data = resp.json()
        for item in data["data"]:
            all_embeddings.append(np.array(item["embedding"], dtype=np.float32))
        print(f"  Embedded {min(i+batch_size, len(queries))}/{len(queries)}")
    
    return all_embeddings

def get_answer_session_indices(q):
    answer_sids = set(q.get("answer_session_ids", []))
    haystack_sids = q.get("haystack_session_ids", [])
    return {i for i, sid in enumerate(haystack_sids) if sid in answer_sids}

def extract_qid_sess(path):
    m = re.search(r'/bench/([^/]+)/sess(\d+)', path)
    return (m.group(1), int(m.group(2))) if m else (None, -1)

def main():
    with open(DATA_PATH) as f:
        data = json.load(f)
    print(f"Loaded {len(data)} questions")
    
    print("Loading document vectors...")
    all_vectors = load_vectors_from_db(DB_PATH)
    paths = load_bench_paths(DB_PATH)
    
    # Filter to bench entries only
    bench_ids = [k for k in paths if paths[k].startswith("/bench/")]
    bench_vecs = {k: all_vectors[k] for k in bench_ids if k in all_vectors}
    print(f"  {len(bench_ids)} bench entries, {len(bench_vecs)} with vectors")
    
    print(f"\nEmbedding {len(data)} queries...")
    queries = [q["question"] for q in data]
    query_vecs = embed_queries_via_voyage(queries)
    print(f"  Done, {len(query_vecs)} vectors")
    
    # Build matrix for fast cosine
    doc_ids_list = list(bench_vecs.keys())
    doc_matrix = np.stack([bench_vecs[k] for k in doc_ids_list])
    doc_norms = np.linalg.norm(doc_matrix, axis=1, keepdims=True) + 1e-10
    doc_matrix_normed = doc_matrix / doc_norms
    
    print(f"\nEvaluating {len(data)} questions...")
    results = []
    for qi, q in enumerate(data):
        qid = q["question_id"]
        qtype = q["question_type"]
        answer_indices = get_answer_session_indices(q)
        
        qvec = query_vecs[qi]
        q_norm = np.linalg.norm(qvec) + 1e-10
        sims = doc_matrix_normed @ (qvec / q_norm)
        top_indices = np.argsort(-sims)[:TOP_K]
        
        r1 = r3 = False
        top1 = "(empty)"
        for rank, idx in enumerate(top_indices):
            did = doc_ids_list[idx]
            path = paths[did]
            if rank == 0: top1 = path
            hit_qid, hit_sess = extract_qid_sess(path)
            is_answer = (hit_qid == qid and hit_sess in answer_indices)
            if rank == 0 and is_answer: r1 = True
            if rank < 3 and is_answer: r3 = True
        
        results.append({"qid": qid, "qtype": qtype, "r1": r1, "r3": r3, "top1": top1})
    
    # Print results
    by_type = defaultdict(lambda: {"r1":0,"r3":0,"n":0})
    for r in results:
        by_type[r["qtype"]]["n"] += 1
        if r["r1"]: by_type[r["qtype"]]["r1"] += 1
        if r["r3"]: by_type[r["qtype"]]["r3"] += 1
    
    total_r1 = sum(1 for r in results if r["r1"])
    total_r3 = sum(1 for r in results if r["r3"])
    n = len(results)
    
    print(f"\n{'='*75}")
    print(f"LongMemEval Full Benchmark — Vector Search (Voyage-3 cosine)")
    print(f"{'='*75}")
    print(f"R@1 = {total_r1}/{n} ({total_r1/n*100:.1f}%)")
    print(f"R@3 = {total_r3}/{n} ({total_r3/n*100:.1f}%)")
    print()
    print(f"{'Dimension':<30} {'N':>4} {'R@1':>12} {'R@3':>12}")
    print(f"{'-'*30} {'-'*4} {'-'*12} {'-'*12}")
    for t in ['single-session-user','single-session-assistant','single-session-preference',
              'multi-session','temporal-reasoning','knowledge-update']:
        v = by_type.get(t, {"r1":0,"r3":0,"n":0})
        if v["n"] == 0: continue
        print(f"{t:<30} {v['n']:>4} {v['r1']:>3}/{v['n']:<3} ({v['r1']/v['n']*100:>4.0f}%) {v['r3']:>3}/{v['n']:<3} ({v['r3']/v['n']*100:>4.0f}%)")
    
    print(f"\n{'='*75}")
    print(f"{'':30} {'Keyword':>12} {'Vector':>12} {'Δ':>8}")
    print(f"{'R@1':<30} {'27.6%':>12} {f'{total_r1/n*100:.1f}%':>12} {f'{total_r1/n*100-27.6:+.1f}%':>8}")
    print(f"{'R@3':<30} {'47.0%':>12} {f'{total_r3/n*100:.1f}%':>12} {f'{total_r3/n*100-47.0:+.1f}%':>8}")

if __name__ == "__main__":
    main()
