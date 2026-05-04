#!/usr/bin/env python3
"""LongMemEval Full Benchmark — Ingest + Evaluate against Tachi project DB.

Directly writes to SQLite for speed, then evaluates keyword-based retrieval.
Usage: python3 eval_longmemeval_full.py
"""
import json, sqlite3, re, os, uuid, time
from datetime import datetime, timezone
from collections import Counter, defaultdict

# --- Config ---
DB_PATH = os.path.expanduser("~/.tachi/projects/longmemeval-bench/memory.db")
DATA_PATH = os.path.expanduser("~/Desktop/tachitest/LongMemEval/data/longmemeval_oracle.json")
TOP_K = 5

def clear_bench_entries(conn):
    """Remove all benchmark entries, keep schema intact."""
    conn.execute("DELETE FROM memories WHERE path LIKE '/bench/%'")
    conn.commit()
    remaining = conn.execute("SELECT count(*) FROM memories").fetchone()[0]
    print(f"  Cleared bench entries. {remaining} non-bench entries remain.")

def ingest_all(conn, data):
    """Ingest all sessions into the DB."""
    now = datetime.now(timezone.utc).isoformat()
    inserted = 0
    for q in data:
        qid = q["question_id"]
        dates = q.get("haystack_dates", [])
        for i, session in enumerate(q["haystack_sessions"]):
            date = dates[i] if i < len(dates) else "unknown"
            lines = [f"[Date: {date}]"]
            for turn in session:
                lines.append(f"{turn['role']}: {turn['content']}")
            text = "\n".join(lines)
            
            entry_id = str(uuid.uuid4())
            path = f"/bench/{qid}/sess{i}"
            
            conn.execute("""
                INSERT OR REPLACE INTO memories 
                (id, text, summary, timestamp, scope, category, topic, domain,
                 path, importance, keywords, entities, archived, retention_policy, source)
                VALUES (?, ?, '', ?, 'project', 'fact', '', 'longmemeval',
                        ?, 0.7, '[]', '[]', 0, 'durable', 'manual')
            """, (entry_id, text, now, path))
            inserted += 1
    
    conn.commit()
    print(f"  Ingested {inserted} sessions")
    return inserted

def search_bench(conn, query, top_k=TOP_K):
    """Keyword-count based search, scoped to /bench/ entries only."""
    words = re.findall(r'[a-zA-Z]{3,}', query.lower())
    # Filter common English stop words
    stops = {'the','and','for','are','but','not','you','all','can','had','her','was',
             'one','our','out','has','how','did','get','its','may','new','now','old',
             'see','way','who','did','got','let','say','she','too','use','him','his',
             'what','when','which','with','have','this','will','your','from','they',
             'been','call','come','each','make','like','long','look','many','some',
             'than','them','then','these','time','very','want','that','about','after',
             'first','also','just','more','into','before','between'}
    words = [w for w in words if w not in stops][:12]
    if not words:
        return []
    
    case_parts = []
    params = []
    for w in words:
        case_parts.append("(CASE WHEN LOWER(m.text) LIKE ? THEN 1 ELSE 0 END)")
        params.append(f"%{w}%")
    
    score_expr = " + ".join(case_parts)
    all_params = params + params + [top_k]
    
    rows = conn.execute(f"""
        SELECT m.id, m.path, ({score_expr}) as score
        FROM memories m
        WHERE m.path LIKE '/bench/%'
        AND ({score_expr}) > 0
        ORDER BY score DESC
        LIMIT ?
    """, all_params).fetchall()
    return [(r[0], r[1], r[2]) for r in rows]

def get_answer_session_indices(q):
    """Map answer_session_ids to indices in haystack_sessions."""
    answer_sids = set(q.get("answer_session_ids", []))
    haystack_sids = q.get("haystack_session_ids", [])
    return {i for i, sid in enumerate(haystack_sids) if sid in answer_sids}

def extract_qid_sess(path):
    m = re.search(r'/bench/([^/]+)/sess(\d+)', path)
    return (m.group(1), int(m.group(2))) if m else (None, -1)

def evaluate(conn, data):
    """Run evaluation on all questions."""
    results = []
    for q in data:
        qid = q["question_id"]
        question = q["question"]
        qtype = q["question_type"]
        answer_indices = get_answer_session_indices(q)
        
        rows = search_bench(conn, question, TOP_K)
        
        r1 = False
        r3 = False
        for i, (_, path, score) in enumerate(rows[:TOP_K]):
            hit_qid, hit_sess = extract_qid_sess(path)
            is_answer = (hit_qid == qid and hit_sess in answer_indices)
            if i == 0 and is_answer:
                r1 = True
            if i < 3 and is_answer:
                r3 = True
        
        results.append({
            "qid": qid, "qtype": qtype, "r1": r1, "r3": r3,
            "n_results": len(rows),
            "top1_path": rows[0][1] if rows else "(empty)",
        })
    return results

def print_results(results):
    by_type = defaultdict(lambda: {"r1": 0, "r3": 0, "n": 0, "empty": 0})
    
    for r in results:
        t = r["qtype"]
        by_type[t]["n"] += 1
        if r["r1"]: by_type[t]["r1"] += 1
        if r["r3"]: by_type[t]["r3"] += 1
        if r["n_results"] == 0: by_type[t]["empty"] += 1
    
    total_r1 = sum(1 for r in results if r["r1"])
    total_r3 = sum(1 for r in results if r["r3"])
    n = len(results)
    
    print(f"\n{'='*75}")
    print(f"LongMemEval Full Benchmark — Tachi Keyword Retrieval")
    print(f"{'='*75}")
    print(f"Total: {n} questions, R@1 = {total_r1}/{n} ({total_r1/n*100:.1f}%), R@3 = {total_r3}/{n} ({total_r3/n*100:.1f}%)")
    print()
    print(f"{'Dimension':<30} {'N':>4} {'R@1':>8} {'R@3':>8} {'Empty':>6}")
    print(f"{'-'*30} {'-'*4} {'-'*8} {'-'*8} {'-'*6}")
    
    for t in ['single-session-user', 'single-session-assistant', 'single-session-preference',
              'multi-session', 'temporal-reasoning', 'knowledge-update']:
        v = by_type.get(t, {"r1":0,"r3":0,"n":0,"empty":0})
        if v["n"] == 0:
            continue
        r1_pct = f"{v['r1']}/{v['n']} ({v['r1']/v['n']*100:.0f}%)"
        r3_pct = f"{v['r3']}/{v['n']} ({v['r3']/v['n']*100:.0f}%)"
        print(f"{t:<30} {v['n']:>4} {r1_pct:>8} {r3_pct:>8} {v['empty']:>6}")
    
    print(f"\n{'='*75}")
    
    # Print some failure examples
    failures = [r for r in results if not r["r3"]]
    if failures:
        print(f"\nR@3 Failures ({len(failures)}):")
        for r in failures[:10]:
            print(f"  [{r['qtype'][:15]}] {r['qid']}: top1={r['top1_path'][:40]}")

def main():
    print(f"Loading data from {DATA_PATH}...")
    with open(DATA_PATH) as f:
        data = json.load(f)
    print(f"  {len(data)} questions, {sum(len(q['haystack_sessions']) for q in data)} sessions")
    
    conn = sqlite3.connect(DB_PATH)
    
    print(f"\nPhase 1: Ingest")
    clear_bench_entries(conn)
    t0 = time.time()
    n = ingest_all(conn, data)
    t1 = time.time()
    print(f"  Done in {t1-t0:.1f}s ({n/(t1-t0):.0f} sessions/sec)")
    
    verify = conn.execute("SELECT count(*) FROM memories WHERE path LIKE '/bench/%'").fetchone()[0]
    print(f"  Verified: {verify} bench entries in DB")
    
    print(f"\nPhase 2: Evaluate")
    t0 = time.time()
    results = evaluate(conn, data)
    t1 = time.time()
    print(f"  Done in {t1-t0:.1f}s ({len(data)/(t1-t0):.0f} questions/sec)")
    
    conn.close()
    
    print_results(results)

if __name__ == "__main__":
    main()
