# zvec shadow sidecar (tachi#683 Phase 0)

A standalone toolkit that shadows Tachi's memory search against
[zvec](https://github.com/alibaba/zvec) (an in-process vector+FTS library)
**without touching the memory-server daemon**. Zero lines changed under
`crates/`. Everything here is a separate process that reads an exported,
read-only snapshot of `memory.db` and never opens the live database for
writing.

## Why sidecar, never in-process (frozen decision, not re-litigated here)

The tachi#683 spike (`~/.cache/zvec-spike/`) found zvec has excellent
performance (2k-doc dense top-10 p50=0.26ms) and genuinely good Chinese FTS
(pinyin-confusable discrimination works), but two properties make in-process
linking into the daemon unacceptable:

- **Concurrency is 1-writer XOR N-readers, not 1-writer + N-readers.** While
  one process holds a collection open, other *processes* cannot even open it
  read-only (`Can't lock read-only collection: LOCK`).
- **Unflushed writes vanish wholesale on kill -9** (96k docs evaporated in
  one spike run). zvec's own release notes mention SIGABRT-class fixes; a
  C++ `abort()` inside zvec linked into the daemon's address space would
  take the whole daemon down with it.

So: **SQLite remains the only system of record.** zvec here is a
fully-rebuildable derived index, fed by exported snapshots, running in its
own process, queried over a plain HTTP endpoint. If this sidecar crashes,
leaks, or gets its process killed mid-write, nothing about the real memory
data is at risk -- worst case, re-run the export+load.

## Architecture

```
 ~/.tachi/global/memory.db  (live, WAL, owned by the memory-server daemon)
            |
            |  read-only SQLite connection (mode=ro), momentary shared lock,
            |  loads the sqlite-vec extension to read the memories_vec
            |  vec0 virtual table -- never a write connection, never
            |  touches the live file otherwise
            v
   export_snapshot.py  --out snapshot.jsonl
            |
            |  one JSON object per memory: id/path/summary/text/keywords/
            |  category/topic/importance/timestamp/created_at/archived/
            |  embedding (1024-dim, reused verbatim from the doc's own
            |  Voyage-4 vector already stored in memories_vec -- no
            |  embedding API is called by anything in this directory)
            v
      sidecar.py  --snapshot snapshot.jsonl --port 8791
            |
            |  loads the JSONL into a *fresh, private* zvec collection
            |  (own temp dir, never the live memory.db), flush()es once,
            |  then serves:
            |    GET  /health
            |    POST /query  {"text": ..., "top_k": 10, "embedding": [..]?}
            v
      compare.py  --snapshot snapshot.jsonl --sidecar-url http://127.0.0.1:8791
            |
            |  for each of 20 queries derived from real memories' own
            |  summaries: fires the query at BOTH `tachi search` (read-only
            |  CLI path) and the sidecar's /query, records top-10 ids +
            |  latency for each, computes overlap@10 and hit@10
            v
   reports/compare_*.jsonl + reports/compare_*.md
```

## How to run it end-to-end

```bash
cd tools/zvec-shadow
python3 -m pip install -r requirements.txt   # zvec, numpy, sqlite-vec

# 1. Export a snapshot from the live global memory.db (read-only; never
#    writes to it; safe to run while the daemon is up).
python3 export_snapshot.py --db ~/.tachi/global/memory.db --out snapshot.jsonl

# 2. Start the sidecar (loads the snapshot into its own private zvec
#    collection, then serves HTTP on :8791).
python3 sidecar.py --snapshot snapshot.jsonl --port 8791 &

# 3. Run the flush/kill-9 durability probe (independent of 1-2; uses its
#    own scratch collections).
python3 probe_flush.py

# 4. Compare tachi search vs the sidecar on a simple labeled query set.
python3 compare.py --snapshot snapshot.jsonl --n-queries 20
python3 compare.py --snapshot snapshot.jsonl --n-queries 20 --hybrid  # + doc-side embeddings

# 5. (Separately, not part of the snapshot/sidecar pipeline) probe whether
#    the Rust binding builds on this machine:
bash probe_rust_binding.sh
```

**Privacy note:** `snapshot.jsonl` and everything under `reports/` are
generated from *your real, private* Tachi memories (verbatim summaries,
text, ids) and are gitignored on purpose (see `.gitignore`) -- never commit
them. `FINDINGS.md` only carries aggregate, content-free numbers from a
real run (hit rates, overlap counts, latencies), never the underlying query
text or memory content.

`FINDINGS.md` has the full G1 (Rust binding) and G2 (flush semantics)
results and transcripts, plus operational quirks discovered while wiring up
G3/G4 (a `tachi search` scope-routing footgun and a response-shape
flakiness under daemon contention -- both worth a standalone bug report
against tachi, independent of #683).

## Mapping to tachi#683 acceptance criteria

| # | Acceptance criterion (verbatim from the issue) | Status this PR |
|---|---|---|
| 1 | Sidecar shadow process + snapshot export path; zero changes to memory-server API or callers | **Done.** `export_snapshot.py` + `sidecar.py`; `git diff --stat origin/main` has zero `crates/` lines (see verification section of the PR). |
| 2 | Flush/commit semantics characterized: what survives kill -9 after flush? Documented + tested | **Done.** `probe_flush.py` + `FINDINGS.md` G2: `flush()` is the sole, precise durability boundary; 3/3 scenarios pass. |
| 3 | Rust binding (zvec-ai/zvec-rust) builds and passes the same smoke on this machine | **Partially done.** Builds and runs cleanly at pinned tag `v0.5.0` (`FINDINGS.md` G1) -- but this ran the crate's own bundled `basic` example, not a line-for-line Rust port of `~/.cache/zvec-spike/smoke.py`'s 2k-doc Chinese/English + latency-percentile suite. Porting that specific smoke test to Rust was judged out of scope for a 45-minute-boxed G1 feasibility check; flag as a follow-up if criterion 3 is read strictly. |
| 4 | Golden set encoded as `recall_simulate` cases; zvec variant beats or ties current backend on Recall@10/MRR; vector-score non-zero rate >95% | **Deferred to Phase 1** (explicit in the frozen spec for this PR). `compare.py` is a simpler stand-in: 20 summary-derived queries, hit@10/overlap@10/latency, not wired to `recall_simulate`. See "Phase 1 integration path" below. |
| 5 | Rebuild-from-SQLite-metadata drill: full rebuild then all goldens still pass | **Mechanism proven, goldens not yet frozen.** Every `sidecar.py` run *is* a full rebuild from SQLite-exported metadata (no persistent zvec state carried between runs); this PR exercised that rebuild path successfully end-to-end (see compare reports). Formal "goldens" don't exist yet -- they arrive with criterion 4's `recall_simulate` integration. |
| 6 | p95 latency <100ms at 10k records (headroom check) | **Not re-verified at 10k in this PR.** The live global DB only has 345 non-archived memories; this PR's own numbers (sidecar server-side query latency: ~1.5-4ms FTS, low ms hybrid, on 345 docs) are consistent with the original spike's 2k-doc dense p50=0.26ms, but nobody generated a 10k-record snapshot here to re-confirm headroom at that scale. |
| 7 | Gate flag: shadow results never reach user-visible output until explicitly opened | **N/A by construction.** There is no integration point into memory-server yet (zero `crates/` changes), so there is nothing for a gate flag to gate. This becomes relevant only once Phase 1 wires a `VectorBackend` trait into a real query path. |

## Phase 1 (explicitly NOT in this PR)

- Trait-ifying the vector backend (`VectorBackend` or similar) behind the
  async writer-queue seam referenced in the issue (#520/#546) -- this PR
  does not touch `crates/` at all, by the frozen spec for Phase 0.
- Wiring the compare harness into `recall_simulate` variants: the natural
  seam is to encode each `compare.py` query (query text + `expected_id`) as
  a `recall_simulate` case pair, run it once against the existing
  sqlite-vec/FTS backend and once against a zvec-backed variant, and let
  `recall_simulate`'s existing Recall@k/MRR scoring do the comparison
  instead of this PR's simpler hit@10/overlap@10. `compare.py`'s
  `load_query_set()` + the JSONL it emits are already shaped to make that
  port mechanical (one query, one expected id, per line).
- Query-side embeddings: this sidecar never calls an embedding API (frozen
  constraint), so its dense path only fires when a caller already has a
  vector to hand it (demonstrated in `--hybrid` mode by reusing a memory's
  own stored Voyage-4 vector as a smoke test, not a real independent
  query-embedding pipeline). A real query-side embedding path is a Phase 1
  decision, tied to whatever the writer-queue/trait design decides about
  where embedding calls live.
- Testing the zvec-rust auto-build-from-CMake-source tier (tier 6 of
  `zvec-sys/build.rs`'s resolution order) -- this PR's G1 only exercised the
  prebuilt-dylib-download tier (tier 5), which is what actually fired on
  this machine.
- Note on issue sequencing: the issue's own "Sequencing" section says this
  work should land "after #501 collapse completes and the async-writer-queue
  design lands... Not before." This PR was dispatched as Phase 0 ahead of
  that stated sequencing; flagging the discrepancy here for the adjudicator
  rather than silently overriding either instruction.
