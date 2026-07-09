# zvec Phase 0 shadow sidecar -- findings (tachi#683)

Environment: macOS arm64, rustc/cargo 1.96.1 (Homebrew), Python 3.14.6,
zvec==0.5.1 (Python), zvec-rust tag v0.5.0, sqlite-vec==0.1.9 (Python).
Date: 2026-07-06.

## G1 -- Rust binding feasibility: **SUCCESS**

Built `zvec-ai/zvec-rust` at the pinned release tag `v0.5.0` (NOT `main`)
against a fresh `CARGO_TARGET_DIR` on this machine.

Reproduction: `tools/zvec-shadow/probe_rust_binding.sh` (clones to
`~/.cache/zvec-shadow-rust-probe` by default, does not touch this repo).

```
$ export CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target
$ cd <zvec-rust checkout>; git checkout v0.5.0
$ cargo build -p zvec
   Compiling zvec-sys v0.5.0 (.../zvec-sys)
warning: zvec-sys@0.5.0: Downloading prebuilt zvec library for aarch64-apple-darwin from
  https://github.com/zvec-ai/zvec-rust/releases/download/v0.5.0/zvec-prebuilt-aarch64-apple-darwin.tar.gz
warning: zvec-sys@0.5.0: Successfully downloaded prebuilt library to .../out/zvec-prebuilt
   Compiling zvec v0.5.0 (.../zvec)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.14s
cargo build -p zvec  0.74s user 0.50s system 19% cpu 6.172 total

$ cargo run -p zvec --example basic
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.37s
     Running `.../target/debug/examples/basic`
zvec version: v0.5.0-4-g2a9837e
Collection created successfully!
Inserted 5 docs (errors: 0)

Vector search results (top 3):
  #1: pk=doc_1, similarity=0.0000
  #2: pk=doc_3, similarity=0.0005
  #3: pk=doc_0, similarity=0.0311

Fetched 2 documents by PK:
  pk=doc_0
  pk=doc_2

Collection stats:
  doc_count: 5
  index 'embedding': 0.0% complete

Deleted 1 docs (errors: 0)

Done!
```

**What this means / doesn't mean:**
- `zvec-sys`'s build script (`zvec-sys/build.rs`) has a 6-tier library
  resolution order (env var -> sibling checkout -> git submodule -> vendor
  dir -> **downloaded prebuilt dylib** -> auto-build from source via
  CMake). On this machine it hit tier 5 (prebuilt download), so **no C++
  toolchain / CMake build was ever exercised** by this probe. If a Phase 1
  Rust sidecar needs to target a platform without a prebuilt release asset,
  tier 6 (auto-clone `alibaba/zvec` + CMake) is untested here and is a
  separate, larger feasibility question.
- This says nothing about whether the Rust binding is *safe to link
  in-process into the tachi-server daemon* -- that question is closed by
  the tachi#683 decision already: never in-process, sidecar only, because a
  C++ `abort()` inside zvec takes the whole host process down with it,
  Rust FFI included. G1 is scoped purely to "can a Rust *sidecar* process
  build and drive zvec on this machine," which is now demonstrated feasible
  as a Phase 1 option alongside the Python sidecar this PR ships.
- Build+run wall time was ~15s total including all Rust dependency
  compilation (criterion, rayon, etc. pulled in by the `zvec` crate's dev
  deps for the example) -- not a representative steady-state number, just
  evidence the pipeline works end-to-end.

## G2 -- flush()/kill -9 durability semantics: **flush() is a precise, synchronous PROCESS-CRASH-level durability boundary**

Script: `tools/zvec-shadow/probe_flush.py` (self-contained, re-runnable,
uses only scratch collections -- never touches a live Tachi DB). Each
scenario runs in a **child process** that ends its own life with
`os.kill(os.getpid(), signal.SIGKILL)` as its literal last statement (no
`finally`, no atexit, no graceful shutdown chance), then the parent reopens
the collection read-only in a fresh process to check what survived.

Results (`tools/zvec-shadow/FINDINGS-flush.json`, this run):

| Scenario | Sequence | Expected | Observed | Pass |
|---|---|---|---|---|
| A | insert 50 -> `flush()` -> SIGKILL | all 50 survive | doc_count=50, all `d0..d49` present | yes |
| B | insert 50 -> SIGKILL (no flush) | 0 survive | doc_count=0 | yes |
| C | insert 50 -> `flush()` -> insert 50 more -> SIGKILL | exactly `d0..d49` survive, `d50..d99` absent | doc_count=50, exactly `d0..d49` | yes |

**Conclusion:** `flush()` is the one and only durability API zvec exposes
(there is no separate `commit()` -- `Collection.flush` is the whole API
surface for it, confirmed via `dir(zvec.Collection)`). Everything written
before the last successful `flush()` call survives a hard process kill;
everything written after it is entirely gone, with no partial/torn state
observed in three runs. This matches the tachi#683 spike's earlier
real-world observation (96k docs evaporating on an unflushed kill -9) and
sharpens it into a precise contract: **the sidecar/export pipeline must
call `flush()` after every load batch it wants durable, and must not assume
anything survives kill -9 without an explicit flush**. For this PR's
sidecar, that only matters at initial load time (`sidecar.py` calls
`col.flush()` once after loading the full snapshot); the sidecar takes no
further writes at query time, so there is no ongoing durability exposure
during normal operation.

**Scope limit (adjudication CP2 -- do not over-read this result):** the
probe characterizes a **process-crash-level** boundary only. SIGKILL
terminates the process but does not drop the OS page cache, so data
`flush()` handed to the kernel survives the kill even if it was never
fsync'd to stable storage. These results therefore say NOTHING about
power-loss / kernel-panic durability -- whether `flush()` fsyncs is
uncharacterized here and would need a different rig (e.g. VM with forced
power-off, or dm-flakey-style write-loss injection) to test. Since zvec is
a rebuildable derived index and SQLite remains the sole system of record,
power-loss durability is a non-goal for this architecture anyway; the
scope limit matters only if someone later tries to promote zvec to a
source of truth.

## Operational quirks found while building G3/G4 (worth carrying into Phase 1 planning)

1. **`tachi search` daemon-scope routing keys off the inherited `PWD` env
   var, not the process's actual working directory.** Running `tachi
   search` from inside this worktree (or via `subprocess.run(cwd=...)`
   without also overriding the `PWD` env var, or via `env -C <dir>` at the
   shell level) silently routes to an empty per-worktree "named project" DB
   instead of `~/.tachi/global/memory.db`, with **no error** -- it just
   returns zero/irrelevant hits, which looked at first like a real
   retrieval-quality problem (an initial compare.py run showed tachi
   hit@10=0/20 and overlap@10=0.00 across the board). Verified directly:
   ```
   $ cd <worktree>; env -C ~ tachi search "..." --top-k 10   # PWD still stale
   -> 0 rows, wrong (empty) project DB
   $ cd <worktree>; env -C ~ PWD=/Users/kckylechen tachi search "..." --top-k 10
   -> 10 rows, correct global DB
   ```
   `compare.py`'s `query_tachi()` now explicitly overrides `PWD` in the
   subprocess env (not just `cwd=`) to work around this. This is worth a
   real tachi bug report independent of #683 -- silent wrong-DB routing
   with no error surfaced is a footgun for any script or agent that shells
   out to `tachi search` from a worktree.
2. **`tachi search` occasionally returns a malformed response shape.** One
   invocation's `sections[].rows[]` array contained plain strings instead
   of the normal row objects, coinciding with a daemon stderr line about
   loop-detection / burst-limit fallback to in-process execution on an
   unrelated tool (`tachi_memory`), suggesting the daemon is a shared
   resource across concurrent callers on this machine and its response
   shape isn't fully stable under contention. `compare.py` now skips
   non-dict rows defensively (logged as `tachi_error`) instead of crashing.
3. Read-only Python access to `memories_vec` (a sqlite-vec `vec0` virtual
   table) requires loading the `sqlite-vec` extension on the connection
   first (`sqlite_vec.load(conn)`) -- a plain `sqlite3.connect(...,
   mode=ro)` cannot even prepare a statement referencing a virtual table
   whose module isn't registered, regardless of read-only-ness.
4. Real Tachi memory ids are not all zvec-safe doc ids: 6/345 memories in
   the live global DB use a `"handoff:<uuid>"` id shape, and zvec's `Doc`
   constructor rejects the literal `:` character (`ValueError: Invalid doc:
   ... contains invalid characters`). `sidecar.py` sanitizes to a zvec-safe
   id and carries the real Tachi id in a stored `tachi_id` field, which is
   what query results report back as `id`. (After the CP1/CP3 corpus
   alignment those specific handoff rows are no longer exported at all --
   they are namespace search noise -- but the sanitizer stays as defense
   for any future id shape that trips zvec's character rules.)

## Snapshot corpus alignment (adjudication CP1/CP3)

The first version of `export_snapshot.py` filtered only `archived = 0`.
Codex's adversarial review of PR #700 correctly flagged that this is NOT
equivalent to Tachi's default retrievable corpus: a default `tachi search`
also excludes superseded rows
(`crates/memcore/src/search.rs:74-92` SearchOptions::default
`include_superseded=false`, enforced in SQL at
`crates/memcore/src/db/memory_crud/search.rs:30-35`), training-seed /
recall-cache / eval rows
(`crates/tachi-server/src/memory_search_ops/search_memory/rows.rs:359-366`),
and -- one layer deeper, in core ranking
(`crates/memcore/src/search/ranking.rs:57` via
`crates/memcore/src/namespace.rs:88-99`) -- wiki_log/kanban/handoff
namespace-noise rows. The last group was not in the adjudication's cited
set but is required by its own labeled-set invariant ("expected hits must
all be Tachi-default-retrievable"): the live DB has 59 kanban + 6 handoff
non-archived rows a default search can never return.

`export_snapshot.py` now implements all of these exclusions as a
predicate-by-predicate Python translation of the Rust source (each helper
carries its file:line citation -- see `tachi_default_retrievable()`).
Effect on the live global DB at rework time: 345 non-archived rows ->
**257 exported** (excluded: 12 superseded, 11 eval, 65 namespace-noise;
this DB has no training-seed or recall-cache rows). Both the sidecar
corpus and the compare labeled set are drawn from the aligned snapshot,
so every expected id in the labeled set is now retrievable by a default
`tachi search`.

## G4 -- compare.py end-to-end run (aggregate numbers only)

Ran against a corpus-aligned snapshot of `~/.tachi/global/memory.db`
(257 default-retrievable memories at the time of this run) with 20 queries
derived from real memories' own summaries (expected hit = the originating
memory; all expected ids verified default-retrievable by construction).
Full per-query detail (including the query text and memory ids, which are
real private content) is intentionally **not** committed -- see
`tools/zvec-shadow/.gitignore` and the README's privacy note; regenerate
locally with `compare.py` to see the per-query table.

| Mode | tachi hit@10 | sidecar hit@10 | mean overlap@10 | mean tachi latency | mean sidecar latency |
|---|---|---|---|---|---|
| FTS-only (sidecar has no query embedding) | 8/20 | 20/20 | 2.00 / 10 | 1024.8 ms | 2.8 ms |
| Hybrid (sidecar reuses each memory's own stored embedding as query vector) | 8/20 | 20/20 | 2.70 / 10 | 651.1 ms | 3.5 ms |

Reading these numbers honestly:
- **tachi hit@10 stayed low after the corpus fix (7/20 pre-fix -> 8/20
  post-fix).** The superseded/eval/noise artifact the review flagged was
  real but small on this DB; the remaining misses are genuine signal about
  this (deliberately easy, summary-derived) query set: on 12 of 20
  queries, Tachi's default top-10 does not contain the memory whose own
  summary the query was built from, while the sidecar's FTS-over-
  summary+text does. Worth investigating in the Phase 1 recall_simulate
  pass rather than hand-waving here -- candidate causes include Tachi's
  decay/quality multipliers demoting older rows and hybrid-rank dilution
  from wiki rows, but neither was verified in this PR.
- **Latency is not apples-to-apples.** The "tachi" number is a full CLI
  subprocess round trip (process spawn + daemon IPC + the daemon's own
  embedding-API call for the query text); the "sidecar" number is a
  same-host HTTP round trip against an already-warm in-process zvec
  collection. The original spike's raw dense-query numbers (p50=0.26ms at
  2k docs) are the more honest zvec-vs-zvec comparison point; this table's
  latency gap mostly reflects CLI/process overhead, not a claim that zvec
  queries are literally ~300x faster than Tachi's whole search stack.
- **Low overlap@10 between tachi and sidecar is expected, not a red flag.**
  Tachi's ranking blends vector + FTS + symbolic + recency-decay; the FTS-only
  sidecar mode is a single-signal baseline by design (query-side embeddings
  are out of scope per the frozen "no paid API calls from the sidecar"
  constraint). The hybrid mode (dense+FTS on the sidecar) closes some of the
  gap, as expected.
- **sidecar hit@10 is inflated relative to a real production query
  workload**, because the labeled query set is deliberately easy (queries
  are literally derived from each target memory's own summary text) -- this
  is the "simple labeled set" the frozen spec asked for, not a claim of
  production-grade recall quality. A fair recall@k/MRR comparison needs the
  Phase 1 `recall_simulate` integration described in README.md.
